//! The three decode worker threads.
//!
//! Every entry point catches panics: a malformed file must never take the whole
//! player down, it should surface as an error state the UI can show and the user
//! can recover from by opening something else.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use ffmpeg_next as ffmpeg;

use super::queue::{AudioMsg, PacketMsg, VideoMsg};
use super::{EngineConfig, EngineEvent, MediaSource, PlaybackState, Shared};
use crate::audio::AudioSink;
use crate::dsp::{apply_gain, TimeStretcher};
use crate::info;
use crate::util::MediaKind;
use crate::video::{RgbaConverter, VideoFrame};
use mvp_subtitle::model::{Cue, SubFormat, Subtitle};

/// Video packets may queue this deep before the demuxer is throttled.
const VIDEO_PACKET_QUEUE: usize = 48;
/// Audio packets may queue this deep before the demuxer is throttled.
const AUDIO_PACKET_QUEUE: usize = 192;
/// How often the demuxer re-checks control state while blocked.
const CONTROL_POLL: Duration = Duration::from_millis(50);
/// How far ahead of the playhead the video decoder may run, in seconds.
///
/// This is the decoder's lead over the master clock, not a drop threshold:
/// the decoder simply waits once the queue is this far ahead. Half a second is
/// enough to absorb a slow repaint or a disk hiccup, and small enough that a
/// seek or a rate change takes effect at once.
const VIDEO_LEAD: f64 = 0.5;

/// Slack added to a resampler output frame, in device-rate frames.
///
/// Covers the resampler's own filter delay plus one source frame, so a rate
/// conversion never has to buffer audio it could have handed over right away.
const RESAMPLE_MARGIN: usize = 4096;

/// Entry point for the demuxer thread. Spawns the decoder threads.
pub(super) fn run_demuxer(shared: Arc<Shared>, source: MediaSource, config: EngineConfig) {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        demuxer_main(&shared, &source, &config)
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => report_error(&shared, err.to_string()),
        Err(panic) => {
            let message = panic_message(&panic);
            report_error(&shared, format!("解码线程异常: {message}"));
        }
    }
}

fn report_error(shared: &Arc<Shared>, message: String) {
    log::error!("{message}");
    shared.set_state(PlaybackState::Error(message.clone()));
    let _ = shared.event_tx.send(EngineEvent::Error(message));
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知错误".to_string()
    }
}

/// Build the AVFormatContext options dictionary.
// `ffmpeg::Dictionary` is an owned, lifetime-free type; the lint below cannot
// see through the re-export and asks for a lifetime parameter that does not
// exist.
#[allow(mismatched_lifetime_syntaxes)]
fn input_options(source: &MediaSource) -> ffmpeg::Dictionary {
    let mut options = ffmpeg::Dictionary::new();
    // Analysing less than FFmpeg's default keeps opening snappy, at the cost of
    // occasionally mis-detecting an exotic file. 5 s / 5 MB is what most players
    // settle on.
    options.set("analyzeduration", "5000000");
    options.set("probesize", "5000000");
    // Never let a dead network peer hang the demuxer.
    options.set("rw_timeout", "15000000");
    options.set("user_agent", "MVP-Versatile-Player/0.1");
    if let MediaSource::Url(url) = source {
        if url.starts_with("rtsp") {
            options.set("rtsp_transport", "tcp");
        }
        options.set("reconnect", "1");
        options.set("reconnect_streamed", "1");
        options.set("reconnect_on_network_error", "1");
        options.set("reconnect_delay_max", "4");
    }
    options
}

/// Owned copy of a stream's codec parameters, safe to move to another thread.
fn own_parameters<'a>(stream: &ffmpeg::Stream<'a>) -> ffmpeg::codec::Parameters {
    let mut params = ffmpeg::codec::Parameters::new();
    // SAFETY: `params` is a freshly allocated AVCodecParameters and the source
    // pointer belongs to a live stream of a live format context. The copy makes
    // the destination fully independent, including its extradata.
    unsafe {
        ffmpeg::ffi::avcodec_parameters_copy(params.as_mut_ptr(), stream.parameters().as_ptr());
    }
    params
}

/// `true` when the demuxer must give up what it is doing right now.
fn interrupted(shared: &Shared) -> bool {
    shared.abort.load(Ordering::Relaxed) || shared.seek_request.lock().is_some()
}

/// Send a **video** packet.
///
/// The demuxer is the only source of *both* streams, so blocking it forever on a
/// full video channel would also stall the audio the user is listening to. But
/// dropping a packet is not free either: it is a frame the user will never see.
///
/// The rule is therefore about *where the playhead is*, not about the queue:
///
/// * while the video waiting to be shown is still **ahead** of the clock there is
///   no emergency, so the demuxer waits for room. The audio channel and the
///   sound card hold seconds of buffer, so a short wait costs nothing;
/// * once the queue has fallen **behind** the clock, skipping is the correct
///   answer — those frames are already in the past and showing them would only
///   make the picture later still.
fn send_video(tx: &Sender<VideoMsg>, msg: VideoMsg, shared: &Shared) -> bool {
    let mut msg = msg;
    loop {
        match tx.send_timeout(msg, CONTROL_POLL) {
            Ok(()) => return true,
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return false,
            Err(crossbeam_channel::SendTimeoutError::Timeout(returned)) => {
                msg = returned;
                if interrupted(shared) {
                    return false;
                }
                if video_is_behind(shared) {
                    shared.dropped_frames.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            }
        }
    }
}

/// `true` when the video still waiting to be shown is already late.
///
/// A queue that is empty is never "behind": nothing has been decoded yet, so
/// dropping the packet that would produce the next frame can only hurt.
fn video_is_behind(shared: &Shared) -> bool {
    let now = shared.clock.now();
    let queue = shared.video_queue.lock();
    match queue.front_pts() {
        Some(front) => front < now - 0.25,
        None => false,
    }
}

/// Send on a bounded channel, polling for abort/seek so the demuxer can never
/// get permanently stuck behind a slow decoder.
fn send_control<T>(tx: &Sender<T>, mut msg: T, shared: &Shared) -> bool {
    loop {
        match tx.send_timeout(msg, CONTROL_POLL) {
            Ok(()) => return true,
            Err(crossbeam_channel::SendTimeoutError::Timeout(returned)) => {
                if interrupted(shared) {
                    return false;
                }
                msg = returned;
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return false,
        }
    }
}

fn demuxer_main(
    shared: &Arc<Shared>,
    source: &MediaSource,
    config: &EngineConfig,
) -> crate::error::Result<()> {
    let path_string = source.as_str();
    let kind = match source {
        MediaSource::Path(p) => crate::util::classify(p),
        MediaSource::Url(_) => MediaKind::Unknown,
    };

    let interrupt_shared = Arc::clone(shared);
    let mut ictx = ffmpeg::format::input_with_interrupt_and_dictionary(
        &path_string,
        move || {
            interrupt_shared.abort.load(Ordering::Relaxed)
                || interrupt_shared.seek_request.lock().is_some()
        },
        input_options(source),
    )?;

    if shared.abort.load(Ordering::Relaxed) {
        return Ok(());
    }

    // ---- describe the file -------------------------------------------------
    let probe_path = match source {
        MediaSource::Path(p) => p.clone(),
        MediaSource::Url(u) => std::path::PathBuf::from(u),
    };
    let media_info = Arc::new(info::describe(&ictx, &probe_path, kind));
    *shared.duration.lock() = media_info.duration.max(0.0);
    *shared.info.lock() = Some(Arc::clone(&media_info));
    let _ = shared.event_tx.send(EngineEvent::Opened(Arc::clone(&media_info)));

    // ---- pick streams ------------------------------------------------------
    let video_stream = media_info
        .video
        .first()
        .map(|v| v.index)
        .filter(|_| !media_info.is_still_image);
    let audio_stream = pick_stream(shared.audio_track.load(Ordering::Relaxed), &media_info.audio);
    let subtitle_stream = pick_subtitle(
        shared.subtitle_track.load(Ordering::Relaxed),
        &media_info.subtitles,
    );

    // ---- spawn the video decoder ------------------------------------------
    let (video_tx, video_rx) = bounded::<VideoMsg>(VIDEO_PACKET_QUEUE);
    let local_video_stream = video_stream;
    if let Some(index) = video_stream {
        let stream = ictx
            .streams()
            .find(|s| s.index() == index)
            .ok_or_else(|| crate::error::MediaError::MissingStream("视频".into()))?;
        let params = own_parameters(&stream);
        let time_base = super::rational_to_f64(stream.time_base());
        let start_offset = stream_start_offset(&stream, time_base);
        let video_config = config.clone();
        spawn_worker(shared, "mvp-video", move |shared| {
            run_video_decoder(
                shared,
                params,
                video_rx,
                time_base,
                start_offset,
                video_config,
            )
        });
    }

    // ---- audio device + decoder -------------------------------------------
    let (audio_tx, audio_rx) = bounded::<AudioMsg>(AUDIO_PACKET_QUEUE);
    let mut local_audio_stream = audio_stream;
    // The selector the demuxer has already acted on: -2 auto, -1 off, >= 0 an
    // explicit container index.
    let mut local_audio_request = shared.audio_track.load(Ordering::Relaxed);
    let mut sink: Option<Arc<AudioSink>> = None;
    if let (Some(index), true) = (audio_stream, config.audio_enabled) {
        match AudioSink::new(config.audio_device.as_deref(), config.volume, config.speed) {
            Ok(device) => {
                let device = Arc::new(device);
                shared.clock.set_audio(Some(Arc::clone(&device)));
                *shared.audio.lock() = Some(Arc::clone(&device));
                sink = Some(device);
            }
            Err(err) => {
                // A missing sound card must not stop the video from playing.
                log::warn!("音频设备不可用，继续静音播放: {err}");
                let _ = shared
                    .event_tx
                    .send(EngineEvent::Error(format!("音频输出不可用: {err}")));
            }
        }
        if let Some(stream) = ictx.streams().find(|s| s.index() == index) {
            let params = own_parameters(&stream);
            let time_base = super::rational_to_f64(stream.time_base());
            let start_offset = stream_start_offset(&stream, time_base);
            let audio_sink = sink.clone();
            let audio_config = config.clone();
            spawn_worker(shared, "mvp-audio", move |shared| {
                run_audio_decoder(
                    shared,
                    params,
                    audio_rx,
                    time_base,
                    start_offset,
                    audio_sink,
                    audio_config,
                )
            });
        }
    }

    // ---- embedded subtitle decoder ----------------------------------------
    let mut subtitle_decoder = match subtitle_stream {
        Some(index) if info::is_text_subtitle_codec(stream_codec_id(&ictx, index)) => {
            match open_subtitle_decoder(&ictx, index) {
                Ok(decoder) => Some((index, decoder)),
                Err(err) => {
                    log::warn!("无法打开内嵌字幕解码器: {err}");
                    None
                }
            }
        }
        Some(_) => {
            let _ = shared.event_tx.send(EngineEvent::Error(
                "该字幕为图形字幕（如 PGS/DVD），暂不支持显示".into(),
            ));
            None
        }
        None => None,
    };
    let mut local_subtitle_request = shared.subtitle_track.load(Ordering::Relaxed);
    let mut embedded_cues: Vec<Cue> = Vec::new();
    let mut last_published = 0usize;

    // ---- main demux loop ---------------------------------------------------
    if config.autoplay {
        shared.clock.set_running(true);
        shared.set_state(PlaybackState::Playing);
    } else {
        shared.set_state(PlaybackState::Paused);
    }

    let mut generation = shared.generation.load(Ordering::SeqCst);
    let mut reached_eof = false;

    loop {
        if shared.abort.load(Ordering::Relaxed) {
            break;
        }

        // -- seek ------------------------------------------------------------
        if let Some(target) = shared.seek_request.lock().take() {
            generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
            shared.video_queue.lock().clear();
            shared.clock.seek(target);
            *shared.seek_target.lock() = Some(target);
            shared.dropped_frames.store(0, Ordering::Relaxed);
            // Hand the request over through shared state: the packet channels
            // may hold a second's worth of stale data, and waiting for a slot in
            // them would stall the seek behind it.
            shared.request_flush(generation, target);
            seek_container(&mut ictx, target)?;
            reached_eof = false;
            if shared.stepping.load(Ordering::Relaxed) > 0 {
                shared.set_state(PlaybackState::Paused);
            }
            continue;
        }

        // -- A–B loop --------------------------------------------------------
        if let Some((a, b)) = *shared.ab_loop.lock() {
            let now = shared.clock.now();
            if b > a && now >= b {
                *shared.seek_request.lock() = Some(a);
                continue;
            }
        }

        // -- track changes ---------------------------------------------------
        //
        // Both checks compare against the *requested* selection they last acted
        // on, never against the stream that happened to be chosen at open time.
        // Comparing against the open-time choice is a subtle trap: "auto" (-2)
        // resolves to a concrete stream index, so the two can never be equal
        // again and the demuxer would spin here forever without reading a single
        // packet — the video would simply never appear.
        let wanted_audio = shared.audio_track.load(Ordering::Relaxed);
        if wanted_audio != local_audio_request {
            local_audio_request = wanted_audio;
            let new_index = pick_stream(wanted_audio, &media_info.audio);
            local_audio_stream = new_index;
            let target = shared.clock.now();
            generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(index) = new_index {
                if let Some(stream) = ictx.streams().find(|s| s.index() == index) {
                    *shared.audio_stream.lock() = Some(Box::new(own_parameters(&stream)));
                }
            }
            shared.request_flush(generation, target);
            continue;
        }

        let wanted_sub = shared.subtitle_track.load(Ordering::Relaxed);
        if wanted_sub != local_subtitle_request {
            local_subtitle_request = wanted_sub;
            let new_index = pick_subtitle(wanted_sub, &media_info.subtitles);
            subtitle_decoder = new_index.and_then(|index| {
                if info::is_text_subtitle_codec(stream_codec_id(&ictx, index)) {
                    open_subtitle_decoder(&ictx, index)
                        .map_err(|e| log::warn!("字幕解码器打开失败: {e}"))
                        .ok()
                        .map(|d| (index, d))
                } else {
                    None
                }
            });
            embedded_cues.clear();
            last_published = 0;
            publish_subtitles(shared, &embedded_cues, &mut last_published, true);
            continue;
        }

        // -- read a packet ---------------------------------------------------
        if reached_eof {
            if handle_eof(shared, &video_tx, &audio_tx, &embedded_cues, &mut last_published)? {
                break;
            }
            continue;
        }

        let packet = {
            let mut iter = ictx.packets();
            iter.next()
        };
        let Some((stream, packet)) = packet else {
            reached_eof = true;
            let _ = video_tx.send(VideoMsg::Eof);
            let _ = audio_tx.send(AudioMsg::Eof);
            continue;
        };

        let index = stream.index();
        let time_base = super::rational_to_f64(stream.time_base());
        let start_offset = stream_start_offset(&stream, time_base);

        if Some(index) == local_video_stream {
            let timestamp = packet.pts().map(|pts| pts as f64 * time_base - start_offset);
            let msg = VideoMsg::Packet(PacketMsg {
                packet,
                generation,
                timestamp,
            });
            if !send_video(&video_tx, msg, shared) {
                continue;
            }
        } else if Some(index) == local_audio_stream {
            if sink.is_some() {
                let timestamp = packet.pts().map(|pts| pts as f64 * time_base - start_offset);
                let msg = AudioMsg::Packet(PacketMsg {
                    packet,
                    generation,
                    timestamp,
                });
                if !send_control(&audio_tx, msg, shared) {
                    continue;
                }
            }
        } else if let Some((sub_index, decoder)) = subtitle_decoder.as_mut() {
            if index == *sub_index {
                if let Some(cue) = decode_subtitle_packet(decoder, &packet, time_base, start_offset)
                {
                    embedded_cues.push(cue);
                    publish_subtitles(shared, &embedded_cues, &mut last_published, false);
                }
            }
        }
    }

    // Shut the workers down.
    let _ = video_tx.send(VideoMsg::Stop);
    let _ = audio_tx.send(AudioMsg::Stop);
    Ok(())
}

/// Wait until everything that was decoded has been played, then report the end.
///
/// Returns `Ok(true)` when the demuxer should stop entirely.
fn handle_eof(
    shared: &Arc<Shared>,
    video_tx: &Sender<VideoMsg>,
    audio_tx: &Sender<AudioMsg>,
    embedded_cues: &[Cue],
    last_published: &mut usize,
) -> crate::error::Result<bool> {
    if shared.abort.load(Ordering::Relaxed) {
        return Ok(true);
    }
    if shared.looping.load(Ordering::Relaxed) {
        *shared.seek_request.lock() = Some(0.0);
        return Ok(false);
    }

    publish_subtitles(shared, embedded_cues, last_published, true);

    let duration = *shared.duration.lock();
    let now = shared.clock.now();
    let video_idle = shared.video_queue.lock().is_empty();
    let pending_frames = shared
        .info
        .lock()
        .as_ref()
        .map(|i| i.video.first().map(|v| v.frames).unwrap_or(0))
        .unwrap_or(0);
    let audio_idle = shared
        .audio
        .lock()
        .as_ref()
        .map(|a| a.queued_chunks() == 0)
        .unwrap_or(true);

    let finished = if duration > 0.0 {
        now >= duration - 0.10 || (video_idle && audio_idle && now >= duration - 1.0)
    } else {
        // Live streams have no duration; only stop when the stream really ended.
        video_idle && audio_idle && pending_frames == 0
    };

    if finished {
        shared.clock.set_running(false);
        if let Some(sink) = shared.audio.lock().as_ref() {
            sink.set_paused(true);
        }
        shared.set_state(PlaybackState::Ended);
        let _ = shared.event_tx.send(EngineEvent::Ended);
        let _ = video_tx.send(VideoMsg::Stop);
        let _ = audio_tx.send(AudioMsg::Stop);
        return Ok(true);
    }

    std::thread::sleep(Duration::from_millis(20));
    Ok(false)
}

/// Map the requested track selector onto a real stream index.
///
/// `-2` means "auto" (first stream), `-1` means "off", anything else is a
/// container index that must exist.
fn pick_stream(selector: i64, streams: &[info::AudioStreamInfo]) -> Option<usize> {
    match selector {
        -1 => None,
        -2 => streams.first().map(|s| s.index),
        index if index >= 0 => streams
            .iter()
            .find(|s| s.index as i64 == index)
            .map(|s| s.index),
        _ => streams.first().map(|s| s.index),
    }
}

fn pick_subtitle(selector: i64, streams: &[info::SubtitleStreamInfo]) -> Option<usize> {
    match selector {
        -1 => None,
        -2 => streams.iter().find(|s| s.is_default).or(streams.first()).map(|s| s.index),
        index if index >= 0 => streams
            .iter()
            .find(|s| s.index as i64 == index)
            .map(|s| s.index),
        _ => None,
    }
}

fn stream_codec_id(ictx: &ffmpeg::format::context::Input, index: usize) -> ffmpeg::codec::Id {
    ictx.streams()
        .find(|s| s.index() == index)
        .map(|s| s.parameters().id())
        .unwrap_or(ffmpeg::codec::Id::None)
}

/// Seconds to subtract from raw timestamps so playback starts at zero.
fn stream_start_offset<'a>(stream: &ffmpeg::Stream<'a>, time_base: f64) -> f64 {
    let start = stream.start_time();
    if start > 0 && start != i64::MIN {
        start as f64 * time_base
    } else {
        0.0
    }
}

/// Seek by time in seconds, letting FFmpeg snap back to the previous keyframe.
fn seek_container(
    ictx: &mut ffmpeg::format::context::Input,
    seconds: f64,
) -> crate::error::Result<()> {
    let timestamp = (seconds.max(0.0) * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
    ictx.seek(timestamp, ..timestamp)?;
    // SAFETY: `ictx` owns a live AVFormatContext; avformat_flush only discards
    // the demuxer's internal read-ahead buffer.
    unsafe { ffmpeg::ffi::avformat_flush(ictx.as_mut_ptr()) };
    Ok(())
}

fn spawn_worker<F>(shared: &Arc<Shared>, name: &str, body: F)
where
    F: FnOnce(Arc<Shared>) + Send + 'static,
{
    let thread_name = name.to_string();
    let error_name = name.to_string();
    let inner = Arc::clone(shared);
    let handle = std::thread::Builder::new()
        .name(thread_name)
        .stack_size(1024 * 1024)
        .spawn(move || {
            let shared_for_panic = Arc::clone(&inner);
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| body(inner)));
            if let Err(panic) = result {
                let message = panic_message(&panic);
                report_error(&shared_for_panic, format!("{error_name} 线程异常: {message}"));
            }
        });
    if let Ok(handle) = handle {
        shared.workers.lock().push(handle);
    }
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

fn run_video_decoder(
    shared: Arc<Shared>,
    params: ffmpeg::codec::Parameters,
    rx: Receiver<VideoMsg>,
    time_base: f64,
    start_offset: f64,
    config: EngineConfig,
) {
    let hardware = shared.hw_decoding.load(std::sync::atomic::Ordering::Relaxed);
    let (mut decoder, hardware_format) = match open_video_decoder(params, hardware) {
        Ok(opened) => opened,
        Err(err) => {
            report_error(&shared, format!("无法打开视频解码器: {err}"));
            return;
        }
    };

    let mut converter = RgbaConverter::new();
    let mut frame = ffmpeg::frame::Video::empty();
    let mut hw_scratch = ffmpeg::frame::Video::empty();
    let mut generation = shared.generation.load(Ordering::SeqCst);
    let mut seek_target: Option<f64> = None;
    let mut last_pts = 0.0f64;
    let mut serial = 0u64;
    let nominal_fps = shared
        .info
        .lock()
        .as_ref()
        .and_then(|i| i.video.first().map(|v| v.fps))
        .filter(|f| *f > 0.1 && f.is_finite())
        .unwrap_or(25.0);
    let fallback_step = 1.0 / nominal_fps;
    let _ = config;

    loop {
        // A pending flush is honoured before anything else, including packets
        // that are already in the channel: those belong to the previous decode
        // session and would otherwise be decoded and throttled against a clock
        // that has already moved.
        if let Some(request) = shared.take_video_flush() {
            generation = request.generation;
            seek_target = Some(request.target);
            last_pts = request.target;
            decoder.flush();
            shared.video_queue.lock().clear();
        }

        let msg = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(msg) => msg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if shared.abort.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        match msg {
            VideoMsg::Stop => break,
            VideoMsg::Eof => {
                let _ = decoder.send_eof();
                drain_video(
                    &shared,
                    &mut decoder,
                    &mut converter,
                    &mut frame,
                    &mut hw_scratch,
                    &mut last_pts,
                    &mut serial,
                    fallback_step,
                    generation,
                    &mut seek_target,
                    hardware_format,
                    time_base,
                    start_offset,
                );
            }
            VideoMsg::Packet(msg) => {
                if msg.generation != generation {
                    continue;
                }
                if decoder.send_packet(&msg.packet).is_err() {
                    // A corrupted packet is normal in the wild; keep going.
                    continue;
                }
                drain_video(
                    &shared,
                    &mut decoder,
                    &mut converter,
                    &mut frame,
                    &mut hw_scratch,
                    &mut last_pts,
                    &mut serial,
                    fallback_step,
                    generation,
                    &mut seek_target,
                    hardware_format,
                    time_base,
                    start_offset,
                );
            }
        }
    }
    shared.pool.clear();
}

#[allow(clippy::too_many_arguments)]
fn drain_video(
    shared: &Arc<Shared>,
    decoder: &mut ffmpeg::decoder::Video,
    converter: &mut RgbaConverter,
    frame: &mut ffmpeg::frame::Video,
    hw_scratch: &mut ffmpeg::frame::Video,
    last_pts: &mut f64,
    serial: &mut u64,
    fallback_step: f64,
    generation: u64,
    seek_target: &mut Option<f64>,
    hardware_format: Option<ffmpeg::ffi::AVPixelFormat>,
    time_base: f64,
    start_offset: f64,
) {
    while decoder.receive_frame(frame).is_ok() {
        // A pending seek invalidates everything still in the pipeline; bail out
        // so the caller can act on it instead of finishing this batch.
        if shared.video_flush_pending() {
            return;
        }

        // A hardware decoder hands back a frame that lives in GPU memory; the
        // colour conversion needs it in RAM.
        let source: &ffmpeg::frame::Video = if is_hardware_frame(frame, hardware_format) {
            match download_frame(frame, hw_scratch) {
                Ok(()) => hw_scratch,
                Err(err) => {
                    log::warn!("{err}");
                    continue;
                }
            }
        } else {
            frame
        };

        let pts = match source.pts() {
            Some(raw) => raw as f64 * time_base - start_offset,
            None => *last_pts + fallback_step,
        };
        let pts = if pts.is_finite() { pts } else { *last_pts + fallback_step };
        *last_pts = pts;

        let now = shared.clock.now();
        let paused = !shared.state().is_playing();

        // Frames from before the seek target belong to the keyframe we landed
        // on, not to the position the user asked for.
        if let Some(target) = *seek_target {
            if pts + 0.02 < target {
                continue;
            }
        }

        // If the decoder has fallen behind, drop rather than fall further
        // behind — but never while paused, where every frame is precious.
        if !paused && pts < now - 0.25 && !shared.video_queue.lock().is_empty() {
            shared.dropped_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let target = *shared.target_size.lock();
        let (width, height) = match target {
            Some((tw, th)) => crate::util::fit_inside(
                (source.width(), source.height()),
                crate::util::even((tw, th)),
            ),
            None => (source.width(), source.height()),
        };

        let data = match converter.convert(source, width, height, &shared.pool) {
            Ok(data) => data,
            Err(err) => {
                log::warn!("视频帧转换失败: {err}");
                continue;
            }
        };

        *serial = serial.wrapping_add(1);
        let decoded = VideoFrame {
            width,
            height,
            pts,
            duration: fallback_step,
            data,
            serial: *serial,
            generation,
        };
        shared.decoded_frames.fetch_add(1, Ordering::Relaxed);

        // ---- presentation-lead gate ----------------------------------------
        // The queue is a presentation buffer: the frame the interface needs
        // next is always its *oldest* one, so a full queue has to make the
        // decoder wait, never drop. The decoder also stops running away from
        // the playhead: it may stay at most `VIDEO_LEAD` ahead, which keeps
        // seeks and rate changes responsive without ever making the picture
        // wait for a frame that has been thrown away.
        //
        // The wait is short and re-checks the abort flag and pending seek each
        // time, so a seek is never delayed by more than a few milliseconds.
        let decoded = Arc::new(decoded);
        loop {
            if shared.video_flush_pending() || shared.abort.load(Ordering::Relaxed) {
                return;
            }
            {
                let mut queue = shared.video_queue.lock();
                // While paused the clock stands still, so only the queue's own
                // capacity bounds the decoder — that is what lets frame
                // stepping walk forward.
                let playing = shared.state().is_playing();
                let lead_ok = !playing
                    || queue.is_empty()
                    || queue
                        .back_pts()
                        .map(|back| back - shared.clock.now() < VIDEO_LEAD)
                        .unwrap_or(true);
                if lead_ok && queue.try_push(Arc::clone(&decoded)).is_none() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

/// Hardware acceleration backends to try, best first.
///
/// D3D11VA is the modern Windows path and is what every current GPU driver
/// exposes; DXVA2 is the older one and catches machines where D3D11 video was
/// never implemented.
const HW_DEVICE_TYPES: &[ffmpeg::ffi::AVHWDeviceType] = &[
    ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
    ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2,
];

thread_local! {
    /// Pixel format the decoder's `get_format` callback should pick.
    ///
    /// `get_format` is a C callback with no user pointer, so the choice has to
    /// live somewhere the callback can read. Thread-local is exactly right: a
    /// decoder runs entirely on one thread.
    static HW_TARGET_FORMAT: std::cell::Cell<ffmpeg::ffi::AVPixelFormat> =
        const { std::cell::Cell::new(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE) };
}

/// Called by `libavcodec` to let the application pick the pixel format.
///
/// Returning the hardware format is what actually switches the decoder to the
/// GPU; without this callback a decoder ignores `hw_device_ctx` entirely.
unsafe extern "C" fn select_hw_format(
    _ctx: *mut ffmpeg::ffi::AVCodecContext,
    formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    use ffmpeg::ffi::AVPixelFormat;
    if formats.is_null() {
        return AVPixelFormat::AV_PIX_FMT_NONE;
    }
    let target = HW_TARGET_FORMAT.with(std::cell::Cell::get);
    // SAFETY: `formats` is a NUL(-1)-terminated array owned by libavcodec and
    // valid for the duration of the call.
    unsafe {
        let mut cursor = formats;
        while *cursor != AVPixelFormat::AV_PIX_FMT_NONE {
            if *cursor == target {
                return target;
            }
            cursor = cursor.add(1);
        }
        // The decoder does not offer what we asked for: take its first choice,
        // which is always a software format.
        *formats
    }
}

/// Attach a hardware device to an unopened decoder context.
///
/// Returns the pixel format the decoder will produce on success; `None` means
/// this machine (or this codec) cannot do hardware decoding and the caller must
/// stay on the software path.
fn enable_hardware(
    ctx: *mut ffmpeg::ffi::AVCodecContext,
    codec: *const ffmpeg::ffi::AVCodec,
) -> Option<ffmpeg::ffi::AVPixelFormat> {
    use ffmpeg::ffi;
    if ctx.is_null() || codec.is_null() {
        return None;
    }

    // SAFETY: both pointers are live and owned by the caller; every call below
    // is a documented read or a documented transfer of ownership, and every
    // allocated buffer is released before returning.
    unsafe {
        for device_type in HW_DEVICE_TYPES {
            let mut target = ffi::AVPixelFormat::AV_PIX_FMT_NONE;
            let mut index = 0;
            loop {
                let config = ffi::avcodec_get_hw_config(codec, index);
                if config.is_null() {
                    break;
                }
                let config = &*config;
                if config.device_type == *device_type
                    && (config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
                {
                    target = config.pix_fmt;
                    break;
                }
                index += 1;
            }
            if target == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                continue;
            }

            let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
            if ffi::av_hwdevice_ctx_create(
                &mut device,
                *device_type,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            ) < 0
                || device.is_null()
            {
                continue;
            }

            HW_TARGET_FORMAT.with(|cell| cell.set(target));
            (*ctx).get_format = Some(select_hw_format);
            // `hw_device_ctx` takes its own reference; drop ours straight away.
            (*ctx).hw_device_ctx = ffi::av_buffer_ref(device);
            ffi::av_buffer_unref(&mut device);

            if (*ctx).hw_device_ctx.is_null() {
                continue;
            }
            log::info!("已启用硬件解码: {:?}", device_type);
            return Some(target);
        }
    }
    None
}

/// `true` when a decoded frame lives in GPU memory and must be downloaded.
fn is_hardware_frame(
    frame: &ffmpeg::frame::Video,
    hardware_format: Option<ffmpeg::ffi::AVPixelFormat>,
) -> bool {
    let Some(target) = hardware_format else {
        return false;
    };
    let format: ffmpeg::ffi::AVPixelFormat = frame.format().into();
    format == target
}

/// Copy a hardware frame back into system memory.
///
/// The player needs the pixels in RAM because it converts them to RGBA and
/// uploads a texture; the expensive part that hardware decoding removes is the
/// bitstream decode, not this transfer.
fn download_frame(
    frame: &ffmpeg::frame::Video,
    scratch: &mut ffmpeg::frame::Video,
) -> crate::error::Result<()> {
    // SAFETY: a fresh empty frame is a valid destination; libavcodec allocates
    // the buffer using the hardware frames context's software format.
    unsafe {
        let result = ffmpeg::ffi::av_hwframe_transfer_data(
            scratch.as_mut_ptr(),
            frame.as_ptr(),
            0,
        );
        if result < 0 {
            return Err(crate::error::MediaError::other(format!(
                "无法从显卡取回画面 (错误码 {result})"
            )));
        }
    }
    Ok(())
}

fn open_video_decoder(
    params: ffmpeg::codec::Parameters,
    hardware: bool,
) -> crate::error::Result<(ffmpeg::decoder::Video, Option<ffmpeg::ffi::AVPixelFormat>)> {
    let codec = find_decoder(&params)?;

    // Keep an untouched copy: `Context::from_parameters` consumes the original,
    // and the hardware attempt may fail after that point.
    let mut backup = ffmpeg::codec::Parameters::new();
    // SAFETY: `backup` is freshly allocated and `params` owns a live
    // AVCodecParameters; the copy makes the two fully independent.
    unsafe {
        ffmpeg::ffi::avcodec_parameters_copy(backup.as_mut_ptr(), params.as_ptr());
    }

    let context = ffmpeg::codec::context::Context::from_parameters(params)?;
    // `Decoder::open` passes a null codec to `avcodec_open2`, which fails with
    // EINVAL; the codec has to be resolved from the stream's codec id.
    let mut decoder = context.decoder();

    if hardware {
        // SAFETY: `decoder` has not been opened yet, which is the only point at
        // which these fields may be written.
        let enabled = unsafe { enable_hardware(decoder.as_mut_ptr(), codec.as_ptr()) };
        if let Some(format) = enabled {
            match decoder.open_as(codec) {
                Ok(opened) => {
                    log::info!("硬件解码器已打开");
                    return Ok((opened.video()?, Some(format)));
                }
                Err(err) => {
                    log::warn!("硬件解码器打开失败，回退到软件解码: {err}");
                    // Rebuild from the untouched parameters and open in software.
                    let context = ffmpeg::codec::context::Context::from_parameters(backup)?;
                    return Ok((context.decoder().open_as(codec)?.video()?, None));
                }
            }
        }
        log::info!("此文件或显卡不支持硬件解码，使用软件解码");
    }

    Ok((decoder.open_as(codec)?.video()?, None))
}

/// Resolve the FFmpeg decoder for a stream's codec id.
fn find_decoder(
    params: &ffmpeg::codec::Parameters,
) -> crate::error::Result<ffmpeg::Codec> {
    let id = params.id();
    ffmpeg::codec::decoder::find(id).ok_or_else(|| {
        crate::error::MediaError::unsupported(format!(
            "系统中没有 {} 的解码器",
            info::codec_name(id)
        ))
    })
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_audio_decoder(
    shared: Arc<Shared>,
    params: ffmpeg::codec::Parameters,
    rx: Receiver<AudioMsg>,
    time_base: f64,
    start_offset: f64,
    sink: Option<Arc<AudioSink>>,
    _config: EngineConfig,
) {
    let Some(sink) = sink else {
        // No output device: nothing to do, and burning CPU decoding silence
        // would only slow the video down.
        return;
    };

    let mut decoder = match open_audio_decoder(params) {
        Ok(decoder) => decoder,
        Err(err) => {
            report_error(&shared, format!("无法打开音频解码器: {err}"));
            return;
        }
    };

    let device_rate = sink.sample_rate();
    let device_channels = sink.channels().max(1);
    let device_layout = ffmpeg::ChannelLayout::default(device_channels as i32);
    let mut resampler: Option<ffmpeg::software::resampling::Context> = None;
    let mut stretcher = TimeStretcher::new(device_channels as usize, device_rate);
    stretcher.set_speed(shared.speed());

    let mut frame = ffmpeg::frame::Audio::empty();
    let mut generation = shared.generation.load(Ordering::SeqCst);
    let mut last_pts = 0.0f64;
    let mut scratch: Vec<f32> = Vec::new();

    loop {
        // Track changes and seeks arrive through shared state, never through the
        // packet channel, so they take effect immediately even with a full queue.
        if let Some(params) = shared.take_audio_stream() {
            match open_audio_decoder(*params) {
                Ok(new_decoder) => {
                    decoder = new_decoder;
                    resampler = None;
                    stretcher.reset();
                    let target = shared.clock.now();
                    sink.flush(target);
                    shared.audio_ended.store(false, Ordering::Relaxed);
                }
                Err(err) => log::warn!("切换音轨失败: {err}"),
            }
        }
        if let Some(request) = shared.take_audio_flush() {
            generation = request.generation;
            last_pts = request.target;
            decoder.flush();
            resampler = None;
            stretcher.reset();
            stretcher.set_speed(shared.speed());
            sink.flush(request.target);
            shared.audio_ended.store(false, Ordering::Relaxed);
        }

        let msg = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(msg) => msg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if shared.abort.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        match msg {
            AudioMsg::Stop => break,
            AudioMsg::Eof => {
                let _ = decoder.send_eof();
                drain_audio(
                    &shared,
                    &sink,
                    &mut decoder,
                    &mut resampler,
                    &mut stretcher,
                    &mut frame,
                    &mut last_pts,
                    &mut scratch,
                    device_layout,
                    device_rate,
                    time_base,
                    start_offset,
                    true,
                );
                shared.audio_ended.store(true, Ordering::Relaxed);
            }
            AudioMsg::Packet(msg) => {
                if msg.generation != generation {
                    continue;
                }
                if decoder.send_packet(&msg.packet).is_err() {
                    continue;
                }
                drain_audio(
                    &shared,
                    &sink,
                    &mut decoder,
                    &mut resampler,
                    &mut stretcher,
                    &mut frame,
                    &mut last_pts,
                    &mut scratch,
                    device_layout,
                    device_rate,
                    time_base,
                    start_offset,
                    false,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drain_audio(
    shared: &Arc<Shared>,
    sink: &Arc<AudioSink>,
    decoder: &mut ffmpeg::decoder::Audio,
    resampler: &mut Option<ffmpeg::software::resampling::Context>,
    stretcher: &mut TimeStretcher,
    frame: &mut ffmpeg::frame::Audio,
    last_pts: &mut f64,
    scratch: &mut Vec<f32>,
    device_layout: ffmpeg::ChannelLayout,
    device_rate: u32,
    time_base: f64,
    start_offset: f64,
    flushing: bool,
) {
    // Keep the stretcher in step with the UI slider.
    if (stretcher.speed() - shared.speed()).abs() > 1e-6 {
        stretcher.set_speed(shared.speed());
    }

    loop {
        let got = if flushing {
            decoder.receive_frame(frame).is_ok()
        } else {
            match decoder.receive_frame(frame) {
                Ok(()) => true,
                Err(_) => false,
            }
        };
        if !got {
            break;
        }

        let pts = match frame.pts() {
            Some(raw) => raw as f64 * time_base - start_offset,
            None => *last_pts,
        };
        let pts = if pts.is_finite() { pts } else { *last_pts };
        *last_pts = pts;

        let input_samples = frame.samples();
        if input_samples == 0 {
            continue;
        }

        // (Re)build the resampler whenever the source format changes, which
        // happens on the first frame and after an audio track switch.
        let needs_resampler = match resampler {
            Some(ctx) => {
                ctx.input().format != frame.format()
                    || ctx.input().rate != frame.rate()
                    || ctx.input().channel_layout.channels() != frame.channel_layout().channels()
            }
            None => true,
        };
        if needs_resampler {
            match ffmpeg::software::resampling::Context::get(
                frame.format(),
                frame.channel_layout(),
                frame.rate(),
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                device_layout,
                device_rate,
            ) {
                Ok(ctx) => *resampler = Some(ctx),
                Err(err) => {
                    log::warn!("无法创建重采样上下文: {err}");
                    continue;
                }
            }
        }
        let Some(ctx) = resampler.as_mut() else {
            continue;
        };

        // Size the output for the *rate ratio* before converting.
        //
        // `ffmpeg-next`'s `Context::run` allocates the output frame with the
        // input's sample count (`output.alloc(fmt, input.samples(), layout)`)
        // and `swr_convert_frame` then fills at most that many samples, holding
        // the rest of the converted audio inside the resampler. Whenever the
        // source rate differs from the device rate the result is truncated:
        // 44.1 kHz into a 192 kHz device produced 1024 frames of output for
        // 1024 frames of input — 5 ms of audio per 23 ms of media — and the
        // device underran itself into near-silence. Allocating the frame for
        // `in * dst_rate / src_rate` (plus the resampler's filter delay) is
        // what makes the device rate irrelevant.
        let mut output = ffmpeg::frame::Audio::empty();
        let in_rate = frame.rate().max(1) as u64;
        let capacity = ((input_samples as u64 * device_rate as u64) / in_rate) as usize
            + RESAMPLE_MARGIN;
        // SAFETY: `alloc` only describes a buffer inside a frame we own locally
        // and have not shared with FFmpeg yet; the call is `unsafe` because the
        // C API may abort on allocation failure, which is the same contract as
        // `Frame::empty` plus a decode.
        unsafe {
            output.alloc(
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                capacity,
                device_layout,
            );
        }
        if let Err(err) = ctx.run(frame, &mut output) {
            log::warn!("音频重采样失败: {err}");
            continue;
        }
        let samples = output.samples();
        if samples == 0 {
            continue;
        }
        let channels = output.channels().max(1) as usize;
        let expected = samples * channels;
        let data = output.plane::<f32>(0);
        if data.len() < expected {
            continue;
        }

        scratch.clear();
        scratch.extend_from_slice(&data[..expected]);
        apply_gain(scratch, sink.effective_gain());

        let mut stretched: Vec<f32> = Vec::with_capacity(expected + 4096);
        stretcher.push(scratch, &mut stretched);
        if !stretched.is_empty() {
            sink.push(stretched, pts);
        }
    }

    if flushing {
        let mut tail: Vec<f32> = Vec::new();
        stretcher.flush(&mut tail);
        if !tail.is_empty() {
            sink.push(tail, *last_pts);
        }
    }
}

fn open_audio_decoder(
    params: ffmpeg::codec::Parameters,
) -> crate::error::Result<ffmpeg::decoder::Audio> {
    let codec = find_decoder(&params)?;
    let context = ffmpeg::codec::context::Context::from_parameters(params)?;
    let decoder = context.decoder().open_as(codec)?.audio()?;
    Ok(decoder)
}

// ---------------------------------------------------------------------------
// Subtitles
// ---------------------------------------------------------------------------

type SubtitleDecoder = ffmpeg::decoder::Subtitle;

fn open_subtitle_decoder(
    ictx: &ffmpeg::format::context::Input,
    index: usize,
) -> crate::error::Result<SubtitleDecoder> {
    let stream = ictx
        .streams()
        .find(|s| s.index() == index)
        .ok_or_else(|| crate::error::MediaError::MissingStream("字幕".into()))?;
    let params = own_parameters(&stream);
    let codec = find_decoder(&params)?;
    let context = ffmpeg::codec::context::Context::from_parameters(params)?;
    Ok(context.decoder().open_as(codec)?.subtitle()?)
}

fn decode_subtitle_packet(
    decoder: &mut SubtitleDecoder,
    packet: &ffmpeg::Packet,
    time_base: f64,
    start_offset: f64,
) -> Option<Cue> {
    let mut subtitle = ffmpeg::Subtitle::new();
    match decoder.decode(packet, &mut subtitle) {
        Ok(true) => {}
        _ => return None,
    }

    let base = match subtitle.pts().or_else(|| packet.pts()) {
        Some(pts) => pts as f64 * time_base - start_offset,
        None => return None,
    };
    let start = base + subtitle.start() as f64 / 1000.0;
    let end = base + subtitle.end() as f64 / 1000.0;

    let mut text = String::new();
    for rect in subtitle.rects() {
        let piece: String = match rect {
            ffmpeg::subtitle::Rect::Text(t) => t.get().to_string(),
            ffmpeg::subtitle::Rect::Ass(a) => a.get().to_string(),
            _ => continue,
        };
        if piece.trim().is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&piece);
    }
    if text.trim().is_empty() {
        return None;
    }

    Some(Cue {
        start,
        end: if end > start { end } else { start + 2.0 },
        text: strip_ass_markup(&text),
        style: Default::default(),
    })
}

/// Reduce an ASS/SSA event text to plain text for our own renderer.
///
/// Embedded ASS subtitles arrive with their full override language attached
/// (`{\an8\b1}`), and muxed SRT occasionally carries the same blocks. The
/// player draws subtitles itself, so everything between braces is dropped and
/// the two ASS line-break escapes become real newlines.
fn strip_ass_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_block = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => in_block = true,
            '}' => in_block = false,
            '\\' if !in_block => match chars.next() {
                Some('N') | Some('n') => out.push('\n'),
                Some('h') => out.push(' '),
                Some(other) => out.push(other),
                None => {}
            },
            _ if !in_block => out.push(c),
            _ => {}
        }
    }
    out
}

/// Re-publish accumulated embedded cues.
///
/// A release build has no console, so every subtitle batch would otherwise be
/// invisible; but rebuilding the whole cue list for every single packet of a
/// two-hour film would be wasteful. The rule is therefore: publish immediately
/// while the list is still short (so subtitles appear the instant playback
/// starts), then batch.
fn publish_subtitles(
    shared: &Arc<Shared>,
    cues: &[Cue],
    last_published: &mut usize,
    force: bool,
) {
    /// Above this many cues, publishing is batched.
    const QUICK_LIMIT: usize = 64;
    /// Batch size once the list is long.
    const BATCH: usize = 24;

    // An external side-car always wins over what is muxed into the container.
    if shared.external_subtitle.lock().is_some() {
        return;
    }
    // With nothing to show there is nothing to publish; switching a track off
    // clears the cue list through its own path.
    if cues.is_empty() {
        return;
    }
    if !force && cues.len() >= QUICK_LIMIT && cues.len() < last_published.saturating_add(BATCH)
    {
        return;
    }
    if cues.len() == *last_published && !force {
        return;
    }
    *last_published = cues.len();
    let subtitle = Arc::new(Subtitle {
        format: SubFormat::Ass,
        title: None,
        cues: cues.to_vec(),
        encoding: "UTF-8",
    });
    *shared.subtitle.lock() = Some(Arc::clone(&subtitle));
    let _ = shared.event_tx.send(EngineEvent::SubtitleChanged(subtitle));
}

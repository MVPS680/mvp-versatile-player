//! The three decode worker threads.
//!
//! Every entry point catches panics: a malformed file must never take the whole
//! player down, it should surface as an error state the UI can show and the user
//! can recover from by opening something else.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use ffmpeg_next as ffmpeg;

use super::queue::{AudioMsg, PacketMsg, VideoMsg};
use super::{EngineConfig, EngineEvent, FlushRequest, MediaSource, PlaybackState, Shared};
use crate::audio::AudioSink;
use crate::bitmap_subtitle::{self, BitmapCue, BitmapSubtitle};
use crate::dsp::TimeStretcher;
use crate::info;
use crate::util::MediaKind;
use crate::video::{RgbaConverter, VideoFrame};
use mvp_subtitle::model::{Cue, SubFormat, Subtitle};

/// Video packets may queue this deep before the demuxer is throttled.
const VIDEO_PACKET_QUEUE: usize = 48;
/// Audio packets may queue this deep before the demuxer is throttled.
const AUDIO_PACKET_QUEUE: usize = 192;
/// How often the demuxer re-checks control state while blocked.
///
/// This is also the bound on how long a stop has to wait for a demuxer that is
/// blocked on a full channel, so it is deliberately short of the "one second of
/// queued packets" it could otherwise be.
const CONTROL_POLL: Duration = Duration::from_millis(50);

/// How long a decoder waits for a packet before re-checking for shutdown.
///
/// The wait is a condition-variable timeout, so a short one costs nothing while
/// idle, and it bounds how long a worker can sit inside `recv_timeout` after the
/// player has been asked to close. It used to be 200 ms, which is 200 ms of
/// "the window is still there" on the way out.
const WORKER_POLL: Duration = Duration::from_millis(25);

/// How far ahead of the playhead the video decoder may run, in seconds.
///
/// This is the decoder's lead over the master clock, not a drop threshold:
/// the decoder simply waits once the queue is this far ahead. Half a second is
/// enough to absorb a slow repaint or a disk hiccup, and small enough that a
/// seek or a rate change takes effect at once.
const VIDEO_LEAD: f64 = 0.5;

/// How much audio the decoder may hold ahead of the playhead, in *real*
/// seconds.
///
/// The sound path's counterpart to [`VIDEO_LEAD`]. Audio is decoded *ahead* of
/// the clock and time-stretched before it is queued, so everything already in
/// the queue keeps the speed it was decoded with: without a bound, an
/// audio-only file — where no video queue throttles the demuxer — fills the
/// whole chunk queue, about three seconds, and a rate change is not heard until
/// that backlog has drained, while the clock has already run off at the new
/// speed.
///
/// The bound is expressed in real seconds rather than media seconds because
/// that is what the user actually waits: 0.5 s of media queued at 0.25x still
/// takes two real seconds to play out, which would leave a slow-speed rate
/// change feeling just as late. Half a second of real audio covers a disk or
/// network hiccup without making a rate change feel late.
const AUDIO_LEAD: f64 = 0.5;

/// How long the decoder may be fed without producing a frame before the engine
/// declares the file unplayable.
///
/// Long enough for the first keyframe of a slow 4K stream, short enough that a
/// mis-detected container does not spin for minutes.
const DECODE_STALL: Duration = Duration::from_secs(12);

/// Packets that must have gone into the decoder before a lack of frames counts
/// as a stall rather than as "still starting up".
const STALL_MIN_PACKETS: u64 = 24;

/// How far the picture waiting to be shown may fall behind the clock before the
/// demuxer starts skipping packets instead of waiting for room.
///
/// The demuxer is the only source of *both* streams, so while it waits for room
/// in the video channel it is not sending audio either. That wait is therefore
/// bounded by how long the sound card's buffer can cover: a quarter of a second
/// of lag is already enough to say the picture has fallen behind, and skipping
/// is what gets the demuxer moving again.
const RESYNC_LAG: f64 = 0.25;

/// Slack added to a resampler output frame, in device-rate frames.
///
/// Covers the resampler's own filter delay plus one source frame, so a rate
/// conversion never has to buffer audio it could have handed over right away.
const RESAMPLE_MARGIN: usize = 4096;

/// How far behind the playhead a graphical subtitle cue is kept.
///
/// The decoder runs a little ahead of the clock, so a cue that just ended is
/// still needed to answer "what was on screen a moment ago"; anything older
/// than this is only taking up memory.
const BITMAP_WINDOW_BEHIND: f64 = 2.0;

/// Hard ceiling on palette-index bytes held by the graphical subtitle window.
const BITMAP_WINDOW_BYTES: usize = 32 * 1024 * 1024;

/// Hard ceiling on cues held by the graphical subtitle window.
const BITMAP_WINDOW_CUES: usize = 64;

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
    bitmap_subtitle::own_parameters(stream)
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
                    log::debug!(
                        "demuxer: dropping a video packet — the queue is {:.2}s behind the clock",
                        shared
                            .video_queue
                            .lock()
                            .front_pts()
                            .map(|pts| shared.clock.now() - pts)
                            .unwrap_or(0.0)
                    );
                    shared.dropped_frames.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            }
        }
    }
}

/// `true` when the picture waiting to be shown is already late.
///
/// The demuxer is the only source of *both* streams, so it may not sit blocked
/// on a full video channel: while it waits, no audio packet is sent either, and
/// a sound card with nothing in front of it runs dry. Once the queue's *front*
/// is behind the clock there is nothing left to lose by skipping — those frames
/// are in the past, and showing them would only push the picture later still.
///
/// A queue that is empty is never "behind": nothing has been decoded yet, so
/// dropping the packet that would produce the next frame can only hurt. That is
/// what makes this safe at the start of a file and right after a seek.
fn video_is_behind(shared: &Shared) -> bool {
    let now = shared.clock.now();
    let queue = shared.video_queue.lock();
    match queue.front_pts() {
        Some(front) => front < now - RESYNC_LAG,
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
    // Refuse what cannot possibly be played before handing anything to FFmpeg.
    // Opening stays asynchronous — the failure arrives as an `Error` event, just
    // like a file FFmpeg itself rejects — but a disc image gets a sentence
    // instead of a demuxer that reads garbage until the cows come home.
    if let MediaSource::Path(path) = source {
        crate::info::validate_input(path)?;
    }

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
    // How fast the source runs decides how many frames a given number of
    // seconds of buffer actually is, and the queue's time budget is expressed
    // in seconds.
    if let Some(video) = media_info.video.first() {
        shared.video_queue.lock().set_pacing(video.fps);
    }
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
        // Seed the device with the engine's *current* volume and mute state,
        // not with the configuration the engine was built from: the user may
        // have moved the volume slider since, and the sink is where the gain is
        // actually applied.
        match AudioSink::new(config.audio_device.as_deref(), shared.volume(), config.speed) {
            Ok(device) => {
                let device = Arc::new(device);
                device.set_muted(shared.muted.load(Ordering::Relaxed));
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
    //
    // Text and graphical subtitle codecs share one decoder; they differ only in
    // how the decoded subtitle is turned into something the interface can draw.
    // The third tuple slot remembers which of the two this track is.
    let mut subtitle_decoder = match subtitle_stream {
        Some(index) => match open_subtitle_decoder(&ictx, index) {
            Ok(decoder) => Some((
                index,
                decoder,
                !info::is_text_subtitle_codec(stream_codec_id(&ictx, index)),
            )),
            Err(err) => {
                log::warn!("无法打开内嵌字幕解码器: {err}");
                None
            }
        },
        None => None,
    };
    let mut local_subtitle_request = shared.subtitle_track.load(Ordering::Relaxed);
    let mut embedded_cues: Vec<Cue> = Vec::new();
    let mut last_published = 0usize;
    // Graphical cues are pruned as they pass instead of being accumulated whole:
    // a feature film can carry a couple of thousand bitmaps, and only the one on
    // screen (plus its immediate neighbours) is ever needed.
    let mut embedded_bitmaps: Vec<BitmapCue> = Vec::new();
    let mut bitmap_canvas: Option<(u32, u32)> = subtitle_stream.and_then(|index| {
        ictx.streams()
            .find(|stream| stream.index() == index)
            .and_then(|stream| bitmap_subtitle::stream_size(&stream))
    });

    // ---- main demux loop ---------------------------------------------------
    if config.autoplay {
        shared.clock.set_running(true);
        shared.set_state(PlaybackState::Playing);
    } else {
        shared.set_state(PlaybackState::Paused);
    }

    let mut generation = shared.generation.load(Ordering::SeqCst);
    let mut reached_eof = false;
    // Per-stream time base and start offset, filled in the first time a packet
    // of that stream is seen. See the packet loop below.
    let mut timings: std::collections::HashMap<usize, (f64, f64)> = std::collections::HashMap::new();
    // `true` once the end of the file has been reported. The pipeline is not
    // torn down then, it parks: a restart request is just a seek, so the loop
    // below has to keep running to serve it.
    let mut ended = false;

    loop {
        if shared.abort.load(Ordering::Relaxed) {
            break;
        }

        // -- seek ------------------------------------------------------------
        //
        // The request is taken out of its slot in a *scoped block*. Reading
        // naturally as `if let Some(target) = shared.seek_request.lock().take()`
        // is an instant self-deadlock: an `if let` keeps the temporary guard for
        // the whole body, and `seek_container` calls into FFmpeg, whose
        // interrupt callback — installed on this very context — locks the same
        // field. A container with an index never noticed, because its seek is an
        // index lookup that reads nothing; an MPEG-TS seeks by binary search, so
        // every packet it read called back into the demuxer thread that was
        // holding the lock, and the picture froze for good.
        let pending_seek = {
            let mut request = shared.seek_request.lock();
            request.take()
        };
        if let Some(target) = pending_seek {
            generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
            shared.video_queue.lock().clear();
            shared.wake_video();
            shared.clock.seek(target);
            *shared.seek_target.lock() = Some(target);
            shared.dropped_frames.store(0, Ordering::Relaxed);
            // Hand the request over through shared state: the packet channels
            // may hold a second's worth of stale data, and waiting for a slot in
            // them would stall the seek behind it.
            shared.request_flush(generation, target);
            // The audio worker resets this when it acts on the flush, but the
            // demuxer can reach the end of the file and consult `handle_eof`
            // before that happens — a stale `true` from the previous run would
            // then end playback the instant a seek landed. Clear it where the
            // seek is published, so "ended" can never survive a seek.
            shared.audio_ended.store(false, Ordering::Relaxed);
            // Graphical cues belong to the position being left and the window is
            // bounded, so they are dropped and decoded again from the new one.
            // Text cues are accumulated whole and can stay. The subtitle decoder
            // is flushed with them: PGS carries a palette and object table from
            // one composition to the next, and those belong to the old position.
            if shared.external_bitmap.lock().is_none() {
                embedded_bitmaps.clear();
                *shared.bitmap_subtitle.lock() = None;
            }
            if let Some((_, decoder, _)) = subtitle_decoder.as_mut() {
                decoder.flush();
            }
            // A seek that fails because a *newer* one arrived is not a broken
            // file: the interrupt callback returns as soon as another request is
            // in the slot, which is what fast scrubbing looks like from inside
            // FFmpeg. Tearing the demuxer down here (`?`) left the player with a
            // dead pipeline and an error banner after nothing worse than
            // dragging the seek bar quickly; the newer request is served by the
            // top of this loop either way. A genuinely unseekable file is the
            // same story: the reads that follow simply carry on from wherever
            // the demuxer got to.
            if let Err(err) = seek_container(&mut ictx, target) {
                log::warn!("跳转失败（{err}），继续播放: {target:.3}s");
            }
            reached_eof = false;
            ended = false;
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
                open_subtitle_decoder(&ictx, index)
                    .map_err(|e| log::warn!("字幕解码器打开失败: {e}"))
                    .ok()
                    .map(|decoder| {
                        (
                            index,
                            decoder,
                            !info::is_text_subtitle_codec(stream_codec_id(&ictx, index)),
                        )
                    })
            });
            bitmap_canvas = new_index.and_then(|index| {
                ictx.streams()
                    .find(|stream| stream.index() == index)
                    .and_then(|stream| bitmap_subtitle::stream_size(&stream))
            });
            embedded_cues.clear();
            embedded_bitmaps.clear();
            last_published = 0;
            if shared.external_bitmap.lock().is_none() {
                *shared.bitmap_subtitle.lock() = None;
            }
            publish_subtitles(shared, &embedded_cues, &mut last_published, true);
            continue;
        }

        // -- read a packet ---------------------------------------------------
        if reached_eof {
            if handle_eof(shared, &embedded_cues, &mut last_published, &mut ended)? {
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
            // Tagged with the session they belong to: a read that the *next*
            // seek interrupts reports the end of the stream too, and by the time
            // the worker gets round to this message the flush of that seek may
            // already have run. See [`VideoMsg::Eof`].
            let _ = video_tx.send(VideoMsg::Eof { generation });
            let _ = audio_tx.send(AudioMsg::Eof { generation });
            continue;
        };

        let index = stream.index();

        // The time base and the start offset are fixed for the whole file, and
        // a long one hands over millions of packets: looked up once per stream
        // instead of once per packet.
        let (time_base, start_offset) = match timings.entry(index) {
            std::collections::hash_map::Entry::Occupied(slot) => *slot.get(),
            std::collections::hash_map::Entry::Vacant(slot) => {
                let time_base = super::rational_to_f64(stream.time_base());
                let start_offset = stream_start_offset(&stream, time_base);
                *slot.insert((time_base, start_offset))
            }
        };

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
        } else if let Some((sub_index, decoder, is_bitmap)) = subtitle_decoder.as_mut() {
            if index == *sub_index {
                if *is_bitmap {
                    if let Some(cue) =
                        bitmap_subtitle::decode_packet(decoder, &packet, time_base, start_offset)
                    {
                        push_bitmap_cue(&mut embedded_bitmaps, cue);
                        let floor = shared.clock.now() - BITMAP_WINDOW_BEHIND;
                        bitmap_subtitle::prune(&mut embedded_bitmaps, floor);
                        cap_bitmap_window(&mut embedded_bitmaps);
                        publish_bitmap_subtitles(shared, &embedded_bitmaps, bitmap_canvas);
                    }
                } else if let Some(cue) =
                    decode_subtitle_packet(decoder, &packet, time_base, start_offset)
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
/// The end of the file does **not** tear the pipeline down: the workers are left
/// idle and the demuxer parks here, with the clock stopped, until the interface
/// asks for a replay. That request arrives as a seek, so the main loop above
/// handles the actual restart. Shutting the workers down instead is what made
/// "play" on a finished file run the clock past the end of the media: there was
/// nothing left to decode, and nothing left to stop the clock either.
///
/// Returns `Ok(true)` when the demuxer should stop entirely, which is now only
/// ever a shutdown.
fn handle_eof(
    shared: &Arc<Shared>,
    embedded_cues: &[Cue],
    last_published: &mut usize,
    ended: &mut bool,
) -> crate::error::Result<bool> {
    if shared.abort.load(Ordering::Relaxed) {
        return Ok(true);
    }

    // Looping is the one thing that revives a file on its own. It is checked
    // before the park below because a request set while parked would otherwise
    // never be acted on.
    if shared.looping.load(Ordering::Relaxed) {
        if *ended {
            // Looping was switched on *after* the file finished, so nothing else
            // will ask for the replay this loop is about to perform.
            shared.clock.seek(0.0);
            shared.clock.set_running(true);
            if let Some(sink) = shared.audio_sink() {
                sink.set_paused(false);
            }
            shared.set_state(PlaybackState::Playing);
        }
        *shared.seek_request.lock() = Some(0.0);
        return Ok(false);
    }

    if *ended {
        // The end has been reported and no restart has arrived yet. Sleep a
        // little so a parked file costs nothing; an actual request is picked up
        // by the main loop before this function is reached.
        std::thread::sleep(Duration::from_millis(20));
        return Ok(false);
    }

    publish_subtitles(shared, embedded_cues, last_published, true);

    let duration = *shared.duration.lock();
    let now = shared.clock.now();
    let video_idle = shared.video_queue.lock().is_empty();
    let (pending_frames, audio_only) = {
        let info = shared.info.lock();
        let pending = info
            .as_ref()
            .map(|i| i.video.first().map(|v| v.frames).unwrap_or(0))
            .unwrap_or(0);
        let audio_only = info.as_ref().map(|i| i.video.is_empty()).unwrap_or(false);
        (pending, audio_only)
    };
    let audio_idle = shared
        .audio
        .lock()
        .as_ref()
        .map(|a| a.queued_chunks() == 0)
        .unwrap_or(true);

    let finished = if duration > 0.0 {
        if audio_only {
            // A sound file is over when its audio has actually been played out,
            // not when the wall clock reaches the file's nominal length. The
            // clock is wall time and knows nothing about the samples: a seek
            // that lands late, or metadata that runs past the last sample,
            // would otherwise keep the bar moving through silence after the
            // sound had already stopped. Draining the queue *and* the decoder
            // sitting at EOF is what "the sound is finished" means, and the bar
            // then stops where the sound stopped.
            match shared.audio.lock().as_ref() {
                Some(a) => shared.audio_ended.load(Ordering::Relaxed) && a.queued_chunks() == 0,
                // No device: nothing will ever play, so fall back to the clock.
                None => now >= duration - 0.10,
            }
        } else {
            now >= duration - 0.10
        }
    } else {
        // Live streams have no duration; only stop when the stream really ended.
        video_idle && audio_idle && pending_frames == 0
    };

    if finished {
        *ended = true;
        shared.clock.set_running(false);
        if let Some(sink) = shared.audio_sink() {
            sink.set_paused(true);
        }
        shared.set_state(PlaybackState::Ended);
        let _ = shared.event_tx.send(EngineEvent::Ended);
        return Ok(false);
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
///
/// `seconds` is *playback* time, which starts at zero even when the container
/// does not. Handing that offset-less number straight to the demuxer points it at
/// a place in the file that does not correspond to the requested position, and
/// because every frame it then produces is still before the seek target, the
/// picture never comes back — a seek that kills playback for good.
fn seek_container(
    ictx: &mut ffmpeg::format::context::Input,
    seconds: f64,
) -> crate::error::Result<()> {
    // `seconds` counts from the start of *playback*; the container's timeline may
    // begin much later (a Blu-ray style transport stream starts at 4200s), so the
    // demuxer has to be given the absolute timestamp.
    // SAFETY: `ictx` owns a live AVFormatContext and `start_time` is a plain
    // field of it, expressed in AV_TIME_BASE units.
    let container_start = unsafe { (*ictx.as_ptr()).start_time };
    let offset = if container_start > 0 && container_start != i64::MIN {
        container_start as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
    } else {
        0.0
    };

    let timestamp = ((seconds.max(0.0) + offset) * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
    log::debug!(
        "seek: playback {seconds:.3}s + container start {offset:.3}s -> ts {timestamp} \
         ({:.3}s in the container)",
        timestamp as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
    );
    // No `avformat_flush` here. It looks harmless — "discard the read-ahead" —
    // but on a plain MP3 it rewinds the demuxer to the *start of the file*:
    // `avformat_seek_file` followed by a read hands back the packet at 160s,
    // while the same seek followed by `avformat_flush` hands back the packet at
    // 0s. The seek is all that is needed; the demuxer has already dropped the
    // buffers that belonged to the old position.
    ictx.seek(timestamp, ..timestamp)?;
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
    // A stream that keeps feeding the decoder without ever producing a frame is
    // not playing, it is spinning. A mis-detected container does exactly that —
    // a disc image handed to the MPEG demuxer, say — and it does it forever
    // while the log fills with decoder errors. Watch the counters rather than
    // trusting the file to be what its extension claims.
    let mut packets_fed = 0u64;
    let mut watched_generation = generation;
    let mut last_progress = Instant::now();
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
        // Shutting down: whatever is in the channel belongs to a playback session
        // that no longer exists. Checking here — before the queue is read — is
        // what keeps closing the window from waiting for a backlog of up to
        // `VIDEO_PACKET_QUEUE` frames to be decoded and thrown away.
        if shared.abort.load(Ordering::Relaxed) {
            break;
        }
        // A pending flush is honoured before anything else, including packets
        // that are already in the channel: those belong to the previous decode
        // session and would otherwise be decoded and throttled against a clock
        // that has already moved.
        if let Some(request) = shared.take_video_flush() {
            reset_video_session(
                &shared,
                &mut decoder,
                &mut generation,
                &mut seek_target,
                &mut last_pts,
                request,
            );
        }

        let msg = match rx.recv_timeout(WORKER_POLL) {
            Ok(msg) => msg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        // The `Stop` message the demuxer sends on the way out sits *behind*
        // whatever it had already queued, so it is not enough to react to it:
        // the abort flag is what makes the queue irrelevant.
        if shared.abort.load(Ordering::Relaxed) {
            break;
        }

        // A flush is re-checked here, *after* the wait, because the packet that
        // woke us may be the first one of the new session and it may have
        // overtaken the request in the channel: the demuxer publishes the flush
        // and then seeks, and a seek that lands instantly (to zero, on an empty
        // channel) puts its keyframe in the channel before this thread is
        // scheduled. Skipping that keyframe would leave the decoder with nothing
        // it can decode — for a file with a single keyframe at the start, the
        // picture would never come back.
        if let Some(request) = shared.take_video_flush() {
            reset_video_session(
                &shared,
                &mut decoder,
                &mut generation,
                &mut seek_target,
                &mut last_pts,
                request,
            );
        }

        // A flush moves the decoder to another part of the file: the frames for
        // *that* part have not been decoded yet, so the stall clock starts over
        // instead of counting the seek as a failure.
        if generation != watched_generation {
            watched_generation = generation;
            packets_fed = 0;
            last_progress = Instant::now();
        }

        let frames_before = shared.decoded_frames.load(Ordering::Relaxed);
        match msg {
            VideoMsg::Stop => {
                break;
            }
            VideoMsg::Eof { generation: ended } => {
                // An end that belongs to a session the flush has already moved
                // on from is not an end of anything: the decoder has just been
                // flushed and is waiting for the first packet of the new
                // session. Draining it here would answer every one of those
                // packets with `AVERROR_EOF` and freeze the picture for good.
                if ended != generation {
                    log::debug!(
                        "video worker: ignoring an end-of-stream from generation {ended} \
                         (at {generation})"
                    );
                    continue;
                }
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
                    None,
                    generation,
                    &mut seek_target,
                    hardware_format,
                    time_base,
                    start_offset,
                );
            }
            VideoMsg::Packet(msg) => {
                if msg.generation != generation {
                    log::debug!(
                        "video worker: skipping a packet from generation {} (at {})",
                        msg.generation,
                        generation
                    );
                    continue;
                }
                log::debug!("video worker: packet at {:?}", msg.timestamp);
                if let Err(err) = decoder.send_packet(&msg.packet) {
                    // A corrupted packet is normal in the wild; keep going.
                    log::debug!("video worker: the decoder refused a packet: {err}");
                    continue;
                }
                packets_fed += 1;
                drain_video(
                    &shared,
                    &mut decoder,
                    &mut converter,
                    &mut frame,
                    &mut hw_scratch,
                    &mut last_pts,
                    &mut serial,
                    fallback_step,
                    msg.timestamp,
                    generation,
                    &mut seek_target,
                    hardware_format,
                    time_base,
                    start_offset,
                );
            }
        }

        if shared.decoded_frames.load(Ordering::Relaxed) > frames_before {
            last_progress = Instant::now();
        } else if packets_fed >= STALL_MIN_PACKETS && last_progress.elapsed() >= DECODE_STALL {
            // Packets keep going in and nothing comes out: say so and stop,
            // rather than spinning until the user gives up on the window.
            report_error(
                &shared,
                "解码无输出：文件可能已损坏，或不是受支持的媒体格式".to_string(),
            );
            shared.abort.store(true, Ordering::SeqCst);
            break;
        }
    }
    shared.pool.clear();
}

/// Move the video decoder onto the decode session a flush request describes.
///
/// Flushing the codec drops the frames still inside it, and clearing the
/// presentation queue drops the ones still waiting to be shown: both belong to
/// the position the playhead has just left.
#[allow(clippy::too_many_arguments)]
fn reset_video_session(
    shared: &Shared,
    decoder: &mut ffmpeg::decoder::Video,
    generation: &mut u64,
    seek_target: &mut Option<f64>,
    last_pts: &mut f64,
    request: FlushRequest,
) {
    *generation = request.generation;
    *seek_target = Some(request.target);
    *last_pts = request.target;
    log::debug!(
        "video worker: flush to {:.3}s (generation {})",
        request.target,
        request.generation
    );
    decoder.flush();
    shared.video_queue.lock().clear();
    // The queue just gained all the room there is, and the decoder may be
    // waiting for some of it.
    shared.wake_video();
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
    packet_pts: Option<f64>,
    generation: u64,
    seek_target: &mut Option<f64>,
    hardware_format: Option<ffmpeg::ffi::AVPixelFormat>,
    time_base: f64,
    start_offset: f64,
) {
    while decoder.receive_frame(frame).is_ok() {
        // A pending seek invalidates everything still in the pipeline; bail out
        // so the caller can act on it instead of finishing this batch. A
        // shutdown is the same thing with no destination: finishing the batch
        // would only delay the join.
        if shared.video_flush_pending() || shared.abort.load(Ordering::Relaxed) {
            return;
        }

        // A hardware decoder hands back a frame that lives in GPU memory; the
        // colour conversion needs it in RAM. Whether that copy is needed at all
        // is decided below: a frame that is about to be dropped should not be
        // taken out of the GPU first, and a seek drops every frame in front of
        // the position that was asked for.
        let hardware = is_hardware_frame(frame, hardware_format);

        // When does this frame play?
        //
        // A hardware decoder hands back frames with no timestamp at all, and the
        // answer used to be "one frame after the previous one" — counted from
        // `last_pts`, which a seek seeds with the *target*. That invented a
        // timeline of its own: after a seek to 8.9s in a 9s file the decoder
        // starts at the keyframe (say 8.0s) and every frame it produces is
        // labelled from 8.9s upward, so frames that really play at 8.0s are
        // queued as 8.94s, 8.98s … 9.9s. Nothing in the queue is ever "due"
        // again, the picture freezes on the last frame that was, and — because
        // the demuxer is blocked handing out packets nobody consumes — the end
        // of the file is never noticed, so the clock runs on and the position
        // climbs past its own duration for as long as the user lets it.
        //
        // The packet knows where the frame is: it was demuxed, timestamped and
        // sent for exactly this frame. So the decoder is asked first, the packet
        // second, and only a stream that says nothing about its own timeline
        // falls back to "one frame on", which is at least bounded by the frame
        // itself rather than by where the user last clicked.
        let pts = match frame.pts() {
            Some(raw) => raw as f64 * time_base - start_offset,
            None => packet_pts.unwrap_or(*last_pts + fallback_step),
        };
        let pts = if pts.is_finite() {
            pts
        } else {
            *last_pts + fallback_step
        };
        log::debug!(
            "video frame at {pts:.3}s (raw {:?}, packet {packet_pts:?}, time_base {time_base:.6}, \
             offset {start_offset:.3}s)",
            frame.pts()
        );

        let now = shared.clock.now();
        let paused = !shared.state().is_playing();

        // Frames from before the seek target belong to the keyframe we landed
        // on, not to the position the user asked for. They are dropped here,
        // before anything has been converted or copied.
        if let Some(target) = *seek_target {
            if pts + 0.02 < target {
                log::debug!("video frame at {pts:.3}s dropped: before the seek target {target:.3}s");
                // Counted apart from `dropped_frames`: this one never cost a
                // copy or a colour conversion, which is the distinction that
                // says whether the pipeline is spending its time on frames the
                // screen will show.
                shared.perf.skipped_frames.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }

        // The presentation queue is read in order and the interface relies on
        // timestamps that never go backwards, so a frame is stamped with the
        // last time handed over when a stream's own timestamps disagree.
        let pts = pts.max(*last_pts);
        *last_pts = pts;

        // If the decoder has fallen behind, drop rather than fall further
        // behind — but never while paused, where every frame is precious. Both
        // drops come before the copy below: a frame that is thrown away has no
        // business being taken out of the GPU first.
        if !paused && pts < now - 0.25 && !shared.video_queue.lock().is_empty() {
            shared.dropped_frames.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let source: &ffmpeg::frame::Video = if hardware {
            let started = Instant::now();
            let copied = download_frame(frame, hw_scratch);
            shared
                .perf
                .download_ms
                .record(started.elapsed().as_secs_f32() * 1000.0);
            match copied {
                Ok(()) => hw_scratch,
                Err(err) => {
                    log::warn!("{err}");
                    continue;
                }
            }
        } else {
            frame
        };

        let target = *shared.target_size.lock();
        let (width, height) = match target {
            Some((tw, th)) => crate::util::fit_inside(
                (source.width(), source.height()),
                crate::util::even((tw, th)),
            ),
            None => (source.width(), source.height()),
        };

        // The preference can change while a file plays, so it is re-read for
        // every frame; the converter only rebuilds its tables when it flips.
        converter.set_tone_map(shared.hdr_tone_map.load(Ordering::Relaxed));
        let convert_started = Instant::now();
        let data = match converter.convert(source, width, height, &shared.pool) {
            Ok(data) => data,
            Err(err) => {
                log::warn!("视频帧转换失败: {err}");
                continue;
            }
        };
        let convert_ms = convert_started.elapsed().as_secs_f32() * 1000.0;
        shared.perf.convert_ms.record(convert_ms);
        // Reported by the converter itself: the tone map is a per-pixel loop
        // inside the conversion, so it cannot be timed from out here.
        shared.perf.tone_map_ms.record(converter.last_tone_map_ms());

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
        // The wait re-checks the abort flag and a pending seek every time, and
        // both of those *signal* the queue, so a seek or a stop is acted on at
        // once rather than up to a poll interval later.
        let decoded = Arc::new(decoded);
        let wait_started = Instant::now();
        let mut queue = shared.video_queue.lock();
        loop {
            if shared.video_flush_pending() || shared.abort.load(Ordering::Relaxed) {
                return;
            }
            // While paused the clock stands still, so only the queue's own
            // capacity bounds the decoder — that is what lets frame stepping
            // walk forward.
            let playing = shared.state().is_playing();
            let now = shared.clock.now();
            let back = queue.back_pts();
            let lead_ok = !playing
                || queue.is_empty()
                || back.map(|back| back - now < VIDEO_LEAD).unwrap_or(true);
            if lead_ok && queue.try_push(Arc::clone(&decoded)).is_none() {
                break;
            }
            // This used to be a flat four-millisecond sleep, which is 250
            // wake-ups a second for a wait that ends the instant the interface
            // takes a frame — and up to four milliseconds of jitter on every
            // frame that arrived with the queue full.
            //
            // How long the wait can possibly be is computable, so wait exactly
            // that long: a full queue is waiting for the interface (or for a
            // seek emptying it), and both of those signal; a closed lead window
            // is waiting for the clock, which nothing can signal but whose
            // reopening time is known. `CONTROL_POLL` is the ceiling either way,
            // and doubles as the bound on how late anything unnoticed can be.
            let sleep = if lead_ok {
                CONTROL_POLL
            } else {
                match back {
                    Some(back) => {
                        let seconds = (back - VIDEO_LEAD - now)
                            .clamp(0.001, CONTROL_POLL.as_secs_f64());
                        Duration::from_secs_f64(seconds)
                    }
                    None => CONTROL_POLL,
                }
            };
            shared.video_ready.wait_for(&mut queue, sleep);
        }
        drop(queue);
        shared
            .perf
            .queue_wait_ms
            .record(wait_started.elapsed().as_secs_f32() * 1000.0);
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

/// Surfaces the hardware decoder may hold beyond what the player holds.
///
/// Four is enough to bridge the download-and-convert stage without reserving a
/// noticeable amount of video memory; the point is to stop the pool running dry
/// on high-resolution, high-frame-rate material.
const HW_EXTRA_FRAMES: i32 = 4;

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
            // How many frames the decoder may keep on the GPU beyond what the
            // caller holds.
            //
            // Zero lets libavcodec size the pool by the decoder's own needs
            // (thread count, reordering depth), which is tuned for *decoding*
            // and not for a player that holds frames for a while. The picture
            // is downloaded and converted on one thread here, so at 4K and a
            // high frame rate the pool drains faster than it refills, and
            // `avcodec_receive_frame` starts returning EAGAIN on a decoder that
            // is not actually out of work — which shows up as a stalled picture
            // rather than as an error. Asking for a handful of extra frames
            // costs a little VRAM (a few surface slots) and removes the stall.
            if (*ctx).extra_hw_frames == 0 {
                (*ctx).extra_hw_frames = HW_EXTRA_FRAMES;
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
    // Position a flush moved the playhead to. Like the video decoder, the audio
    // decoder has to throw away the samples that belong before it: the container
    // seek snaps back to an earlier point — for some formats, to the beginning of
    // the file — and without this the sound would replay from there while the
    // clock and the picture are already at the target.
    let mut seek_target: Option<f64> = None;
    let mut scratch: Vec<f32> = Vec::new();

    loop {
        // Shutting down: stop before the queued packets are decoded, so the
        // backlog the demuxer left behind costs nothing to abandon. See the
        // video worker for the same check.
        if shared.abort.load(Ordering::Relaxed) {
            break;
        }
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
            reset_audio_session(
                &shared,
                &mut decoder,
                &sink,
                &mut generation,
                &mut last_pts,
                &mut resampler,
                &mut stretcher,
                &mut seek_target,
                request,
            );
        }

        let msg = match rx.recv_timeout(WORKER_POLL) {
            Ok(msg) => msg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        if shared.abort.load(Ordering::Relaxed) {
            break;
        }

        // Re-checked after the wait for the same reason as the video worker: the
        // chunk that woke us may be the first of the new session and may have
        // overtaken the request in the channel.
        if let Some(request) = shared.take_audio_flush() {
            reset_audio_session(
                &shared,
                &mut decoder,
                &sink,
                &mut generation,
                &mut last_pts,
                &mut resampler,
                &mut stretcher,
                &mut seek_target,
                request,
            );
        }

        match msg {
            AudioMsg::Stop => break,
            AudioMsg::Eof { generation: ended } => {
                // Same guard as the video worker's: see [`VideoMsg::Eof`]. An
                // audio decoder left drained answers every packet with
                // `AVERROR_EOF`, which is silence for the rest of the file.
                if ended != generation {
                    continue;
                }
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
                    &mut seek_target,
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
                    &mut seek_target,
                    false,
                );
            }
        }
    }
}

/// Move the audio decoder onto the decode session a flush request describes.
///
/// The resampler and the time stretcher hold samples of their own, so they are
/// reset rather than flushed, and the sound card is re-anchored on the position
/// the playhead has just moved to.
#[allow(clippy::too_many_arguments)]
fn reset_audio_session(
    shared: &Shared,
    decoder: &mut ffmpeg::decoder::Audio,
    sink: &AudioSink,
    generation: &mut u64,
    last_pts: &mut f64,
    resampler: &mut Option<ffmpeg::software::resampling::Context>,
    stretcher: &mut TimeStretcher,
    seek_target: &mut Option<f64>,
    request: FlushRequest,
) {
    *generation = request.generation;
    *last_pts = request.target;
    *seek_target = Some(request.target);
    decoder.flush();
    *resampler = None;
    stretcher.reset();
    stretcher.set_speed(shared.speed());
    sink.flush(request.target);
    shared.audio_ended.store(false, Ordering::Relaxed);
}

/// Wait until the audio just decoded is no more than [`AUDIO_LEAD`] ahead of the
/// playhead.
///
/// Returns `false` when the batch must be abandoned: the player is shutting down,
/// or a seek / track change is waiting and must not be left behind a full queue.
/// The wait is a plain sleep because the clock — the thing being waited for —
/// advances on its own; each sleep is bounded to a poll interval so a control
/// request is noticed promptly.
fn wait_for_audio_lead(shared: &Shared, pts: f64) -> bool {
    loop {
        if shared.abort.load(Ordering::Relaxed) || shared.audio_flush.lock().is_some() {
            return false;
        }
        // Media seconds ahead, converted to the real time it will take to play
        // them out. The clock closes that real gap at one second per second, so
        // the wait below is exact while the speed is steady, and re-measured
        // after every sleep.
        let speed = shared.speed().max(0.05);
        let lead = (pts - shared.clock.now()) / speed;
        if lead <= AUDIO_LEAD {
            return true;
        }
        let wait = (lead - AUDIO_LEAD).clamp(0.001, CONTROL_POLL.as_secs_f64());
        std::thread::sleep(Duration::from_secs_f64(wait));
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
    seek_target: &mut Option<f64>,
    flushing: bool,
) {
    // Keep the stretcher in step with the UI slider.
    if (stretcher.speed() - shared.speed()).abs() > 1e-6 {
        stretcher.set_speed(shared.speed());
    }

    loop {
        // A shutdown abandons the batch: see `drain_video`.
        if shared.abort.load(Ordering::Relaxed) {
            return;
        }
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

        // Throw away the audio that belongs before a seek target, exactly as the
        // video decoder does. The container seek snaps back to an earlier point
        // (for a plain MP3, to the start of the file), so without this the sound
        // would be replayed from there while the clock and the picture are
        // already at the target — the audio then runs out early and playback
        // ends before the bar has reached the end.
        if let Some(target) = *seek_target {
            let frame_secs = input_samples as f64 / frame.rate().max(1) as f64;
            if pts + frame_secs <= target {
                continue;
            }
            // This frame reaches the target: stop skipping, keep the whole
            // frame. Trimming it to the sample would be finer, but the residual
            // is under one frame (a few milliseconds) and the video decoder
            // makes the same trade.
            *seek_target = None;
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
        // The volume is *not* applied here: this runs seconds ahead of the
        // playhead, so a change would not be heard until the buffer drained.
        // The output callback applies it as the samples are played.

        let mut stretched: Vec<f32> = Vec::with_capacity(expected + 4096);
        stretcher.push(scratch, &mut stretched);
        if !stretched.is_empty() {
            // Never let the already-stretched output pile up ahead of the
            // playhead: it carries whatever speed it was decoded with, so a
            // backlog is a rate change the user has not heard yet. See
            // [`AUDIO_LEAD`].
            if !wait_for_audio_lead(shared, pts) {
                return;
            }
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
    if !matches!(decoder.decode(packet, &mut subtitle), Ok(true)) {
        bitmap_subtitle::free_subtitle(&mut subtitle);
        return None;
    }

    let cue = (|| {
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
    })();
    // The decoder allocated the text rectangles; `ffmpeg-next`'s wrapper has no
    // `Drop`, so they are released here before the subtitle goes out of scope.
    bitmap_subtitle::free_subtitle(&mut subtitle);
    cue
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
    if shared.has_external_subtitle() {
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

/// Add a decoded graphical cue to the sliding window.
///
/// A PGS composition with no rectangles is a "clear": it closes whatever was on
/// screen and shows nothing itself. A cue with no duration of its own is closed
/// at the next cue's start, or given a short default if it is the last one.
fn push_bitmap_cue(cues: &mut Vec<BitmapCue>, cue: BitmapCue) {
    if cue.rects.is_empty() {
        if let Some(last) = cues.last_mut() {
            if cue.start > last.start {
                last.end = cue.start;
            }
        }
        return;
    }
    if let Some(last) = cues.last_mut() {
        if last.end <= last.start {
            last.end = if cue.start > last.start {
                cue.start
            } else {
                last.start + 4.0
            };
        }
    }
    let mut cue = cue;
    if cue.end <= cue.start {
        cue.end = cue.start + 4.0;
    }
    cues.push(cue);
}

/// Drop the oldest graphical cues until the window is inside its byte and count
/// budgets, so a pathological track cannot grow the player's memory forever.
fn cap_bitmap_window(cues: &mut Vec<BitmapCue>) {
    while cues.len() > BITMAP_WINDOW_CUES || bitmap_subtitle::byte_size(cues) > BITMAP_WINDOW_BYTES
    {
        if cues.is_empty() {
            break;
        }
        cues.remove(0);
    }
}

/// Publish the graphical cue window, unless an external subtitle is in charge.
fn publish_bitmap_subtitles(
    shared: &Arc<Shared>,
    cues: &[BitmapCue],
    canvas: Option<(u32, u32)>,
) {
    if shared.has_external_subtitle() {
        return;
    }
    if cues.is_empty() {
        return;
    }
    let track = Arc::new(BitmapSubtitle {
        cues: cues.to_vec(),
        canvas,
    });
    *shared.bitmap_subtitle.lock() = Some(Arc::clone(&track));
    let _ = shared
        .event_tx
        .send(EngineEvent::BitmapSubtitleChanged(track));
}

//! End-to-end tests against real media files.
//!
//! These are the tests that would have caught the mistakes unit tests cannot:
//! a wrong pixel format, a decoder that never produces a frame, a clock that
//! never advances, or a seek that silently lands in the wrong place. The
//! fixtures in `testdata/` are tiny (a few hundred kilobytes) and are generated
//! by `scripts/make-testdata.ps1`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource, PlaybackState};
use mvp_core::{Engine, ImageView};

/// Absolute path to a fixture.
fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join(name)
}

/// Skip a test when its fixture is missing, so a fresh checkout without the
/// generated media still builds a green test run.
fn fixture(name: &str) -> Option<PathBuf> {
    let path = testdata(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: {} is missing", path.display());
        None
    }
}

/// An engine with the audio device disabled so the tests are deterministic and
/// silent.
fn silent_engine() -> Engine {
    let config = EngineConfig {
        audio_enabled: false,
        autoplay: true,
        ..EngineConfig::default()
    };
    Engine::new(config).expect("engine construction")
}

/// Wait for the `Opened` event, returning the probed media info.
fn wait_for_open(engine: &Engine, timeout: Duration) -> Arc<mvp_core::MediaInfo> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match engine.poll_event() {
            Some(EngineEvent::Opened(info)) => return info,
            Some(EngineEvent::Error(message)) => panic!("engine reported an error: {message}"),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("the file did not open within {timeout:?}");
}

/// Collect decoded frames until `wanted` have arrived or the deadline passes.
fn collect_frames(engine: &Engine, wanted: usize, timeout: Duration) -> Vec<Arc<mvp_core::VideoFrame>> {
    let mut frames = Vec::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && frames.len() < wanted {
        let now = engine.display_position();
        if let Some(frame) = engine.take_frame(now) {
            frames.push(frame);
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    frames
}

#[test]
fn opens_a_video_and_reports_its_streams() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));

    assert_eq!(info.video.len(), 1, "one video stream expected");
    let video = info.primary_video().expect("a video stream");
    assert_eq!(video.width, 320);
    assert_eq!(video.height, 240);
    assert!(video.codec.contains("h264"), "got codec {}", video.codec);
    assert!(!video.pixel_format.is_empty());
    assert!((video.fps - 25.0).abs() < 0.5, "got {} fps", video.fps);

    assert_eq!(info.audio.len(), 1, "one audio stream expected");
    let audio = info.primary_audio().expect("an audio stream");
    assert!(audio.sample_rate >= 8000);
    assert!(audio.channels >= 1);

    assert!(info.duration > 2.0, "got duration {}", info.duration);
    assert!(info.size > 0);
    assert!(!info.is_still_image);
    engine.stop();
}

#[test]
fn autoplay_decides_whether_an_opened_file_starts_running() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };

    // The interface opens a file for the user (command line, drag and drop)
    // with `autoplay` set from the settings: with it off, the file must load
    // and wait rather than not open at all.
    let engine = silent_engine();
    engine.set_autoplay(false);
    engine.open(MediaSource::Path(path.clone())).expect("open");
    let _ = wait_for_open(&engine, Duration::from_secs(15));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        engine.state(),
        PlaybackState::Paused,
        "with autoplay off, an opened file must load paused"
    );
    assert!(
        engine.duration() > 0.0,
        "the file must still be probed and loaded"
    );

    // Selecting an entry inside the player is an explicit instruction to play,
    // and the interface says so by flipping this back on before opening.
    engine.set_autoplay(true);
    engine.open(MediaSource::Path(path)).expect("open");
    let _ = wait_for_open(&engine, Duration::from_secs(15));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        engine.state(),
        PlaybackState::Playing,
        "an explicit selection must start playing even though the autoplay \
         preference is off"
    );
    assert!(
        !collect_frames(&engine, 1, Duration::from_secs(3)).is_empty(),
        "playback must actually be running"
    );
    engine.stop();
}

#[test]
fn decodes_frames_at_the_source_resolution_and_advances_the_clock() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.set_target_size(320, 240);
    engine.open(MediaSource::Path(path)).expect("open");
    wait_for_open(&engine, Duration::from_secs(15));

    let frames = collect_frames(&engine, 6, Duration::from_secs(15));
    assert!(
        frames.len() >= 6,
        "expected at least six decoded frames, got {}",
        frames.len()
    );

    for (index, frame) in frames.iter().enumerate() {
        assert!(frame.is_valid(), "frame {index} has no pixels");
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 240);
        assert_eq!(frame.data.len(), 320 * 240 * 4);
        assert!(frame.pts.is_finite(), "frame {index} has a bogus timestamp");
    }

    // Timestamps must be non-decreasing, which is what the renderer relies on.
    for pair in frames.windows(2) {
        assert!(
            pair[1].pts >= pair[0].pts,
            "timestamps went backwards: {} then {}",
            pair[0].pts,
            pair[1].pts
        );
    }
    assert!(frames.last().unwrap().pts > 0.0, "the clock must advance");

    // The picture must not be uniformly black — that would mean the colour
    // conversion silently produced nothing.
    let frame = frames.last().unwrap();
    let non_black = frame
        .data
        .chunks_exact(4)
        .filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8)
        .count();
    assert!(
        non_black > 100,
        "the decoded frame looks empty ({non_black} non-black pixels)"
    );
    engine.stop();
}

#[test]
fn seeking_lands_near_the_requested_position() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));
    assert!(info.duration > 2.0);

    let frames = collect_frames(&engine, 2, Duration::from_secs(10));
    assert!(!frames.is_empty(), "playback must start before seeking");

    engine.seek(2.0);
    // The seek is asynchronous: give the demuxer time to land.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut after_seek: Vec<Arc<mvp_core::VideoFrame>> = Vec::new();
    while Instant::now() < deadline && after_seek.len() < 3 {
        if let Some(frame) = engine.take_frame(engine.display_position()) {
            if frame.pts > 1.0 {
                after_seek.push(frame);
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert!(
        !after_seek.is_empty(),
        "no frames arrived after seeking to 2.0s"
    );
    // A keyframe seek is allowed to land slightly early, never far late.
    let pts = after_seek[0].pts;
    assert!(
        (0.8..=2.5).contains(&pts),
        "seek landed at {pts}s, expected close to 2.0s"
    );
    engine.stop();
}

/// A container whose timeline does not start at zero — a Blu-ray style transport
/// stream starts at 4200 seconds — must still seek where the user asked.
///
/// The bug this guards: playback time was handed to the demuxer as if it were
/// container time, so the seek landed *before* the point that was asked for,
/// every frame that then came out was still behind the target, and the picture
/// never came back. The fixture is a four-second transport stream with an
/// `-output_ts_offset` of 4200s, which is exactly that shape.
///
/// It also pins down what the frames are stamped with after a seek. The frames
/// of a transport stream carry no usable timestamp of their own, and the engine
/// used to invent one — counting 1/fps on from the position the user asked for.
/// That reads well in this test but is what took the player apart in practice:
/// the invented time is offset from the real one by however far the keyframe
/// before the target was, so the last frames of the file end up stamped past
/// its own end, nothing in the queue is ever due again, the picture freezes on
/// the old frame and — with the demuxer blocked handing packets to a decoder
/// that never comes back for them — the end of the file is never noticed and the
/// clock runs on past the duration.
#[test]
fn seeking_works_when_the_container_timeline_starts_late() {
    let Some(path) = fixture("offset.ts") else {
        return;
    };
    // The engine explains a failed seek through `log`; without a logger the test
    // only sees "no frame arrived".
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .try_init();
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));
    assert!(info.duration > 3.0, "got duration {}", info.duration);
    assert!(
        info.start_time > 3600.0,
        "the fixture must start late, got {}",
        info.start_time
    );

    assert!(
        !collect_frames(&engine, 2, Duration::from_secs(10)).is_empty(),
        "playback must start before the seek"
    );

    engine.seek(2.0);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut after: Vec<Arc<mvp_core::VideoFrame>> = Vec::new();
    while Instant::now() < deadline && after.is_empty() {
        if let Some(frame) = engine.take_frame(engine.display_position()) {
            after.push(frame);
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let Some(frame) = after.first() else {
        let snapshot = engine.snapshot_state();
        panic!(
            "no frame arrived after seeking to 2.0s in a container that starts at {}s \
             (state {:?}, position {:.2}s, queued {}, decoded {}, dropped {})",
            info.start_time,
            snapshot.state,
            snapshot.position,
            snapshot.queued_frames,
            snapshot.decoded_frames,
            snapshot.dropped_frames
        );
    };
    // Where the seek *lands* is the container's business: a transport stream
    // seeks by binary search over the byte stream and answers on the next
    // keyframe, a second away in this fixture, so the frame is stamped with the
    // time it really has rather than with the position that was asked for. What
    // the engine guarantees is that the picture never comes back *before* that
    // position — the frames in front of the target are dropped, which is what
    // this lower bound pins down — and that it comes back inside the media at
    // all, which is what the offset translation is for.
    assert!(
        (1.98..=info.duration).contains(&frame.pts),
        "the seek landed at {}s in a {}s file that starts at {}s, expected at or \
         after the requested 2.0s",
        frame.pts,
        info.duration,
        info.start_time
    );
    engine.stop();
}

#[test]
fn pause_freezes_the_clock_and_play_resumes_it() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    wait_for_open(&engine, Duration::from_secs(15));
    collect_frames(&engine, 2, Duration::from_secs(10));

    engine.pause();
    assert_eq!(engine.state(), PlaybackState::Paused);
    std::thread::sleep(Duration::from_millis(60));
    let frozen = engine.position();
    std::thread::sleep(Duration::from_millis(120));
    let still = engine.position();
    assert!(
        (still - frozen).abs() < 0.05,
        "a paused clock moved from {frozen} to {still}"
    );

    engine.play();
    assert_eq!(engine.state(), PlaybackState::Playing);
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        engine.position() > still,
        "the clock did not resume: {} vs {still}",
        engine.position()
    );
    engine.stop();
}

#[test]
fn a_decoded_frame_can_be_written_as_a_valid_png() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    wait_for_open(&engine, Duration::from_secs(15));
    let frames = collect_frames(&engine, 2, Duration::from_secs(10));
    let frame = frames.last().expect("at least one decoded frame");

    // The engine hands out the only reference to a frame, so the snapshot is the
    // caller's job — this is exactly what the player's snapshot command does.
    let target = std::env::temp_dir().join("mvp_core_snapshot_test.png");
    let _ = std::fs::remove_file(&target);
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.data.clone())
        .expect("the frame buffer must be complete");
    image.save(&target).expect("snapshot");

    let bytes = std::fs::read(&target).expect("the snapshot file must exist");
    assert!(bytes.len() > 100, "the PNG looks empty");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG file");
    let decoded = image::load_from_memory(&bytes).expect("the PNG must decode");
    assert_eq!(decoded.width(), 320);
    assert_eq!(decoded.height(), 240);

    // A blank frame would compress to almost nothing; a real one carries detail.
    assert!(
        bytes.len() > 2_000,
        "the snapshot looks like a blank frame ({} bytes)",
        bytes.len()
    );
    let _ = std::fs::remove_file(&target);
    engine.stop();
}

#[test]
fn embedded_text_subtitles_are_decoded_from_the_container() {
    let Some(path) = fixture("with_subs.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));

    assert!(
        !info.subtitles.is_empty(),
        "the fixture must carry an embedded subtitle track"
    );
    let track = &info.subtitles[0];
    assert!(
        track.is_text,
        "mov_text must be reported as a text subtitle, got {}",
        track.codec
    );
    engine.set_subtitle_track(Some(track.index));

    // Subtitle packets are decoded by the demuxer as it reads ahead, so the
    // cues show up asynchronously.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut cues: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        while engine.poll_event().is_some() {}
        if let Some(subtitle) = engine.subtitle() {
            if !subtitle.is_empty() {
                cues = subtitle.cues.iter().map(|c| c.text.clone()).collect();
                break;
            }
        }
        // Keep the pipeline moving so the demuxer is not throttled by a full
        // frame queue.
        let _ = engine.take_frame(engine.display_position());
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(!cues.is_empty(), "no embedded subtitle cues were decoded");
    let joined = cues.join("\n");
    assert!(
        joined.contains("内嵌字幕") || joined.contains("Embedded"),
        "unexpected embedded subtitle text: {joined}"
    );
    // The decoded cues must also be findable by time, which is what the
    // renderer does every frame.
    let subtitle = engine.subtitle().expect("subtitle");
    assert!(
        subtitle.active_at(0.5).is_some(),
        "the first cue covers 0.2s–1.2s and must be active at 0.5s"
    );
    engine.stop();
}

/// "Show subtitles when the file has them" must mean the file's own default
/// track, not "do nothing until the user finds the track menu".
///
/// The interface opens a file with [`Engine::set_subtitle_track_auto`]; this is
/// the engine side of that contract. It used to be impossible: the player set
/// the selector to "off" on every open, and because a selector of "auto" was
/// reported back as "off", the track pickers showed "subtitles disabled" while
/// the file was displaying them.
#[test]
fn the_default_embedded_subtitle_track_needs_no_choice() {
    let Some(path) = fixture("with_subs.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.set_subtitle_track_auto();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));

    let expected = info
        .subtitles
        .iter()
        .find(|stream| stream.is_text)
        .expect("the fixture carries a text subtitle track")
        .index;
    assert_eq!(
        engine.subtitle_track(),
        Some(expected),
        "with no explicit choice the file's own default track must be reported as in use"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut decoded = false;
    while Instant::now() < deadline {
        while engine.poll_event().is_some() {}
        if engine.subtitle().is_some_and(|s| !s.is_empty()) {
            decoded = true;
            break;
        }
        let _ = engine.take_frame(engine.display_position());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(decoded, "the default track's cues were never decoded");

    // Turning them off is reported as "off" — the other half of keeping "auto"
    // and "off" apart.
    engine.set_subtitle_track(None);
    assert_eq!(engine.subtitle_track(), None);
    assert!(engine.subtitle().is_none(), "the cues must be dropped too");
    engine.stop();
}

#[test]
fn stopping_returns_the_engine_to_idle() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    wait_for_open(&engine, Duration::from_secs(15));
    engine.stop();
    assert_eq!(engine.state(), PlaybackState::Idle);
    // Nothing may be handed out once the engine has been stopped.
    assert!(engine.take_frame(0.0).is_none());
    // A second stop must be harmless.
    engine.stop();
}

/// Playing a file to its end and pressing play again must **replay** it.
///
/// The bug this guards: reaching the end used to tear the decode pipeline down,
/// so `play` had nothing left to play but still restarted the clock. The
/// position then climbed past the media's duration and the file carried on
/// "playing" long after its last frame.
#[test]
fn play_after_the_end_replays_from_the_start() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.set_target_size(320, 240);
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));

    // Run the file to its end. Frames have to be taken as they become due: that
    // is what a player does, and what keeps the decoder unblocked.
    fn wait_for_end(engine: &Engine) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while engine.state() != PlaybackState::Ended && Instant::now() < deadline {
            while engine.poll_event().is_some() {}
            let _ = engine.take_frame(engine.display_position());
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    wait_for_end(&engine);
    assert_eq!(
        engine.state(),
        PlaybackState::Ended,
        "the fixture must play through to its end"
    );
    assert!(
        engine.position() <= info.duration + 0.5,
        "the clock ran past the media before the end was even reported: {} > {}",
        engine.position(),
        info.duration
    );

    // Pressing play is a replay, not a resume of a clock that has nothing left.
    engine.play();
    assert_eq!(engine.state(), PlaybackState::Playing);
    assert!(
        engine.position() < 1.0,
        "play after the end must start at the beginning, got {}",
        engine.position()
    );

    // It really is playing again: frames flow, and the clock stays inside the
    // media instead of running away past its duration.
    let frames = collect_frames(&engine, 3, Duration::from_secs(5));
    assert!(
        !frames.is_empty(),
        "a replay must decode frames again, not just restart the clock"
    );
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        engine.position() < info.duration,
        "after a replay the clock was past the media: {} >= {}",
        engine.position(),
        info.duration
    );

    // A second end must park the same way, or the next "play" would be the old
    // bug all over again. Seeking near the end is the quick way back there.
    engine.seek(info.duration - 0.4);
    wait_for_end(&engine);
    assert_eq!(engine.state(), PlaybackState::Ended);
    engine.play();
    assert!(
        engine.position() < 1.0,
        "the second play after the end must replay too, got {}",
        engine.position()
    );
    engine.stop();
}

/// The position the interface is *shown* never passes the end of the media.
///
/// The master clock is a wall clock, so nothing ties it to the length of the
/// file: a seek re-anchors it forward, and it keeps running while the demuxer
/// works out where that seek landed. Clicking the seek bar faster than the
/// demuxer can answer therefore let the position climb past the duration — a
/// "0:11" under a "0:09" total, with the progress bar pinned full — and a
/// positive audio delay put a position past the end of *any* file on screen the
/// same way, because the delay is added to the position the OSD prints.
#[test]
fn the_reported_position_never_passes_the_end_of_the_media() {
    let Some(path) = fixture("tiny.mp4") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));
    let duration = info.duration;
    assert!(duration > 0.0, "got duration {duration}");

    // A delay that, unclamped, pushes the readout well past the end of the file.
    engine.set_audio_delay(1.5);

    // Click the last of the bar over and over, consuming frames the way the
    // interface does every frame. Every one of these seeks lands on a file that
    // has already ended, which is the path a click on a finished file takes.
    let mut worst = 0.0f64;
    for _ in 0..120 {
        engine.seek(duration - 0.05);
        std::thread::sleep(Duration::from_millis(25));
        let shown = engine.display_position();
        worst = worst.max(shown);
        let _ = engine.take_frame(shown);
    }

    // Then let it run into the end with nothing seeking at all: a clock left
    // running past the media is exactly what this guards against.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let shown = engine.display_position();
        worst = worst.max(shown);
        let _ = engine.take_frame(shown);
        std::thread::sleep(Duration::from_millis(10));
    }

    engine.stop();
    assert!(
        worst <= duration + 1e-6,
        "the reported position reached {worst:.3}s in a {duration:.3}s file"
    );
}

#[test]
fn an_audio_only_file_opens_without_a_video_stream() {
    let Some(path) = fixture("tiny.mp3") else {
        return;
    };
    let engine = silent_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(15));
    assert!(!info.audio.is_empty(), "the MP3 must report an audio stream");
    assert!(info.video.is_empty(), "an MP3 has no video");
    assert!(info.has_audio());
    engine.stop();
}

#[test]
fn opening_a_missing_file_reports_an_error_instead_of_panicking() {
    let engine = silent_engine();
    let missing = std::env::temp_dir().join("mvp-core-definitely-missing-file.mkv");
    let _ = std::fs::remove_file(&missing);
    engine.open(MediaSource::Path(missing)).expect("open is async");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_error = false;
    while Instant::now() < deadline {
        if let Some(EngineEvent::Error(_)) = engine.poll_event() {
            saw_error = true;
            break;
        }
        if let PlaybackState::Error(_) = engine.state() {
            saw_error = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(saw_error, "a missing file must surface an error state");
    engine.stop();
}

#[test]
fn garbage_input_does_not_bring_the_engine_down() {
    let path = std::env::temp_dir().join("mvp-core-garbage.bin");
    std::fs::write(&path, vec![0x41u8; 4096]).expect("write the fixture");
    let engine = silent_engine();
    let _ = engine.open(MediaSource::Path(path.clone()));
    std::thread::sleep(Duration::from_millis(400));
    // Whatever happened, the engine must still be usable afterwards.
    engine.stop();
    // Drain the error the garbage file produced before reusing the engine.
    while engine.poll_event().is_some() {}
    if let Some(real) = fixture("tiny.mp4") {
        engine.open(MediaSource::Path(real)).expect("reopen");
        let info = wait_for_open(&engine, Duration::from_secs(15));
        assert!(!info.video.is_empty());
        engine.stop();
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Images — no engine involved, but this is where the real files are.
// ---------------------------------------------------------------------------

#[test]
fn still_images_load_with_the_expected_geometry() {
    let Some(path) = fixture("tiny.png") else {
        return;
    };
    let doc = mvp_core::image_view::load(&path).expect("load the PNG");
    assert_eq!(doc.width, 320);
    assert_eq!(doc.height, 240);
    assert!(!doc.is_animated, "a PNG is not animated");
    assert_eq!(doc.frames.len(), 1);
    assert_eq!(doc.frames[0].data.len(), 320 * 240 * 4);
    assert!(doc.format.contains("PNG"), "got format {}", doc.format);
}

#[test]
fn animated_gifs_keep_every_frame() {
    let Some(path) = fixture("tiny.gif") else {
        return;
    };
    let doc = mvp_core::image_view::load(&path).expect("load the GIF");
    assert!(doc.is_animated, "a looping GIF must be animated");
    assert!(
        doc.frames.len() >= 2,
        "expected several frames, got {}",
        doc.frames.len()
    );
    assert!(doc.animation_duration_ms() > 0);
    for frame in &doc.frames {
        assert_eq!(frame.data.len(), doc.width as usize * doc.height as usize * 4);
    }
}

#[test]
fn the_image_viewer_zooms_and_rotates() {
    let Some(path) = fixture("tiny.png") else {
        return;
    };
    let mut view = ImageView::new();
    view.open(&path).expect("open");
    assert!(view.is_loaded());
    assert_eq!(view.dimensions(), Some((320, 240)));

    view.zoom_by(4.0, Some((200.0, 200.0)));
    assert!(view.zoom > 1.0);
    view.rotate_cw();
    assert_eq!(view.rotation, 90);
    view.toggle_flip_h();
    assert!(view.flip_h);
    view.reset_view();
    assert_eq!(view.rotation, 0);
    assert!(!view.flip_h);
    assert!((view.zoom - 1.0).abs() < 1e-6);
}

#[test]
fn a_subtitle_side_car_parses_and_lines_up_with_the_clock() {
    let subtitle = mvp_core::subtitle::Subtitle::parse(
        b"1\n00:00:01,000 --> 00:00:03,000\nHello\n\n2\n00:00:04,000 --> 00:00:06,000\nWorld\n",
    );
    assert_eq!(subtitle.len(), 2);
    assert!(subtitle.active_at(2.0).is_some());
    let first = subtitle.active_at(2.0).unwrap();
    assert_eq!(first.text.trim(), "Hello");
    assert!(subtitle.active_at(3.0).is_none(), "cues are half-open");
    assert_eq!(subtitle.active_at(5.0).unwrap().text.trim(), "World");
}

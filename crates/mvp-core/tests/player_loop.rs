//! Replays the exact call sequence `PlayerApp::update` performs, headlessly.
//!
//! The engine is unit tested on its own, but the *interface* drives it in a
//! specific order (target size, snapshot, position, take frame, toggle pause)
//! and with the audio device enabled — neither of which the other tests cover.
//! This file exists because a bug that only shows up in that combination is
//! exactly the kind of bug that reaches a user.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource};
use mvp_core::Engine;

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join(name)
}

/// The engine as the player configures it: audio on, default queue budgets.
///
/// The device is opened exactly as the interface opens it — that is part of what
/// this file tests — but the gain is zeroed so a test run never makes noise. The
/// pipeline, the device timing and every counter behave identically.
fn player_engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).expect("engine construction");
    engine.set_volume(0.0);
    engine
}

#[test]
fn the_player_call_order_produces_frames() {
    let path = testdata("tiny.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let engine = player_engine();

    // The interface disables embedded subtitles and audio *before* opening, and
    // that is exactly what used to wedge the demuxer: the track-change check
    // compared the request against the stream chosen at open time, so "off"
    // never matched and the demuxer spun without ever reading a packet.
    engine.set_subtitle_track(None);
    engine.set_audio_track(None);

    engine.open(MediaSource::Path(path)).expect("open");

    // Wait for the probe, exactly as `handle_engine_events` does.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(_)) = engine.poll_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // One second of a 60 Hz interface loop.
    let mut frames = 0usize;
    let mut unwrapped = 0usize;
    for _ in 0..120 {
        engine.set_target_size(980, 551);
        let _ = engine.snapshot_state();

        let now = engine.display_position();
        if let Some(frame) = engine.take_frame(now) {
            frames += 1;
            // This is the operation `update_frame_texture` performs. It must
            // succeed: if the engine keeps its own reference alive on purpose
            // (for snapshots), the interface can never move the pixels out.
            if Arc::try_unwrap(frame).is_ok() {
                unwrapped += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    assert!(frames > 5, "the engine produced no frames at all");
    assert_eq!(
        frames, unwrapped,
        "every frame handed to the interface must be uniquely owned, \
         otherwise the texture upload silently gives up"
    );
    engine.stop();
}

#[test]
fn the_picture_never_freezes_once_the_frame_queue_fills() {
    let path = testdata("scene.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let engine = player_engine();
    engine.set_subtitle_track(None);
    engine.open(MediaSource::Path(path)).expect("open");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(_)) = engine.poll_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    engine.set_target_size(980, 551);

    // Let the decoder prime and fill its frame queue. That is the moment the
    // regression struck: the queue used to evict its *oldest* frame to make
    // room for the newest, so once it was full it held nothing but timestamps
    // in the future. `take_frame` only returns frames that are due, so it then
    // returned `None` for the rest of the file — a picture frozen on the first
    // few frames while `decoded_frames` kept climbing and `dropped_frames`
    // stayed at zero, because the eviction was counted nowhere.
    std::thread::sleep(Duration::from_secs(4));

    let mut late = 0usize;
    let window = Instant::now();
    while window.elapsed() < Duration::from_secs(3) {
        if engine.take_frame(engine.display_position()).is_some() {
            late += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = engine.snapshot_state();
    assert!(
        late > 10,
        "the picture froze after the queue filled: only {late} frames arrived in the \
         last 3 s (decoded={}, dropped={}, queued={})",
        snapshot.decoded_frames,
        snapshot.dropped_frames,
        snapshot.queued_frames
    );
    engine.stop();
}

#[test]
fn audio_keeps_reaching_the_device() {
    if mvp_core::audio::default_output_device_name().is_none() {
        eprintln!("skipping: no audio output device");
        return;
    }
    let path = testdata("scene.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    // Silent: the device is opened and driven exactly as in the player, but the
    // gain is zero, so this asserts on the queue and the underrun counter rather
    // than on anything a human could hear.
    let engine = player_engine();
    engine.set_subtitle_track(None);
    engine.open(MediaSource::Path(path)).expect("open");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(_)) = engine.poll_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    std::thread::sleep(Duration::from_millis(1500));
    let before = engine.snapshot_state();
    std::thread::sleep(Duration::from_secs(2));
    let after = engine.snapshot_state();

    // A stream whose rate differs from the device's used to be truncated by the
    // resampler: 44.1 kHz into a 192 kHz device delivered 5 ms of audio per
    // 23 ms of media, the queue in front of the device stayed empty and the
    // device underran roughly 43 times a second — which is what "no sound at
    // all" sounded like.
    assert!(
        after.audio_queue_seconds > 0.25,
        "the device queue is nearly empty ({:.3}s of audio buffered): the sound card is \
         being starved",
        after.audio_queue_seconds
    );
    let underruns = after.underruns.saturating_sub(before.underruns);
    assert!(
        underruns <= 5,
        "the device ran dry {underruns} times in two seconds"
    );

    engine.stop();
}

#[test]
fn a_disabled_track_does_not_stall_the_demuxer() {
    let path = testdata("tiny.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    // Every combination of "off before open" and "off after open" must still
    // read packets. A regression here shows up as a completely black player,
    // which is far too easy to miss without a test.
    for subtitle in [None, Some(0usize)] {
        for audio in [None, Some(0usize)] {
            let engine = player_engine();
            engine.set_subtitle_track(subtitle);
            engine.set_audio_track(audio);
            engine.open(MediaSource::Path(path.clone())).expect("open");

            let deadline = Instant::now() + Duration::from_secs(10);
            let mut decoded = 0u64;
            while Instant::now() < deadline {
                let snapshot = engine.snapshot_state();
                decoded = snapshot.decoded_frames;
                if decoded > 3 {
                    break;
                }
                let _ = engine.take_frame(engine.display_position());
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                decoded > 3,
                "the demuxer stalled with subtitle={subtitle:?} audio={audio:?} \
                 (decoded only {decoded} frames)"
            );
            engine.stop();
        }
    }
}

#[test]
fn toggling_playback_repeatedly_is_safe() {
    let path = testdata("tiny.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let engine = player_engine();
    engine.open(MediaSource::Path(path)).expect("open");
    std::thread::sleep(Duration::from_millis(400));

    // Hammer the transport controls the way an impatient user would.
    for _ in 0..50 {
        engine.toggle_pause();
        let _ = engine.snapshot_state();
        let _ = engine.display_position();
        std::thread::sleep(Duration::from_millis(4));
    }

    // Seeking while paused is the other common interaction.
    engine.pause();
    for position in [0.5, 1.5, 0.2, 2.0, 0.0] {
        engine.seek(position);
        std::thread::sleep(Duration::from_millis(80));
        let _ = engine.snapshot_state();
    }
    engine.play();
    std::thread::sleep(Duration::from_millis(200));
    engine.stop();
}

#[test]
fn the_position_tracks_the_wall_clock_during_steady_playback() {
    let path = testdata("scene.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let engine = player_engine();
    engine.set_subtitle_track(None);
    engine.open(MediaSource::Path(path)).expect("open");

    // Wait for the probe so the target size is known.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(_)) = engine.poll_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    engine.set_target_size(640, 360);

    // Let the pipeline prime, then measure over a known window.
    std::thread::sleep(Duration::from_millis(1500));
    let baseline = engine.display_position();
    let baseline_at = Instant::now();

    let mut frames = 0usize;
    let mut last_pts = -1.0f64;
    while baseline_at.elapsed() < Duration::from_secs(5) {
        if let Some(frame) = engine.take_frame(engine.display_position()) {
            frames += 1;
            assert!(
                frame.pts >= last_pts,
                "frame timestamps went backwards: {} then {}",
                last_pts,
                frame.pts
            );
            last_pts = frame.pts;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let elapsed = baseline_at.elapsed().as_secs_f64();
    let advanced = engine.display_position() - baseline;
    let snapshot = engine.snapshot_state();

    assert!(frames > 30, "only {frames} frames arrived in {elapsed:.1}s");
    // The clock must run at real time. A negative or wildly large advance is the
    // signature of the audio clock being anchored to the *buffered* audio rather
    // than to the sample the device is playing.
    assert!(
        (advanced - elapsed).abs() < 0.35,
        "the clock advanced {advanced:.3}s over {elapsed:.3}s of wall time"
    );
    assert!(
        snapshot.dropped_frames < 15,
        "{} frames were dropped during steady playback",
        snapshot.dropped_frames
    );
    assert!(
        snapshot.decoded_frames > 30,
        "only {} frames were decoded",
        snapshot.decoded_frames
    );
    engine.stop();
}

#[test]
fn seeking_during_playback_keeps_the_clock_consistent() {
    let path = testdata("scene.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .try_init();

    let engine = player_engine();
    engine.set_subtitle_track(None);
    engine.open(MediaSource::Path(path)).expect("open");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(_)) = engine.poll_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    engine.set_target_size(640, 360);
    std::thread::sleep(Duration::from_millis(800));

    for target in [12.0f64, 3.0, 18.0, 8.0] {
        engine.seek(target);
        std::thread::sleep(Duration::from_millis(700));
        let position = engine.display_position();
        assert!(
            (position - target).abs() < 2.5,
            "after seeking to {target}s the clock reads {position:.3}s"
        );
        // Frames must keep flowing after a seek.
        let mut got = false;
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if engine.take_frame(engine.display_position()).is_some() {
                got = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(got, "no frame arrived after seeking to {target}s");
    }
    engine.stop();
}

#[test]
fn the_video_target_size_converges_to_something_usable() {    let path = testdata("tiny.mp4");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let engine = player_engine();
    engine.open(MediaSource::Path(path)).expect("open");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut info = None;
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(opened)) = engine.poll_event() {
            info = Some(opened);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let info = info.expect("the file must open");
    let (source_width, source_height) = info
        .primary_video()
        .map(|v| (v.width, v.height))
        .expect("a video stream");

    // The interface derives the decode target from the *probe*, not from the
    // already-scaled frame — deriving it from the frame would ratchet the size
    // down to a thumbnail on the very first frame.
    let source = (source_width, source_height);
    let viewport = (980u32, 551u32);
    let target = mvp_core::util::even(mvp_core::util::fit_inside(source, viewport));
    assert_eq!(
        target,
        (320, 240),
        "a 320x240 source shown in a 980x551 viewport must decode at its native size"
    );

    // Feeding the *scaled* size back in must not shrink it further.
    let second = mvp_core::util::even(mvp_core::util::fit_inside(target, viewport));
    assert_eq!(second, target, "the target size must be a fixed point");
    engine.stop();
}

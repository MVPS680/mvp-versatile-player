//! A storm of seeks must leave the pipeline alive.
//!
//! Dragging the seek bar quickly asks for a new position while the previous seek
//! may still be inside FFmpeg. The interrupt callback then makes that read fail
//! with `AVERROR_EXIT`, which is not a broken file: the request that caused the
//! interruption is the one that should be served. Tearing the demuxer down on
//! that error (`?`, as this once did) left the player parked on an error banner
//! with a dead pipeline — nothing played, and no later seek did anything — from
//! nothing worse than scrubbing the bar, and fast scrubbing is where a cancelled
//! seek is *expected*.
//!
//! The targets include both ends of the media and values the interface cannot
//! normally produce (`f64::MAX`, `NaN`). A drag past either end is clamped, and
//! the clamp must never be what decides whether the engine survives.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource};
use mvp_core::{Engine, PlaybackState};

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join(name)
}

/// Everything the engine reported that was not routine.
///
/// A worker panic is contained by the engine instead of ending the process, so
/// it only ever shows up as an error state or an error event; collecting them is
/// what lets a test see a panic at all.
fn collect_errors(engine: &Engine, seen: &mut Vec<String>) {
    while let Some(event) = engine.poll_event() {
        match event {
            EngineEvent::Error(message) => seen.push(format!("event: {message}")),
            EngineEvent::StateChanged(PlaybackState::Error(message)) => {
                seen.push(format!("state: {message}"))
            }
            _ => {}
        }
    }
    if let PlaybackState::Error(message) = engine.state() {
        seen.push(format!("state (polled): {message}"));
    }
}

#[test]
fn a_seek_storm_on_an_audio_only_file_keeps_playing() {
    // The device is opened exactly as the player opens it — that is part of what
    // this test is about — but silent, so a test run makes no noise.
    if mvp_core::audio::default_output_device_name().is_none() {
        eprintln!("skipping: no audio output device");
        return;
    }
    let path = testdata("tiny.mp3");
    if !path.exists() {
        eprintln!("skipping: {} is missing", path.display());
        return;
    }

    let engine = Engine::new(EngineConfig::default()).expect("engine construction");
    engine.set_volume(0.0);
    engine.open(MediaSource::Path(path)).expect("open");

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(_)) = engine.poll_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let duration = engine.duration();
    assert!(duration > 0.0, "the probe never reported a duration");

    engine.play();

    let mut problems: Vec<String> = Vec::new();
    for step in 0..4000usize {
        let target = match step % 8 {
            0 => 0.0,
            1 => duration,
            2 => duration - 0.001,
            3 => duration / 2.0,
            4 => f64::MAX,
            5 => -1.0,
            6 => f64::NAN,
            _ => duration * ((step * 37) % 100) as f64 / 100.0,
        };
        engine.seek(target);
        // The calls `PlayerApp::update` makes every frame, in its order.
        let _ = engine.snapshot_state();
        let _ = engine.take_frame(engine.display_position());
        let _ = engine.time_until_next_frame();
        if step % 97 == 0 {
            if engine.is_playing() {
                engine.pause();
            } else {
                engine.play();
            }
        }
        if step % 16 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        collect_errors(&engine, &mut problems);
    }

    // The engine must still *answer*: a demuxer that died somewhere in the storm
    // shows up here as a pipeline that never comes back to life.
    engine.seek(0.0);
    let revive = Instant::now() + Duration::from_secs(2);
    let mut advanced = false;
    while Instant::now() < revive {
        if engine.display_position() > 0.05 {
            advanced = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    engine.stop();
    problems.dedup();
    for problem in &problems {
        eprintln!("!! {problem}");
    }
    assert!(
        problems.is_empty(),
        "the seek storm produced {} engine errors",
        problems.len()
    );
    assert!(
        advanced,
        "playback never resumed after the seek storm: the demuxer is gone"
    );
}

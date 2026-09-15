//! Drive one file through the engine and print what happens at every step.
//!
//! This exists because "it hangs when I open my file" is not something a unit
//! test can explain: the engine runs four threads, and the interesting question
//! is always *which* stage stopped making progress. This tool answers that,
//! with timestamps, for any file — a Blu-ray transport stream, a damaged
//! download, a network URL.
//!
//! ```text
//! cargo run -p mvp-core --example seek_probe -- "C:\path\to\file.m2ts" 60
//! cargo run -p mvp-core --example seek_probe -- https://example.com/stream.m3u8
//! ```
//!
//! The second argument is the position to seek to, in seconds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource, PlaybackState};
use mvp_core::{Engine, MediaInfo};

/// Print progress on **stderr**, which is unbuffered: a probe that dies mid-way
/// has to leave its trail behind, and a block-buffered stdout would swallow it.
fn stamp(started: Instant, what: &str) {
    eprintln!("[{:>7.3}s] {what}", started.elapsed().as_secs_f64());
}

fn main() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .filter_module("mvp_core", log::LevelFilter::Debug)
        .try_init();

    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: seek_probe <file-or-url> [seek-seconds] [--hardware|--software]");
        std::process::exit(2);
    };
    let target = args
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(2.0);

    let started = Instant::now();
    let engine = Engine::new(EngineConfig {
        audio_enabled: false,
        autoplay: true,
        ..EngineConfig::default()
    })
    .expect("engine construction");

    stamp(started, &format!("opening {path}"));
    engine
        .open(MediaSource::new(path))
        .expect("the engine refused to open");

    let info = wait_for_open(&engine, started);
    match info.primary_video() {
        Some(video) => println!(
            "          {}x{} {}, {:.3} fps, pix {} — {}",
            video.width, video.height, video.codec, video.fps, video.pixel_format, video.profile
        ),
        None => println!("          no video stream"),
    }
    println!("          duration {:.2}s, start {:.2}s", info.duration, info.start_time);
    println!("          dynamic range: {:?}", hdr_summary(&info));

    stamp(started, "decoding the first frames");
    let frames = collect(&engine, 3, Duration::from_secs(20));
    stamp(started, &format!("got {} frames", frames.len()));
    if let Some(last) = frames.last() {
        println!("          first {:.3}s … last {:.3}s", frames[0].pts, last.pts);
    }

    stamp(started, &format!("seeking to {target:.2}s"));
    engine.seek(target);
    let after = collect(&engine, 3, Duration::from_secs(20));
    stamp(started, &format!("got {} frames after the seek", after.len()));
    for frame in &after {
        eprintln!("          frame at {:.3}s", frame.pts);
    }

    eprintln!("          position {:.3}s", engine.display_position());
    eprintln!("          duration {:.3}s", engine.duration());
    eprintln!("          info present: {}", engine.info().is_some());
    eprintln!("          state {:?}", engine.state());
    eprintln!("          audio track {:?}", engine.audio_track());
    eprintln!("          time until next frame {:?}", engine.time_until_next_frame());
    eprintln!("          taking a snapshot…");
    let snapshot = engine.snapshot_state();
    eprintln!(
        "          state {:?}, position {:.3}s, queued {}, decoded {}, dropped {}, underruns {}",
        snapshot.state,
        snapshot.position,
        snapshot.queued_frames,
        snapshot.decoded_frames,
        snapshot.dropped_frames,
        snapshot.underruns
    );

    stamp(started, "stopping");
    engine.stop();
    stamp(started, "stopped");
}

fn hdr_summary(info: &MediaInfo) -> String {
    match info.primary_video() {
        Some(video) => video.hdr.label(),
        None => "n/a".to_string(),
    }
}

fn wait_for_open(engine: &Engine, started: Instant) -> Arc<MediaInfo> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match engine.poll_event() {
            Some(EngineEvent::Opened(info)) => {
                stamp(started, "opened");
                return info;
            }
            Some(EngineEvent::Error(message)) => {
                stamp(started, &format!("engine error: {message}"));
                std::process::exit(1);
            }
            Some(EngineEvent::Ended) => println!("          [event] ended"),
            Some(EngineEvent::StateChanged(state)) => {
                if state == PlaybackState::Error(String::new()) {
                    println!("          [event] error state");
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    stamp(started, "the file did not open within 30s");
    std::process::exit(1);
}

fn collect(engine: &Engine, wanted: usize, timeout: Duration) -> Vec<Arc<mvp_core::VideoFrame>> {
    let mut frames = Vec::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && frames.len() < wanted {
        if let Some(frame) = engine.take_frame(engine.display_position()) {
            frames.push(frame);
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    frames
}

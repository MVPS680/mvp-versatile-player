//! Measure what the engine actually sustains on one file, in real time.
//!
//! `seek_probe` answers "where does it hang"; this answers "how many frames per
//! second reach the screen and what did each one cost". It drives the engine
//! exactly the way the interface does — pull a frame when one is due, sleep
//! until the next screen refresh otherwise — and prints the pipeline counters
//! the statistics panel shows.
//!
//! ```text
//! cargo run -p mvp-core --release --example fps_probe -- tmp\uhd120.mp4 5 60
//! ```
//!
//! Arguments: `<file-or-url> [seconds] [screen-hz] [WxH] [--software]`.
//!
//! `WxH` mimics the interface's `set_target_size`: the decoder is asked to
//! convert straight to the size the window displays, which is where the
//! conversion cost is decided. Left out, the source size is used.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource};
use mvp_core::Engine;

fn main() {
    let _ = env_logger::builder().is_test(false).try_init();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let software = args.iter().any(|a| a == "--software");
    args.retain(|a| a != "--software");
    let Some(path) = args.first().cloned() else {
        eprintln!("usage: fps_probe <file-or-url> [seconds] [screen-hz] [WxH] [--software]");
        std::process::exit(2);
    };
    let seconds = args
        .get(1)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(5.0);
    let hz = args
        .get(2)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(60.0);
    let interval = Duration::from_secs_f64(1.0 / hz);
    let target = args.get(3).and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
    });

    let engine = Engine::new(EngineConfig {
        audio_enabled: false,
        autoplay: true,
        hardware_decoding: !software,
        ..EngineConfig::default()
    })
    .expect("engine construction");
    engine
        .open(MediaSource::new(path.clone()))
        .expect("the engine refused to open");
    if let Some((w, h)) = target {
        engine.set_target_size(w, h);
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(EngineEvent::Opened(info)) = engine.poll_event() {
            match info.primary_video() {
                Some(v) => println!(
                    "source: {}x{} {} {:.3} fps hdr={}",
                    v.width,
                    v.height,
                    v.codec,
                    v.fps,
                    v.hdr.label()
                ),
                None => println!("source: no video stream"),
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // Simulate the interface: a `take_frame` per screen refresh.
    let run = Duration::from_secs_f64(seconds);
    let started = Instant::now();
    let mut next_tick = started;
    let mut presented = 0u64;
    let mut ticks = 0u64;
    let mut queue_samples: Vec<usize> = Vec::new();
    let mut last_sample = started;
    let mut first_pts: Option<f64> = None;
    let mut last_pts = 0.0f64;

    while started.elapsed() < run {
        let now = Instant::now();
        if now < next_tick {
            std::thread::sleep(next_tick - now);
        }
        next_tick += interval;
        if next_tick < Instant::now() {
            next_tick = Instant::now();
        }
        ticks += 1;
        let position = engine.display_position();
        if let Some(frame) = engine.take_frame(position) {
            presented += 1;
            if first_pts.is_none() {
                first_pts = Some(frame.pts);
            }
            last_pts = frame.pts;
        }
        if last_sample.elapsed() >= Duration::from_millis(200) {
            last_sample = Instant::now();
            let snapshot = engine.snapshot_state();
            queue_samples.push(snapshot.queued_frames);
            println!(
                "  t={:>5.2}s pos={:>6.2}s queued={:>3} audioq={:>6.3}s dec={:>5} drop={:>5} \
                 presdrop={:>5} skip={:>4} dl={:>5.1} conv={:>5.1} tone={:>5.1} wait={:>5.1}",
                started.elapsed().as_secs_f64(),
                position,
                snapshot.queued_frames,
                snapshot.audio_queue_seconds,
                snapshot.decoded_frames,
                snapshot.dropped_frames,
                snapshot.presentation_drops,
                snapshot.skipped_frames,
                snapshot.download_ms,
                snapshot.convert_ms,
                snapshot.tone_map_ms,
                snapshot.queue_wait_ms,
            );
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    let snapshot = engine.snapshot_state();
    let max_queue = queue_samples.iter().copied().max().unwrap_or(0);
    let avg_queue: f64 = if queue_samples.is_empty() {
        0.0
    } else {
        queue_samples.iter().sum::<usize>() as f64 / queue_samples.len() as f64
    };
    println!(
        "result: presented {presented} in {:.2}s = {:.1} fps (ticks {ticks} = {:.1} Hz)",
        elapsed,
        presented as f64 / elapsed,
        ticks as f64 / elapsed
    );
    println!(
        "        media advanced {:.2}s → {:.1}x realtime",
        last_pts - first_pts.unwrap_or(0.0),
        (last_pts - first_pts.unwrap_or(0.0)) / elapsed
    );
    println!(
        "        decoded={} dropped={} presentation_drops={} skipped={}",
        snapshot.decoded_frames,
        snapshot.dropped_frames,
        snapshot.presentation_drops,
        snapshot.skipped_frames
    );
    println!(
        "        queue avg={avg_queue:.1} max={max_queue}  download={:.1}ms convert={:.1}ms \
         tone={:.1}ms wait={:.1}ms",
        snapshot.download_ms,
        snapshot.convert_ms,
        snapshot.tone_map_ms,
        snapshot.queue_wait_ms,
    );

    drop(engine);

    // The spare threads only exist while the engine is alive, so hold a flag to
    // keep the optimiser honest about the numbers above.
    static DONE: AtomicBool = AtomicBool::new(false);
    DONE.store(true, Ordering::Relaxed);
    let _ = Arc::new(());
}

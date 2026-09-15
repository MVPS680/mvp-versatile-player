//! Dolby Vision / HDR test material.
//!
//! These tests are opt-in: a real Dolby Vision sample is far too large to keep
//! in `testdata/`, so they read the path from the `MVP_DV_SAMPLE` environment
//! variable and skip when it is unset:
//!
//! ```text
//! set MVP_DV_SAMPLE=C:\path\to\dolby\sample.mkv
//! cargo test -p mvp-core --test dolby_vision -- --nocapture
//! ```
//!
//! What they guard: a Dolby Vision container declares an *enhancement layer*
//! (a second video stream), carries a DOVI configuration record and signals
//! BT.2020 with the PQ transfer function — none of which SDR material exercises.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource, PlaybackState};
use mvp_core::{Engine, MediaInfo};

/// Path of the sample, or `None` when the test should skip.
fn sample() -> Option<PathBuf> {
    let raw = std::env::var_os("MVP_DV_SAMPLE")?;
    let path = PathBuf::from(raw);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: {} does not exist", path.display());
        None
    }
}

fn engine(hardware: bool) -> Engine {
    // The engine explains a stall through `log`; without a logger that
    // explanation is a silent timeout.
    init_logging();
    let config = EngineConfig {
        audio_enabled: false,
        hardware_decoding: hardware,
        autoplay: true,
        ..EngineConfig::default()
    };
    Engine::new(config).expect("engine construction")
}

fn wait_for_open(engine: &Engine, timeout: Duration) -> Arc<MediaInfo> {
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

fn collect_frames(
    engine: &Engine,
    wanted: usize,
    timeout: Duration,
) -> Vec<Arc<mvp_core::VideoFrame>> {
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

/// Run `body` on its own thread and give up on it after `seconds`.
///
/// A hung decoder or a hang inside `Engine::stop` cannot be interrupted from
/// outside, so the sweep below abandons the thread and carries on with the next
/// sample: a test that hangs forever tells you nothing, a test that reports
/// which file hung tells you everything.
///
/// The three outcomes are told apart on purpose. A *panic* (an engine error
/// event, an assertion) drops the sending end of the channel, which is
/// immediately visible; a *hang* only shows up when the deadline passes.
fn within<R: Send + 'static>(
    seconds: u64,
    body: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(body());
    });
    match rx.recv_timeout(Duration::from_secs(seconds)) {
        Ok(value) => Ok(value),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("the thread panicked (its message is above)".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("hung: still running after {seconds}s"))
        }
    }
}

/// Install a logger once, so the engine's own explanation for a stall reaches
/// the test output instead of vanishing.
fn init_logging() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .try_init();
}

/// The probe path: this is what the player runs before the first frame, and it
/// must survive an enhancement layer, a DOVI record and PQ signalling.
#[test]
fn probes_a_dolby_vision_file() {
    let Some(path) = sample() else {
        return;
    };
    let info = mvp_core::info::probe(&path).expect("probe must succeed");
    eprintln!(
        "container={} duration={:.2}s size={} video_streams={} audio_streams={}",
        info.format_name,
        info.duration,
        info.size,
        info.video.len(),
        info.audio.len()
    );
    for v in &info.video {
        eprintln!("  video #{} {} ({}) {}x{} {}fps pix={} profile={} level={}",
            v.index, v.codec, v.codec_long, v.width, v.height, v.fps,
            v.pixel_format, v.profile, v.level);
    }
    assert!(
        info.primary_video().is_some(),
        "a Dolby Vision file still has a base layer"
    );
    for (label, value) in mvp_core::info::info_rows(&info) {
        eprintln!("  row: {label} = {value}");
    }
}

/// Software decoding of the base layer must produce real pixels.
#[test]
fn decodes_a_dolby_vision_file_in_software() {
    let Some(path) = sample() else {
        return;
    };
    let engine = engine(false);
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(20));
    eprintln!("opened: {:.2}s", info.duration);

    let frames = collect_frames(&engine, 5, Duration::from_secs(30));
    assert!(
        !frames.is_empty(),
        "software decoding produced no frame at all (state {:?})",
        engine.state()
    );
    for frame in &frames {
        assert!(frame.is_valid(), "frame without pixels");
        assert!(frame.pts.is_finite(), "bogus timestamp");
    }
    let last = frames.last().unwrap();
    eprintln!(
        "{} frames, first {:.3}s, last {:.3}s, {}x{}",
        frames.len(),
        frames.first().unwrap().pts,
        last.pts,
        last.width,
        last.height
    );
    engine.stop();
}

/// The same file through the hardware path (D3D11VA). Dolby Vision material is
/// 10-bit HEVC, so this is the path a graphics driver sees.
#[test]
fn decodes_a_dolby_vision_file_with_hardware_decoding() {
    let Some(path) = sample() else {
        return;
    };
    let engine = engine(true);
    engine.open(MediaSource::Path(path)).expect("open");
    let _ = wait_for_open(&engine, Duration::from_secs(20));

    let frames = collect_frames(&engine, 5, Duration::from_secs(30));
    assert!(
        !frames.is_empty(),
        "hardware decoding produced no frame at all (state {:?})",
        engine.state()
    );
    for frame in &frames {
        assert!(frame.is_valid(), "frame without pixels");
    }
    eprintln!("hardware path produced {} frames", frames.len());
    engine.stop();
}

/// Seeking must survive the two-stream layout these files have.
#[test]
fn seeks_inside_a_dolby_vision_file() {
    let Some(path) = sample() else {
        return;
    };
    init_logging();
    let engine = engine(false);
    engine.open(MediaSource::Path(path)).expect("open");
    let info = wait_for_open(&engine, Duration::from_secs(20));
    assert!(info.duration > 10.0, "sample is too short to seek in");

    let started = Instant::now();
    let frames = collect_frames(&engine, 2, Duration::from_secs(30));
    assert!(!frames.is_empty(), "no frame before the seek");
    let target = info.duration / 2.0;
    eprintln!(
        "before seek: {} frames in {:.1}s, position {:.2}s, target {:.2}s",
        frames.len(),
        started.elapsed().as_secs_f64(),
        engine.display_position(),
        target
    );

    let seeking = Instant::now();
    engine.seek(target);

    let deadline = Instant::now() + Duration::from_secs(40);
    let mut after: Vec<Arc<mvp_core::VideoFrame>> = Vec::new();
    let mut next_report = Instant::now();
    while Instant::now() < deadline && after.len() < 3 {
        if Instant::now() >= next_report {
            let snapshot = engine.snapshot_state();
            eprintln!(
                "  waiting after {:.1}s: pos {:.2}s, queued {}, decoded {}, dropped {}, state {:?}",
                seeking.elapsed().as_secs_f64(),
                snapshot.position,
                snapshot.queued_frames,
                snapshot.decoded_frames,
                snapshot.dropped_frames,
                snapshot.state
            );
            next_report = Instant::now() + Duration::from_secs(2);
        }
        if let Some(frame) = engine.take_frame(engine.display_position()) {
            if frame.pts > 1.0 {
                eprintln!(
                    "after {:.1}s: frame at {:.2}s",
                    seeking.elapsed().as_secs_f64(),
                    frame.pts
                );
                after.push(frame);
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert!(!after.is_empty(), "no frame arrived after the seek");
    assert!(after.iter().all(|f| f.is_valid()));
    assert_eq!(engine.state(), PlaybackState::Playing);

    let stopping = Instant::now();
    engine.stop();
    eprintln!("stop took {:.1}s", stopping.elapsed().as_secs_f64());
}

/// A sweep over a whole folder of samples, for the "does anything crash on my
/// test disc" case: set `MVP_DV_DIR` to the folder and every media file in it is
/// probed, opened, decoded and seeked, each stage under a watchdog.
#[test]
fn sweeps_every_sample_in_a_folder() {
    let Some(dir) = std::env::var_os("MVP_DV_DIR").map(PathBuf::from) else {
        eprintln!("skipping: MVP_DV_DIR is not set");
        return;
    };
    if !dir.is_dir() {
        eprintln!("skipping: {} is not a folder", dir.display());
        return;
    }

    let mut samples: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read the sample folder")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .collect();
    samples.sort();
    assert!(!samples.is_empty(), "no samples in {}", dir.display());

    let mut hung: Vec<String> = Vec::new();

    for path in samples {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        eprintln!("--- {name} ---");

        // ---- probe --------------------------------------------------------
        let probed = {
            let probe_path = path.clone();
            within(60, move || {
                mvp_core::info::probe(&probe_path).map(|info| {
                    let primary = info.primary_video().map(|v| {
                        format!(
                            "{}x{} {} {}",
                            v.width, v.height, v.codec, v.pixel_format
                        )
                    });
                    (
                        info.video.len(),
                        info.audio.len(),
                        info.duration,
                        primary.unwrap_or_else(|| "no video".into()),
                    )
                })
            })
        };
        match probed {
            Err(reason) => {
                eprintln!("  probe {reason}");
                hung.push(format!("{name}: probe"));
                continue;
            }
            Ok(Err(err)) => {
                eprintln!("  probe failed: {err}");
                continue;
            }
            Ok(Ok((video, audio, duration, primary))) => eprintln!(
                "  probe ok: {video} video / {audio} audio / {duration:.1}s / {primary}"
            ),
        }

        // ---- decode + seek, software and hardware --------------------------
        for hardware in [false, true] {
            let decode_path = path.clone();
            let outcome = within(120, move || {
                let engine = engine(hardware);
                engine.open(MediaSource::Path(decode_path)).expect("open");
                let info = wait_for_open(&engine, Duration::from_secs(20));
                let frames = collect_frames(&engine, 3, Duration::from_secs(15));
                let mut seeked = true;
                if info.duration > 10.0 {
                    engine.seek(info.duration / 2.0);
                    std::thread::sleep(Duration::from_millis(500));
                    seeked = !collect_frames(&engine, 1, Duration::from_secs(15)).is_empty();
                }
                let stopped = engine.state();
                engine.stop();
                (info.duration, frames.len(), seeked, stopped)
            });
            match outcome {
                Err(reason) => {
                    eprintln!("  hardware={hardware}: {reason}");
                    hung.push(format!("{name}: decode hardware={hardware}"));
                }
                Ok((duration, frames, seeked, state)) => eprintln!(
                    "  hardware={hardware}: {duration:.1}s, {frames} frames, \
                     seek={seeked}, end state {state:?}"
                ),
            }
        }
    }

    assert!(
        hung.is_empty(),
        "these samples never came back (hang): {}",
        hung.join(", ")
    );
}

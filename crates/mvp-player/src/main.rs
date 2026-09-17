//! # MVP-Versatile-Player
//!
//! Entry point. Its jobs, in order:
//!
//! 1. parse the command line (this is how Explorer's "Open with" arrives);
//! 2. claim the single-instance mutex, or hand the file list to the instance
//!    that already owns it and exit — so double-clicking ten files in Explorer
//!    opens one window with a ten-item playlist rather than ten windows;
//! 3. create the window and hand control to [`app::PlayerApp`].
//!
//! Nothing expensive happens before the window appears: FFmpeg's tables are
//! initialised lazily on first use, the audio device is opened on first
//! playback, and the CJK font is the only file read at start-up.

// A GUI build must not flash a console window; debug builds keep it so that
// `println!`/`log` output is visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod display;
mod icons;
mod layout;
mod settings;
mod state;
mod theme;
mod ui;
mod view;

use std::path::PathBuf;
use std::time::Instant;
use std::sync::Mutex;

use crossbeam_channel::bounded;
use mvp_platform::single_instance::{send_to_primary, AppInstance, IpcMessage};

use crate::app::{PlayerApp, StartupArgs};

fn main() -> eframe::Result<()> {
    let process_start = Instant::now();

    init_logging();

    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("参数错误: {message}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };

    if args.show_help {
        print_help();
        return Ok(());
    }
    if args.show_version {
        println!("MVP-Versatile-Player {}", env!("CARGO_PKG_VERSION"));
        println!("{}", mvp_core::ffmpeg_version());
        return Ok(());
    }

    // Must be set before any window exists so the taskbar groups our windows
    // and the jump list works.
    mvp_platform::shell::set_app_user_model_id("MVP.Versatile.Player");

    // ---- single instance -------------------------------------------------
    let (instance, ipc_rx) = match AppInstance::acquire(settings::APP_ID) {
        Ok(Some(instance)) => {
            let (tx, rx) = bounded::<IpcMessage>(64);
            instance.set_handler(move |message| {
                let _ = tx.try_send(message);
            });
            (Some(instance), rx)
        }
        Ok(None) => {
            let message = if args.files.is_empty() {
                IpcMessage::Activate
            } else {
                IpcMessage::OpenPaths(args.files.clone())
            };
            match send_to_primary(settings::APP_ID, &message) {
                Ok(true) => log::info!("已交由正在运行的实例处理"),
                Ok(false) => log::warn!("未找到正在运行的实例，将以新窗口启动"),
                Err(err) => log::warn!("无法与正在运行的实例通信: {err}"),
            }
            // The primary instance owns the window from here on.
            return Ok(());
        }
        Err(err) => {
            // Losing the single-instance guard is not fatal: the player still
            // works, it just cannot merge windows.
            log::warn!("单实例检测失败，继续以独立进程运行: {err}");
            let (_tx, rx) = bounded::<IpcMessage>(1);
            (None, rx)
        }
    };

    // ---- persisted window geometry --------------------------------------
    //
    // Fitted to the screen *before* the window exists. The size on disk was
    // chosen on whatever display the player last ran on, and that display may
    // be wider, taller or less scaled than the one in front of the user now —
    // asking for it unchanged is what used to open a window taller than the
    // screen, with the transport bar off the bottom edge.
    let settings = settings::Settings::load();
    let (window_size, window_min) = display::startup_size(settings.window_size);
    // A remembered position is only usable while the screen it was chosen on is
    // still there in the same shape. Unplugging a monitor or dropping the
    // resolution otherwise reopens the window somewhere it cannot be reached,
    // so an unreachable position is dropped and the window centred instead.
    let window_pos = settings.window_pos.filter(|position| {
        let reachable = display::position_reachable(*position, window_size);
        if !reachable {
            log::info!("上次的窗口位置 {position:?} 已不在屏幕上，改为居中显示");
        }
        reachable
    });
    log::info!(
        "窗口尺寸 {:?}（最小 {:?}），屏幕可用区域 {:?}",
        window_size,
        window_min,
        mvp_platform::monitor::primary_work_area_points()
    );

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MVP-Versatile-Player")
        .with_app_id("mvp-versatile-player")
        .with_inner_size(window_size)
        .with_min_inner_size(window_min)
        .with_icon(load_window_icon());
    if let Some(position) = window_pos {
        viewport = viewport.with_position(position);
    }
    // `-f` is a launch flag, not a preference: it must not be written back, or
    // one fullscreen start would make every later start fullscreen.
    let start_fullscreen = settings.start_fullscreen || args.fullscreen;
    if start_fullscreen {
        viewport = viewport.with_fullscreen(true);
    } else if settings.maximized {
        viewport = viewport.with_maximized(true);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        vsync: true,
        centered: window_pos.is_none(),
        persist_window: false,
        ..Default::default()
    };

    log::info!(
        "MVP-Versatile-Player {} 启动，准备窗口耗时 {:.1} ms",
        env!("CARGO_PKG_VERSION"),
        process_start.elapsed().as_secs_f32() * 1000.0
    );

    eframe::run_native(
        "MVP-Versatile-Player",
        options,
        Box::new(move |cc| {
            let mut app = PlayerApp::new(cc, args, instance, ipc_rx, process_start);
            if start_fullscreen {
                app.ui.fullscreen = true;
            }
            Ok(Box::new(app))
        }),
    )
}

/// Path of the log file written on every run.
pub fn log_file_path() -> PathBuf {
    settings::Settings::config_dir().join("mvp.log")
}

/// Initialise logging to **both** the console and a rotating log file.
///
/// A release build is a windows-subsystem binary with no console at all, so
/// without a file there would be no way for a user to report what went wrong.
/// Keeping stderr as well means `cargo run` and `--help` stay useful during
/// development.
fn init_logging() {
    // Defer opening the on-disk log file until the first write. Opening the
    // file (and checking its size / rotating) can be slow on some systems
    // (anti-virus, networked profiles). Instead register a writer that
    // lazily opens the file on demand while keeping stderr logging.
    let path = log_file_path();
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    );
    builder.format_timestamp_millis();
    builder.target(env_logger::Target::Pipe(Box::new(LazyTeeWriter::new(path))));
    let _ = builder.try_init();
}

/// Writes every log record to the log file *and* to stderr.
/// Writer that lazily opens the log file on first write and mirrors output to
/// stderr. This keeps start-up fast while still producing a persisted log.
struct LazyTeeWriter {
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

impl LazyTeeWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: Mutex::new(None),
        }
    }
}

impl std::io::Write for LazyTeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Try to open the file lazily. If the mutex is poisoned or open
        // fails, fall back to stderr only so logging never blocks startup.
        if let Ok(mut guard) = self.file.lock() {
            if guard.is_none() {
                if let Some(parent) = self.path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // Keep exactly one previous run so the log cannot grow.
                const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
                if std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
                    let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
                }
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .ok();
                *guard = f;
            }
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(buf);
            }
        }
        let _ = std::io::stderr().write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
        let _ = std::io::stderr().flush();
        Ok(())
    }
}

/// Parse the command line.
///
/// Deliberately hand-rolled rather than pulling in an argument parser: the
/// binary needs to stay small, and the surface is tiny. Unknown options that do
/// not start with `--` are treated as files, which is what makes "Open with"
/// work even when a shell passes something unusual.
fn parse_args(args: impl Iterator<Item = String>) -> Result<StartupArgs, String> {
    let mut parsed = StartupArgs::default();
    let mut pending_files: Vec<PathBuf> = Vec::new();
    let mut iter = args.peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" | "/?" => parsed.show_help = true,
            "-v" | "--version" => parsed.show_version = true,
            "-f" | "--fullscreen" => parsed.fullscreen = true,
            "--no-autoplay" | "--paused" => parsed.no_autoplay = true,
            "--no-audio" | "--mute" => parsed.no_audio = true,
            "-s" | "--subtitle" => {
                let value = iter.next().ok_or("--subtitle 需要一个文件路径")?;
                parsed.subtitle = Some(PathBuf::from(value));
            }
            "--volume" => {
                let value = iter.next().ok_or("--volume 需要一个数值")?;
                let number: f32 = value
                    .trim_end_matches('%')
                    .parse()
                    .map_err(|_| format!("无法解析音量: {value}"))?;
                // Accept both `75` and `0.75`; anything above 2 is a percentage.
                parsed.volume = Some(if number > 2.0 { number / 100.0 } else { number });
            }
            "--speed" => {
                let value = iter.next().ok_or("--speed 需要一个数值")?;
                let number: f64 = value
                    .trim_end_matches('x')
                    .parse()
                    .map_err(|_| format!("无法解析速度: {value}"))?;
                if !(0.25..=4.0).contains(&number) {
                    return Err("速度必须在 0.25 到 4.0 之间".to_string());
                }
                parsed.speed = Some(number);
            }
            other if other.starts_with("--") => {
                return Err(format!("未知选项: {other}"));
            }
            other => pending_files.push(PathBuf::from(other)),
        }
    }

    parsed.files = pending_files;
    Ok(parsed)
}

fn print_help() {
    println!(
        "\
MVP-Versatile-Player {version}
多功能音视频与图片播放器（FFmpeg 内核）

用法:
    mvp-versatile-player [选项] [文件或文件夹...]

选项:
    -f, --fullscreen      以全屏方式启动
        --no-autoplay     本次启动打开文件后暂停，不自动播放
        --no-audio        不打开音频输出（静音播放）
        --volume <值>      初始音量，支持 0.0-2.0 或 0-200（百分比）
        --speed <值>       初始播放速度，0.25 - 4.0
    -s, --subtitle <文件>  为第一个文件加载外部字幕
    -h, --help            显示本帮助
    -v, --version         显示版本与 FFmpeg 版本

说明:
    文件参数可以混用文件与文件夹；文件夹会被递归扫描并加入播放列表。
    如果播放器已经在运行，新打开的文件会加入已有窗口的播放列表。
",
        version = env!("CARGO_PKG_VERSION")
    );
}

/// Build the window/taskbar icon by rasterising the logo.
///
/// Drawing it in code avoids shipping a binary asset and avoids decoding a PNG
/// at start-up, which matters when the whole point is to appear instantly.
fn load_window_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let centre = SIZE as f32 / 2.0;
    let radius = centre - 1.0;

    // Rounded-square background in the accent colour.
    let accent = (0x7Cu8, 0x5Cu8, 0xFFu8);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let fx = x as f32 + 0.5 - centre;
            let fy = y as f32 + 0.5 - centre;
            let distance = (fx * fx + fy * fy).sqrt();
            let alpha = if distance <= radius - 1.0 {
                255.0
            } else if distance <= radius {
                (radius - distance).clamp(0.0, 1.0) * 255.0
            } else {
                0.0
            };
            let index = ((y * SIZE + x) * 4) as usize;
            // A subtle vertical gradient keeps the icon from looking flat.
            let shade = 0.85 + 0.15 * (y as f32 / SIZE as f32);
            rgba[index] = (accent.0 as f32 * shade) as u8;
            rgba[index + 1] = (accent.1 as f32 * shade) as u8;
            rgba[index + 2] = (accent.2 as f32 * shade) as u8;
            rgba[index + 3] = alpha as u8;
        }
    }

    // White play triangle.
    let apex = (centre + 13.0, centre);
    let top = (centre - 9.0, centre - 14.0);
    let bottom = (centre - 9.0, centre + 14.0);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let point = (x as f32 + 0.5, y as f32 + 0.5);
            if point_in_triangle(point, apex, top, bottom) {
                let index = ((y * SIZE + x) * 4) as usize;
                rgba[index] = 255;
                rgba[index + 1] = 255;
                rgba[index + 2] = 255;
                rgba[index + 3] = 255;
            }
        }
    }

    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

fn point_in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<StartupArgs, String> {
        parse_args(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn files_are_collected() {
        let result = parse(&["a.mkv", "b.mp3"]).unwrap();
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files[0], PathBuf::from("a.mkv"));
    }

    #[test]
    fn flags_are_recognised() {
        let result = parse(&["--fullscreen", "--no-autoplay", "movie.mkv"]).unwrap();
        assert!(result.fullscreen);
        assert!(result.no_autoplay);
        assert_eq!(result.files, vec![PathBuf::from("movie.mkv")]);
    }

    /// Double-clicking a file in Explorer must play it, whatever the settings
    /// file happens to say.
    ///
    /// The bug this guards: the start-up path used to consult a saved
    /// "打开文件后自动播放" preference, which one `--no-autoplay` launch (or one
    /// click on the switch) could leave off for good — the player then opened
    /// every double-clicked file paused with nothing on screen to explain why.
    /// Only the launch itself may ask for a paused start.
    #[test]
    fn a_file_argument_is_played_unless_this_launch_asks_otherwise() {
        let plain = parse(&["movie.mkv"]).unwrap();
        assert_eq!(plain.files.len(), 1);
        assert!(plain.autoplay_command_line_files());

        let paused = parse(&["--no-autoplay", "movie.mkv"]).unwrap();
        assert!(!paused.autoplay_command_line_files());
        assert!(!parse(&["--paused"]).unwrap().autoplay_command_line_files());
    }

    #[test]
    fn volume_accepts_fractions_and_percentages() {
        assert_eq!(parse(&["--volume", "0.75"]).unwrap().volume, Some(0.75));
        assert_eq!(parse(&["--volume", "75"]).unwrap().volume, Some(0.75));
        assert_eq!(parse(&["--volume", "75%"]).unwrap().volume, Some(0.75));
    }

    #[test]
    fn speed_is_validated() {
        assert_eq!(parse(&["--speed", "1.5"]).unwrap().speed, Some(1.5));
        assert_eq!(parse(&["--speed", "2x"]).unwrap().speed, Some(2.0));
        assert!(parse(&["--speed", "9"]).is_err());
    }

    #[test]
    fn unknown_options_are_rejected_with_a_message() {
        let err = parse(&["--nope"]).unwrap_err();
        assert!(err.contains("--nope"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(parse(&["--help"]).unwrap().show_help);
        assert!(parse(&["-v"]).unwrap().show_version);
    }

    #[test]
    fn window_icon_is_well_formed() {
        let icon = load_window_icon();
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert_eq!(icon.width, 64);
        // The centre must be the white of the play triangle.
        let index = ((32 * 64 + 32) * 4) as usize;
        assert_eq!(&icon.rgba[index..index + 3], &[255, 255, 255]);
        // A corner must be fully transparent.
        assert_eq!(icon.rgba[3], 0);
    }
}

//! `mvp-updater.exe` — the external half of the update mechanism.
//!
//! The player checks for an update and downloads the package; this binary does
//! the part the player cannot do to itself: it waits for the player to exit,
//! unpacks and verifies the package, replaces only the files that changed and
//! restarts the player.
//!
//! Two ways to run it:
//!
//! * **Guided** (what the player uses):
//!   `mvp-updater.exe --target-dir <dir> --package <zip> --sha256 <hex>
//!   --parent-pid <pid> --version <x.y.z> [--restart <exe>]`
//! * **Standalone** (a user double-clicks it): with no arguments it targets its
//!   own directory, asks the update service what the latest release is and does
//!   the whole flow itself. If it is running from inside the install directory
//!   it first copies itself to `%APPDATA%\...\update\` and relaunches from there,
//!   so it never has to replace the executable it is running as.
//!
//! There is no window. Progress and failures go to
//! `%APPDATA%\MVP-Versatile-Player\update\updater.log`; a failure additionally
//! shows a message box and restarts the (unchanged) old version.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use mvp_updater::{api, apply, config, net};

/// File name of the player executable inside the install directory.
const PLAYER_EXE: &str = "mvp-versatile-player.exe";
/// File name this updater uses when it copies itself into the update directory.
const UPDATER_EXE: &str = "mvp-updater.exe";
/// How long to wait for the player to exit, and for its files to unlock.
const EXIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Rotate the log once it reaches this size.
const LOG_MAX_BYTES: u64 = 1024 * 1024;

fn main() {
    logging::init();

    let args = match Args::parse() {
        Ok(args) => args,
        Err(error) => {
            log::error!("bad command line: {error:#}");
            show_error(&format!("更新器参数错误：\n\n{error}"));
            std::process::exit(2);
        }
    };
    if args.help {
        print_help();
        return;
    }

    let player = args
        .restart
        .clone()
        .or_else(|| args.target_dir.as_ref().map(|dir| dir.join(PLAYER_EXE)));

    let target = match resolve_target(&args) {
        Ok(target) => target,
        Err(error) => {
            log::error!("could not determine the install directory: {error:#}");
            show_error(&format!("无法确定安装目录：\n\n{error}"));
            std::process::exit(2);
        }
    };

    if let Err(error) = run(&args, target) {
        log::error!("the update failed: {error:#}");
        show_error(&format!(
            "更新失败：\n\n{error}\n\n旧版本已保留，可继续使用。"
        ));
        // Put the user back where they were: relaunch the version that is still
        // installed (the rollback guarantees it is intact).
        if let Some(exe) = player.filter(|exe| exe.exists()) {
            let workdir = exe.parent().unwrap_or_else(|| Path::new("."));
            if let Err(error) = apply::restart_player(&exe, workdir) {
                log::error!("could not restart the old version: {error:#}");
            }
        }
        std::process::exit(1);
    }
}

/// Decide which directory to update, refusing the one case that cannot work.
fn resolve_target(args: &Args) -> Result<PathBuf> {
    if let Some(target) = &args.target_dir {
        return Ok(target.clone());
    }
    let exe = std::env::current_exe().context("could not locate the updater executable")?;
    let dir = exe
        .parent()
        .context("the updater executable has no parent directory")?;
    Ok(dir.to_path_buf())
}

/// The orchestration proper.
fn run(args: &Args, target: PathBuf) -> Result<()> {
    let update_dir = config::update_dir();

    // Running from inside the install directory means replacing ourselves is
    // impossible; move to the update directory and hand off first.
    if let Some(self_dir) = current_dir() {
        if same_dir(&self_dir, &target) && !same_dir(&self_dir, &update_dir) {
            return relocate_self(&target);
        }
        if same_dir(&self_dir, &update_dir) && args.target_dir.is_none() {
            bail!("the updater lives in the update directory; run it with --target-dir");
        }
    }

    // One updater per install directory.
    let _guard = single_instance(&target)?;

    // Resolve the package: given by the player, or downloaded here.
    let (package, version, sha256) = match &args.package {
        Some(package) => (
            package.clone(),
            args.version.clone().unwrap_or_default(),
            args.sha256.clone(),
        ),
        None => match download_latest()? {
            Some(download) => (download.package, download.version, download.sha256),
            None => {
                log::info!("nothing to update");
                return Ok(());
            }
        },
    };

    // Wait for the player to actually exit, then for its files to unlock. The
    // process wait is the primary signal; the file probe defends against a
    // reused pid and against a zombie that has not released its image yet.
    if let Some(pid) = args.parent_pid {
        wait_for_process(pid, EXIT_TIMEOUT);
    }
    wait_until_writable(&target.join(PLAYER_EXE), EXIT_TIMEOUT)?;

    let restart = args.restart.clone().or_else(|| {
        let exe = target.join(PLAYER_EXE);
        exe.exists().then_some(exe)
    });
    let options = apply::RunOptions {
        target_dir: target,
        package,
        version,
        expected_sha256: sha256,
        restart_exe: restart,
    };
    let outcome = apply::run(&options)?;
    log::info!(
        "update applied: {} of {} file(s) written, {} unchanged",
        outcome.installed,
        outcome.entries,
        outcome.unchanged
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Standalone mode
// ---------------------------------------------------------------------------

/// A package that was downloaded by the standalone flow.
struct Downloaded {
    package: PathBuf,
    version: String,
    sha256: Option<String>,
}

/// Ask the service for the latest release and download it if it is newer than
/// this build. `Ok(None)` means "there is nothing to do".
fn download_latest() -> Result<Option<Downloaded>> {
    let Some(info) = api::fetch_latest()? else {
        log::info!("the update service has no published version");
        return Ok(None);
    };
    let current = env!("CARGO_PKG_VERSION");
    if !info.update_is_available(current) {
        log::info!("already up to date (running {current}, latest {})", info.version);
        return Ok(None);
    }

    let dir = config::download_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("could not create {}", dir.display()))?;
    let package = dir.join(package_file_name(&info));
    log::info!("downloading {} from {}", info.version, info.download_url);

    let mut last_log = Instant::now();
    let computed = net::download_to_file(&info.download_url, &package, &mut |received, total| {
        if last_log.elapsed() >= Duration::from_secs(2) {
            last_log = Instant::now();
            match total {
                Some(total) if total > 0 => {
                    log::info!("  {} / {} KiB", received / 1024, total / 1024)
                }
                _ => log::info!("  {} KiB", received / 1024),
            }
        }
    })?;

    if let Some(expected) = info.expected_sha256() {
        if !computed.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&package);
            bail!("the download failed its SHA-256 check (expected {expected}, got {computed})");
        }
    }

    Ok(Some(Downloaded {
        package,
        version: info.version.clone(),
        sha256: info.expected_sha256().map(str::to_string),
    }))
}

/// A sensible file name for the package, taken from the download URL.
fn package_file_name(info: &api::UpdateInfo) -> String {
    info.download_url
        .rsplit('/')
        .next()
        .map(|segment| segment.split(['?', '#']).next().unwrap_or(""))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("MVP-Versatile-Player{}.zip", info.version))
}

/// Copy the running updater into the update directory and relaunch it there,
/// targeting the same install directory. Does not return.
fn relocate_self(target: &Path) -> Result<()> {
    let self_exe = std::env::current_exe().context("could not locate the updater executable")?;
    let update_dir = config::update_dir();
    fs::create_dir_all(&update_dir)
        .with_context(|| format!("could not create {}", update_dir.display()))?;
    let relocated = update_dir.join(UPDATER_EXE);
    if !same_file(&self_exe, &relocated) {
        fs::copy(&self_exe, &relocated)
            .with_context(|| format!("could not copy the updater to {}", relocated.display()))?;
    }

    log::info!("relocating to {} and handing off", relocated.display());
    let mut command = std::process::Command::new(&relocated);
    command.arg("--target-dir").arg(target);
    let player = target.join(PLAYER_EXE);
    if player.exists() {
        command.arg("--restart").arg(&player);
    }
    command
        .spawn()
        .context("could not relaunch the updater from the update directory")?;
    // Release our copy of the executable so the new instance owns the directory.
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

/// Parsed command line.
#[derive(Debug, Default)]
struct Args {
    target_dir: Option<PathBuf>,
    package: Option<PathBuf>,
    sha256: Option<String>,
    parent_pid: Option<u32>,
    version: Option<String>,
    restart: Option<PathBuf>,
    help: bool,
}

impl Args {
    fn parse() -> Result<Args> {
        let argv: Vec<String> = std::env::args().collect();
        let mut args = Args::default();
        let mut index = 1;
        while index < argv.len() {
            let flag = argv[index].clone();
            match flag.as_str() {
                "--target-dir" => {
                    index += 1;
                    args.target_dir = Some(PathBuf::from(value_at(&argv, index, &flag)?));
                }
                "--package" => {
                    index += 1;
                    args.package = Some(PathBuf::from(value_at(&argv, index, &flag)?));
                }
                "--sha256" => {
                    index += 1;
                    args.sha256 = Some(value_at(&argv, index, &flag)?);
                }
                "--parent-pid" => {
                    index += 1;
                    let value = value_at(&argv, index, &flag)?;
                    args.parent_pid = Some(
                        value
                            .parse()
                            .with_context(|| format!("{flag} expects a number, got {value:?}"))?,
                    );
                }
                "--version" => {
                    index += 1;
                    args.version = Some(value_at(&argv, index, &flag)?);
                }
                "--restart" => {
                    index += 1;
                    args.restart = Some(PathBuf::from(value_at(&argv, index, &flag)?));
                }
                "-h" | "--help" => args.help = true,
                other => bail!("unknown argument {other:?}"),
            }
            index += 1;
        }
        Ok(args)
    }
}

/// The value that must follow a flag.
fn value_at(argv: &[String], index: usize, flag: &str) -> Result<String> {
    argv.get(index)
        .cloned()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{flag} needs a value"))
}

/// Usage text (visible only in debug builds, which keep a console).
fn print_help() {
    println!(
        "\
mvp-updater {version}

用法:
  mvp-updater --target-dir <目录> --package <zip> --sha256 <hex> --parent-pid <pid> --version <x.y.z> [--restart <exe>]
  mvp-updater               独立模式：更新自身所在目录

选项:
  --target-dir <目录>    要更新的安装目录
  --package <zip>        已下载的发布包
  --sha256 <hex>         发布包的 SHA-256（解压前复核）
  --parent-pid <pid>     等待该进程退出后再更新
  --version <x.y.z>      要安装的版本号
  --restart <exe>        更新完成后重启的程序
  -h, --help             显示本帮助",
        version = env!("CARGO_PKG_VERSION")
    );
}

// ---------------------------------------------------------------------------
// Path and time helpers
// ---------------------------------------------------------------------------

/// The directory the running executable lives in.
fn current_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Case-insensitive, canonicalised comparison of two directories.
fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical = |path: &Path| {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    canonical(a).eq_ignore_ascii_case(&canonical(b))
}

/// Whether two paths denote the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    same_dir(a, b)
}

/// Poll `path` until it can be opened for writing, or `timeout` elapses.
///
/// Used to confirm the player has truly let go of its own executable: the
/// loader keeps the image open with the write bit denied, so a successful
/// write-open is a reliable "it is gone" signal.
fn wait_until_writable(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if !path.exists() {
            return Ok(());
        }
        match fs::OpenOptions::new().write(true).open(path) {
            Ok(_) => return Ok(()),
            Err(_) => {
                if Instant::now() >= deadline {
                    bail!("{} is still locked after {timeout:?}", path.display());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Windows integration
// ---------------------------------------------------------------------------

/// How long a freshly opened handle is held until it is closed. On Windows the
/// OS releases it on process exit regardless, so there is nothing to do; on
/// other platforms this is a placeholder.
#[cfg(windows)]
struct InstanceGuard(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the handle was created by `CreateMutexW` and is closed once,
        // here.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// Non-Windows placeholder so the rest of `main` compiles.
#[cfg(not(windows))]
struct InstanceGuard;

/// Acquire the per-install named mutex, refusing to run while another updater
/// for the same directory is active.
#[cfg(windows)]
fn single_instance(target: &Path) -> Result<InstanceGuard> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let hash = hash_path(target);
    let name = format!("Local\\MVPVersatilePlayer.Updater.{hash:016x}");
    let name_w = wide(&name);
    // SAFETY: `name_w` is a NUL-terminated buffer that outlives the call.
    let created = unsafe { CreateMutexW(None, true, PCWSTR(name_w.as_ptr())) };
    // SAFETY: `GetLastError` must be read before anything else overwrites it; it
    // has no preconditions.
    let last_error = unsafe { GetLastError() };
    let handle = created.with_context(|| format!("CreateMutexW({name})"))?;

    if last_error == ERROR_ALREADY_EXISTS {
        // SAFETY: our own handle; closing it leaves the other instance's mutex.
        let _ = unsafe { CloseHandle(handle) };
        bail!("another updater is already running for {}", target.display());
    }
    Ok(InstanceGuard(handle))
}

/// Non-Windows stub: no named mutex.
#[cfg(not(windows))]
fn single_instance(target: &Path) -> Result<InstanceGuard> {
    let _ = target;
    Ok(InstanceGuard)
}

/// Wait (bounded) for process `pid` to exit.
#[cfg(windows)]
fn wait_for_process(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    if pid == 0 {
        return;
    }
    // SAFETY: `OpenProcess` takes only plain values; a failure is returned.
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => {
            let millis = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
            // SAFETY: `handle` is a live process handle that outlives the wait.
            let result = unsafe { WaitForSingleObject(handle, millis) };
            // SAFETY: `handle` is ours and not used afterwards.
            let _ = unsafe { CloseHandle(handle) };
            if result == WAIT_OBJECT_0 {
                log::info!("the player process {pid} has exited");
            } else {
                log::warn!("the player process {pid} did not exit within {timeout:?}");
            }
        }
        Err(error) => log::warn!("could not open the player process {pid}: {error}"),
    }
}

/// Non-Windows stub: nothing to wait for.
#[cfg(not(windows))]
fn wait_for_process(pid: u32, timeout: Duration) {
    let _ = (pid, timeout);
}

/// Show a modal error box. Best effort: never fails the process.
#[cfg(windows)]
fn show_error(message: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    };

    let body = wide(message);
    let caption = wide("MVP-Versatile-Player 更新");
    // SAFETY: both buffers are NUL-terminated and outlive the call; there is no
    // parent window.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

/// Non-Windows stub: write to stderr.
#[cfg(not(windows))]
fn show_error(message: &str) {
    eprintln!("{message}");
}

/// Encode `s` as a NUL-terminated UTF-16 buffer for `PCWSTR`.
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A stable 64-bit hash of a path, for the single-instance mutex name.
fn hash_path(path: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().to_ascii_lowercase().hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

mod logging {
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use log::{LevelFilter, Log, Metadata, Record};

    use mvp_updater::config;

    /// Install the file logger. A second call (never expected) is ignored.
    pub fn init() {
        // The logger lives for the whole process; leaking the box is exactly the
        // right lifetime for a `&'static dyn Log`.
        let logger: &'static FileLogger = Box::leak(Box::new(FileLogger {
            path: config::log_path(),
            max_bytes: super::LOG_MAX_BYTES,
            file: Mutex::new(None),
        }));
        if log::set_logger(logger).is_ok() {
            let level = if std::env::var_os("MVP_UPDATER_DEBUG").is_some() {
                LevelFilter::Debug
            } else {
                LevelFilter::Info
            };
            log::set_max_level(level);
        }
    }

    /// A tiny append-only logger that rotates one generation at `max_bytes`.
    ///
    /// A full logging framework is not worth a dependency here: the updater
    /// writes a handful of lines and is gone.
    struct FileLogger {
        path: PathBuf,
        max_bytes: u64,
        file: Mutex<Option<File>>,
    }

    impl Log for FileLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            metadata.level() <= log::max_level()
        }

        fn log(&self, record: &Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let line = format!(
                "[{}] {:<5} {}\n",
                timestamp(),
                record.level().as_str(),
                record.args()
            );
            self.write_line(line.as_bytes());
        }

        fn flush(&self) {
            if let Ok(mut slot) = self.file.lock() {
                if let Some(file) = slot.as_mut() {
                    let _ = file.flush();
                }
            }
        }
    }

    impl FileLogger {
        fn write_line(&self, bytes: &[u8]) {
            let Ok(mut slot) = self.file.lock() else {
                return;
            };
            // Rotate before appending so the live log stays within the cap.
            let current = slot
                .as_ref()
                .and_then(|file| file.metadata().ok())
                .map(|meta| meta.len())
                .unwrap_or(0);
            if current >= self.max_bytes {
                *slot = None;
                let _ = fs::rename(&self.path, self.path.with_extension("log.1"));
            }
            if slot.is_none() {
                if let Some(parent) = self.path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                *slot = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .ok();
            }
            if let Some(file) = slot.as_mut() {
                let _ = file.write_all(bytes);
            }
        }
    }

    /// `YYYY-MM-DD hh:mm:ss.mmm`, local time.
    #[cfg(windows)]
    fn timestamp() -> String {
        use windows::Win32::System::SystemInformation::GetLocalTime;

        // SAFETY: `GetLocalTime` has no arguments and no preconditions.
        let time = unsafe { GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond,
            time.wMilliseconds
        )
    }

    /// Seconds since the Unix epoch, on hosts with no local-time call.
    #[cfg(not(windows))]
    fn timestamp() -> String {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(since) => format!("{}.{:03}", since.as_secs(), since.subsec_millis()),
            Err(_) => "0.000".to_string(),
        }
    }
}

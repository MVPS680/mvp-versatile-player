//! Per-user single-instance guard plus the IPC channel that forwards a
//! second launch to the running player.
//!
//! The scheme has two halves:
//!
//! * **Primary detection** — a named mutex `Local\MVPVersatilePlayer.<app_id>.Mutex`.
//!   The first process to create it wins; everybody else sees
//!   `ERROR_ALREADY_EXISTS` and behaves as a *secondary* instance.
//! * **Transport** — a hidden top-level window (never shown, `WS_EX_TOOLWINDOW`)
//!   owned by a dedicated thread that pumps its own message queue. A secondary
//!   instance locates it with `FindWindowW` and forwards a `WM_COPYDATA`
//!   payload, which is bounded by `SendMessageTimeoutW` so a hung primary can
//!   never wedge the new process.
//!
//! The window is a *normal* top-level window rather than a message-only
//! (`HWND_MESSAGE`) window on purpose: message-only windows are children of an
//! invisible system window and are not reachable through `FindWindowW` from
//! another process.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

use once_cell::sync::Lazy;

/// Namespace prefix shared with [`crate::assoc::PROG_ID_PREFIX`], so every
/// kernel object we create is recognisably ours.
#[cfg(windows)]
const IPC_NAMESPACE: &str = crate::assoc::PROG_ID_PREFIX;

/// Tag of the `activate` payload: bring the existing window to the front.
const TAG_ACTIVATE: &str = "activate";
/// Tag of the `open` payload: a file list follows, one path per line.
const TAG_OPEN: &str = "open";
/// Tag of the `quit` payload: ask the primary instance to exit.
const TAG_QUIT: &str = "quit";

/// `dwData` stamped into every `COPYDATASTRUCT` we send, so a stray
/// `WM_COPYDATA` from an unrelated program is ignored instead of being parsed
/// as a file list.
#[cfg(windows)]
const IPC_MAGIC: usize = 0x4D56_5031; // "MVP1"

/// How long a secondary instance waits for the primary to acknowledge.
#[cfg(windows)]
const IPC_SEND_TIMEOUT_MS: u32 = 5_000;

/// How long `acquire()` waits for the background thread to create its window.
#[cfg(windows)]
const IPC_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A message forwarded from a secondary instance to the primary one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcMessage {
    /// Show and focus the running player.
    Activate,
    /// Open (or enqueue) these paths in the running player.
    OpenPaths(Vec<PathBuf>),
    /// Ask the running player to exit.
    Quit,
}

/// Callback invoked on the IPC thread for every forwarded message.
type Handler = Arc<dyn Fn(IpcMessage) + Send + Sync + 'static>;

/// Process-global handler slot.
///
/// A single slot is enough because a process only ever hosts one primary
/// instance; `acquire` is the only way to become one, and the IPC window of a
/// *different* `app_id` in the same process would still be the same
/// application. Keeping the slot here (rather than in `GWLP_USERDATA`) avoids
/// handing a raw pointer's lifetime to the window manager.
static HANDLER: Lazy<RwLock<Option<Handler>>> = Lazy::new(|| RwLock::new(None));

/// Handle to the primary-instance role.
///
/// Holds the named mutex for as long as the value lives, so that even a
/// `WM_APP + 1`-less exit cannot let a second player start before this one is
/// gone. The IPC thread is joined by [`AppInstance::shutdown`] (and by `Drop`).
pub struct AppInstance {
    /// Raw handle of the named mutex, `0` once closed.
    mutex: AtomicIsize,
    /// `HWND` of the hidden IPC window, `0` before creation / after teardown.
    window: Arc<AtomicIsize>,
    /// Thread id of the IPC thread, used to make `shutdown()` re-entrant.
    thread_id: Arc<AtomicU32>,
    /// Set by `shutdown()` so it is a no-op the second time.
    stopped: AtomicBool,
    /// The thread that owns the IPC window and pumps its messages.
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AppInstance {
    /// Become the primary instance, or return `Ok(None)` when one already exists.
    ///
    /// On success this starts the hidden IPC window on a background thread and
    /// waits (briefly, bounded by [`IPC_STARTUP_TIMEOUT`]) for that window to
    /// exist, so a secondary instance started immediately afterwards is
    /// guaranteed to find it. It never blocks on the message loop itself.
    #[cfg(windows)]
    pub fn acquire(app_id: &str) -> anyhow::Result<Option<AppInstance>> {
        win::acquire(app_id)
    }

    /// Non-Windows stub: there is no named-mutex / window-message backend.
    #[cfg(not(windows))]
    pub fn acquire(app_id: &str) -> anyhow::Result<Option<AppInstance>> {
        let _ = app_id;
        Err(anyhow::anyhow!("windows only"))
    }

    /// Install the callback invoked (on the IPC thread) for every forwarded
    /// message.
    ///
    /// Replaces any previously installed handler. A handler that panics is
    /// contained: the panic is logged and the offending message dropped, so the
    /// IPC thread keeps running.
    pub fn set_handler<F: Fn(IpcMessage) + Send + Sync + 'static>(&self, f: F) {
        match HANDLER.write() {
            Ok(mut slot) => *slot = Some(Arc::new(f)),
            Err(_) => log::error!("the IPC handler slot is poisoned; the handler was not installed"),
        }
    }

    /// Stop the IPC thread and release the primary-instance mutex.
    ///
    /// Safe to call more than once, and safe to call from inside the handler
    /// (the join is skipped in that case to avoid self-deadlock). Called
    /// automatically by `Drop`.
    pub fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        win_request_quit(&self.window);

        // Never join ourselves: `shutdown` may legitimately be called from the
        // handler, which runs on the IPC thread.
        let on_ipc_thread = current_thread_id() != 0
            && current_thread_id() == self.thread_id.load(Ordering::SeqCst);
        if !on_ipc_thread {
            join_thread(&self.thread);
        }
        close_mutex(&self.mutex);
    }
}

impl Drop for AppInstance {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Forward a message to the running primary instance.
///
/// Returns `Ok(false)` when no primary is listening — either because none is
/// running or because it stopped answering — which callers should treat as
/// "start the player normally".
#[cfg(windows)]
pub fn send_to_primary(app_id: &str, msg: &IpcMessage) -> anyhow::Result<bool> {
    win::send_to_primary(app_id, msg)
}

/// Non-Windows stub: there is never a primary instance.
#[cfg(not(windows))]
pub fn send_to_primary(app_id: &str, msg: &IpcMessage) -> anyhow::Result<bool> {
    let _ = (app_id, msg);
    Ok(false)
}

// ---------------------------------------------------------------------------
// Payload codec (portable, so it can be unit-tested everywhere)
// ---------------------------------------------------------------------------

/// Serialise `msg` to the wire format: a tag line, then one path per line.
#[cfg_attr(not(windows), allow(dead_code))]
fn encode_message(msg: &IpcMessage) -> Vec<u8> {
    let mut payload = String::new();
    match msg {
        IpcMessage::Activate => payload.push_str(TAG_ACTIVATE),
        IpcMessage::Quit => payload.push_str(TAG_QUIT),
        IpcMessage::OpenPaths(paths) => {
            payload.push_str(TAG_OPEN);
            for path in paths {
                payload.push('\n');
                payload.push_str(&path.to_string_lossy());
            }
        }
    }
    payload.into_bytes()
}

/// Serialise `msg` including the terminating NUL that `WM_COPYDATA` requires
/// (`cbData` must cover it).
#[cfg_attr(not(windows), allow(dead_code))]
fn encode_message_nul(msg: &IpcMessage) -> Vec<u8> {
    let mut payload = encode_message(msg);
    payload.push(0);
    payload
}

/// Parse a received payload; `None` when it is not valid UTF-8 or carries an
/// unknown tag.
#[cfg_attr(not(windows), allow(dead_code))]
fn decode_message(bytes: &[u8]) -> Option<IpcMessage> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split('\n');
    let tag = lines.next()?.trim_end_matches('\r').trim();
    match tag {
        TAG_ACTIVATE => Some(IpcMessage::Activate),
        TAG_QUIT => Some(IpcMessage::Quit),
        TAG_OPEN => Some(IpcMessage::OpenPaths(
            lines
                .map(|line| line.trim_end_matches('\r'))
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
        )),
        _ => None,
    }
}

/// Drop the trailing NUL(s) a `WM_COPYDATA` payload is padded with.
#[cfg_attr(not(windows), allow(dead_code))]
fn trim_trailing_nul(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    &bytes[..end]
}

// ---------------------------------------------------------------------------
// Thread helpers (portable shims over the Win32 bits)
// ---------------------------------------------------------------------------

/// Thread id of the calling thread, or `0` where the concept does not exist.
#[cfg(windows)]
fn current_thread_id() -> u32 {
    // SAFETY: `GetCurrentThreadId` has no arguments and no preconditions.
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

/// Non-Windows shim: no thread ids, so the self-join guard never triggers.
#[cfg(not(windows))]
fn current_thread_id() -> u32 {
    0
}

/// Ask the IPC thread to leave its message loop.
#[cfg(windows)]
fn win_request_quit(window: &AtomicIsize) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

    let hwnd = window.load(Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    let thread_window = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: `thread_window` is a handle we created and only ever post to; a
    // stale handle simply makes `PostMessageW` fail.
    if let Err(error) = unsafe {
        PostMessageW(
            Some(thread_window),
            WM_APP + 1,
            WPARAM(0),
            LPARAM(0),
        )
    } {
        log::debug!("could not post the IPC shutdown message: {error}");
    }
}

/// Non-Windows shim: nothing to wake up.
#[cfg(not(windows))]
fn win_request_quit(window: &AtomicIsize) {
    let _ = window;
}

/// Join the IPC thread, logging instead of panicking if it panicked itself.
fn join_thread(thread: &Mutex<Option<JoinHandle<()>>>) {
    let handle = match thread.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => {
            log::error!("the IPC thread handle is poisoned; skipping the join");
            None
        }
    };
    if let Some(handle) = handle {
        if handle.join().is_err() {
            log::error!("the IPC thread panicked");
        }
    }
}

/// Close the named mutex and mark the handle as gone.
#[cfg(windows)]
fn close_mutex(mutex: &AtomicIsize) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};

    let raw = mutex.swap(0, Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    // SAFETY: `raw` is a handle we created with `CreateMutexW`, and the swap
    // above guarantees it is closed exactly once.
    let _ = unsafe { CloseHandle(HANDLE(raw as *mut core::ffi::c_void)) };
}

/// Non-Windows shim: no handle to close.
#[cfg(not(windows))]
fn close_mutex(mutex: &AtomicIsize) {
    mutex.store(0, Ordering::SeqCst);
}

/// Invoke the installed handler, containing any panic it raises.
#[cfg(windows)]
fn dispatch_to_handler(message: IpcMessage) {
    let handler = HANDLER.read().ok().and_then(|slot| slot.clone());
    let Some(handler) = handler else {
        log::debug!("received an IPC message, but no handler is installed yet");
        return;
    };
    // The handler is user code running inside a window procedure: a panic that
    // escaped into the OS would abort the process.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(message)));
    if outcome.is_err() {
        log::error!("the IPC message handler panicked; the message was dropped");
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::{
        current_thread_id, decode_message, dispatch_to_handler, encode_message_nul, AppInstance,
        IpcMessage, IPC_MAGIC, IPC_NAMESPACE, IPC_SEND_TIMEOUT_MS, IPC_STARTUP_TIMEOUT,
    };
    use crate::wide::wide;

    use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
    use std::sync::{mpsc, Arc, Mutex};

    use anyhow::Context;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE,
        HWND, LPARAM, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::HBRUSH;
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow,
        DispatchMessageW, FindWindowW, GetMessageW, PostMessageW, PostQuitMessage, RegisterClassW,
        SendMessageTimeoutW, TranslateMessage, UnregisterClassW, ASFW_ANY, HCURSOR, HICON, MSG,
        SMTO_ABORTIFHUNG, SMTO_NORMAL, WM_APP, WM_COPYDATA, WM_DESTROY, WNDCLASSW, WNDCLASS_STYLES,
        WS_EX_TOOLWINDOW, WS_POPUP,
    };

    /// Name of the mutex that decides who is the primary instance.
    fn mutex_name(app_id: &str) -> String {
        format!(r"Local\{IPC_NAMESPACE}.{app_id}.Mutex")
    }

    /// Class name of the hidden IPC window (used both to create it and to find
    /// it again from a secondary instance).
    fn window_class(app_id: &str) -> String {
        format!("{IPC_NAMESPACE}.{app_id}.IpcWnd")
    }

    /// See [`AppInstance::acquire`].
    pub(super) fn acquire(app_id: &str) -> anyhow::Result<Option<AppInstance>> {
        let name = mutex_name(app_id);
        let name_w = wide(&name);
        // SAFETY: `name_w` is a live NUL-terminated buffer, no security
        // attributes are needed, and we ask for initial ownership so the
        // `ERROR_ALREADY_EXISTS` result means "somebody else got here first".
        let created = unsafe { CreateMutexW(None, true, PCWSTR(name_w.as_ptr())) };
        // `GetLastError` must be read before anything else can overwrite it.
        // SAFETY: no arguments, no preconditions.
        let last_error = unsafe { GetLastError() };
        let handle = created.with_context(|| format!("CreateMutexW({name})"))?;

        if last_error == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` is ours; closing it does not release the mutex
            // object owned by the real primary instance.
            let _ = unsafe { CloseHandle(handle) };
            log::info!("another instance already owns {name}; starting as a secondary");
            return Ok(None);
        }

        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        let window = Arc::new(AtomicIsize::new(0));
        let thread_id = Arc::new(AtomicU32::new(0));
        let class = window_class(app_id);
        let thread_window = Arc::clone(&window);
        let thread_id_slot = Arc::clone(&thread_id);

        let builder = std::thread::Builder::new().name(format!("mvp-ipc-{app_id}"));
        let spawned = builder.spawn(move || {
            run_ipc_thread(&class, &thread_window, &thread_id_slot, &ready_tx);
        });

        let spawned = match spawned {
            Ok(handle) => handle,
            Err(error) => {
                // SAFETY: `handle` is our own mutex handle and is closed once.
                let _ = unsafe { CloseHandle(handle) };
                return Err(anyhow::Error::new(error).context("spawning the IPC thread"));
            }
        };

        let startup = ready_rx.recv_timeout(IPC_STARTUP_TIMEOUT);
        let startup_error = match startup {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(error) => Some(anyhow::anyhow!(
                "timed out after {IPC_STARTUP_TIMEOUT:?} waiting for the IPC window ({error})"
            )),
        };

        if let Some(error) = startup_error {
            // Roll back: wake the thread if it somehow got as far as a window,
            // then release the mutex so the next launch can try again.
            let hwnd = window.load(Ordering::SeqCst);
            if hwnd != 0 {
                // SAFETY: this is our own window handle; failure is harmless.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd as *mut core::ffi::c_void)),
                        WM_APP + 1,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
            let _ = spawned.join();
            // SAFETY: `handle` is our own mutex handle and is closed exactly once.
            let _ = unsafe { CloseHandle(handle) };
            return Err(error.context("starting the single-instance IPC endpoint"));
        }

        log::debug!("acquired the primary-instance mutex {name}");

        Ok(Some(AppInstance {
            mutex: AtomicIsize::new(handle.0 as isize),
            window,
            thread_id,
            stopped: std::sync::atomic::AtomicBool::new(false),
            thread: Mutex::new(Some(spawned)),
        }))
    }

    /// Body of the IPC thread: create the window, report readiness, pump
    /// messages, then clean up.
    fn run_ipc_thread(
        class: &str,
        window_slot: &AtomicIsize,
        thread_id_slot: &AtomicU32,
        ready: &mpsc::Sender<anyhow::Result<()>>,
    ) {
        let window = match create_ipc_window(class) {
            Ok(window) => window,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        window_slot.store(window.0 as isize, Ordering::SeqCst);
        thread_id_slot.store(current_thread_id(), Ordering::SeqCst);
        let _ = ready.send(Ok(()));

        pump_messages();

        window_slot.store(0, Ordering::SeqCst);
        // SAFETY: we own `window`; destroying it before unregistering the class
        // is the documented order. Failures are not actionable.
        unsafe {
            let _ = DestroyWindow(window);
        }
        let class_w = wide(class);
        // SAFETY: `class_w` is NUL-terminated and the class is no longer in use.
        unsafe {
            let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), None);
        }
    }

    /// Register the window class and create the never-shown IPC window.
    fn create_ipc_window(class: &str) -> anyhow::Result<HWND> {
        // SAFETY: `PCWSTR::null()` asks for the handle of the current module.
        let module = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .context("GetModuleHandleW for the IPC window class")?;
        let instance = HINSTANCE(module.0);

        let class_w = wide(class);
        let title_w = wide(&format!("{IPC_NAMESPACE} IPC endpoint ({class})"));
        let definition = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(window_procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: HICON(std::ptr::null_mut()),
            hCursor: HCURSOR(std::ptr::null_mut()),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
        };
        // SAFETY: `definition` is fully initialised and every string pointer it
        // holds outlives this call.
        let atom = unsafe { RegisterClassW(&definition) };
        if atom == 0 {
            // SAFETY: no arguments, no preconditions.
            let last_error = unsafe { GetLastError() };
            if last_error != ERROR_CLASS_ALREADY_EXISTS {
                return Err(anyhow::anyhow!(
                    "RegisterClassW({class}) failed with Win32 error {}",
                    last_error.0
                ));
            }
            log::debug!("the IPC window class {class} was already registered");
        }

        // A `WS_POPUP` top-level window is required so that another process can
        // find it with `FindWindowW`; `WS_EX_TOOLWINDOW` keeps it out of the
        // taskbar and the Alt+Tab list even if it were ever shown.
        // SAFETY: the class is registered and both string buffers are
        // NUL-terminated; the window is never shown.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR(class_w.as_ptr()),
                PCWSTR(title_w.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .context("CreateWindowExW for the IPC endpoint")?;
        Ok(window)
    }

    /// Standard `GetMessage` loop for the IPC thread.
    fn pump_messages() {
        let mut message = MSG::default();
        loop {
            // SAFETY: `message` is a valid out-pointer, `None` means "all
            // windows of this thread" and the 0/0 filter range means "no
            // filtering".
            let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
            // 0 is WM_QUIT and -1 is an error; both end the loop.
            if result.0 <= 0 {
                break;
            }
            // SAFETY: `message` was filled in by `GetMessageW` above.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// Window procedure of the hidden IPC window.
    ///
    /// Kept as a thin `extern "system"` shim so the real logic can live in a
    /// safe function with explicit `unsafe` blocks and SAFETY comments.
    ///
    /// # Safety
    ///
    /// Called by Windows with a valid `HWND` for our class.
    unsafe extern "system" fn window_procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        handle_window_message(hwnd, message, wparam, lparam)
    }

    /// Real implementation of the IPC window procedure.
    fn handle_window_message(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        use windows::Win32::Foundation::LRESULT;

        match message {
            WM_COPYDATA => {
                handle_copy_data(lparam);
                // A `WM_COPYDATA` receiver must answer with `TRUE` (1) to tell
                // the sender the payload was accepted.
                LRESULT(1)
            }
            // Private "please stop" message used by `AppInstance::shutdown`.
            _ if message == WM_APP + 1 => {
                // SAFETY: `PostQuitMessage` is thread-local and we are on the
                // thread that owns this window.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: as above.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            // SAFETY: `DefWindowProcW` is the documented default handler for any
            // message a window procedure does not process; `hwnd` is our window.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    /// Decode a `WM_COPYDATA` payload and hand it to the installed handler.
    fn handle_copy_data(lparam: LPARAM) {
        if lparam.0 == 0 {
            log::debug!("ignoring WM_COPYDATA without a payload");
            return;
        }
        // SAFETY: during `WM_COPYDATA` Windows guarantees `lparam` points at a
        // `COPYDATASTRUCT` that stays valid for the whole call.
        let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
        if data.dwData != IPC_MAGIC {
            log::warn!(
                "ignoring WM_COPYDATA with foreign tag {:#x}",
                data.dwData
            );
            return;
        }
        if data.lpData.is_null() || data.cbData == 0 {
            log::debug!("ignoring an empty WM_COPYDATA payload");
            return;
        }
        // SAFETY: `lpData`/`cbData` describe a readable buffer that the sender
        // keeps alive until we return from this message.
        let bytes = unsafe {
            std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize)
        };
        let bytes = super::trim_trailing_nul(bytes);
        match decode_message(bytes) {
            Some(message) => dispatch_to_handler(message),
            None => log::warn!("ignoring a malformed IPC payload ({} byte(s))", bytes.len()),
        }
    }

    /// See [`super::send_to_primary`].
    pub(super) fn send_to_primary(app_id: &str, msg: &IpcMessage) -> anyhow::Result<bool> {
        let class = window_class(app_id);
        let class_w = wide(&class);
        // SAFETY: `class_w` is NUL-terminated; a null window name means "any
        // window of that class".
        let window = match unsafe { FindWindowW(PCWSTR(class_w.as_ptr()), PCWSTR::null()) } {
            Ok(window) => window,
            Err(_) => {
                log::info!("no running primary instance found for {app_id}");
                return Ok(false);
            }
        };

        // Let the primary raise its own window; without this Windows would only
        // flash its taskbar button.
        // SAFETY: `ASFW_ANY` means "any process", so there is nothing to
        // validate; a failure is not fatal.
        if let Err(error) = unsafe { AllowSetForegroundWindow(ASFW_ANY) } {
            log::debug!("AllowSetForegroundWindow failed: {error}");
        }

        let payload = encode_message_nul(msg);
        let data = COPYDATASTRUCT {
            dwData: IPC_MAGIC,
            cbData: payload.len() as u32,
            lpData: payload.as_ptr() as *mut core::ffi::c_void,
        };
        let mut acknowledgement = 0usize;
        // SAFETY: `data` and the buffer it points at outlive this bounded
        // (never infinite) call; `SMTO_ABORTIFHUNG` additionally protects us
        // from a primary that stopped pumping its queue.
        let sent = unsafe {
            SendMessageTimeoutW(
                window,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&data as *const COPYDATASTRUCT as isize),
                SMTO_ABORTIFHUNG | SMTO_NORMAL,
                IPC_SEND_TIMEOUT_MS,
                Some(&mut acknowledgement),
            )
        };
        if sent.0 == 0 {
            // SAFETY: no arguments, no preconditions.
            let last_error = unsafe { GetLastError() };
            log::warn!(
                "the primary instance did not acknowledge the IPC message within {IPC_SEND_TIMEOUT_MS} ms (Win32 error {})",
                last_error.0
            );
            return Ok(false);
        }
        if acknowledgement == 0 {
            log::warn!("the primary instance rejected the IPC message");
            return Ok(false);
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_round_trips() {
        let encoded = encode_message_nul(&IpcMessage::Activate);
        assert_eq!(encoded.last(), Some(&0));
        assert_eq!(
            decode_message(trim_trailing_nul(&encoded)),
            Some(IpcMessage::Activate)
        );
    }

    #[test]
    fn quit_round_trips() {
        let encoded = encode_message_nul(&IpcMessage::Quit);
        assert_eq!(
            decode_message(trim_trailing_nul(&encoded)),
            Some(IpcMessage::Quit)
        );
    }

    #[test]
    fn open_paths_round_trip_including_unicode_and_spaces() {
        let paths = vec![
            PathBuf::from(r"C:\Users\张三\我的 视频\示例 文件.mkv"),
            PathBuf::from(r"\\server\share\a.mp4"),
            PathBuf::from("relative path.mp3"),
        ];
        let encoded = encode_message_nul(&IpcMessage::OpenPaths(paths.clone()));
        assert_eq!(
            decode_message(trim_trailing_nul(&encoded)),
            Some(IpcMessage::OpenPaths(paths))
        );
    }

    #[test]
    fn open_paths_with_no_files_is_still_valid() {
        let encoded = encode_message_nul(&IpcMessage::OpenPaths(Vec::new()));
        assert_eq!(
            decode_message(trim_trailing_nul(&encoded)),
            Some(IpcMessage::OpenPaths(Vec::new()))
        );
        // A blank line must not turn into an empty path.
        assert_eq!(
            decode_message(b"open\n\n"),
            Some(IpcMessage::OpenPaths(Vec::new()))
        );
    }

    #[test]
    fn payload_is_utf8_with_the_documented_layout() {
        let encoded = encode_message_nul(&IpcMessage::OpenPaths(vec![PathBuf::from(r"C:\a.mp4")]));
        let text = String::from_utf8(encoded).expect("payload must be UTF-8");
        assert_eq!(text, "open\nC:\\a.mp4\0");
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        assert_eq!(decode_message(b""), None);
        assert_eq!(decode_message(b"nonsense"), None);
        assert_eq!(decode_message(b"ACTIVATE"), None);
        assert_eq!(decode_message(&[0xff, 0xfe, 0xfd]), None);
    }

    #[test]
    fn trailing_nul_trimming() {
        assert_eq!(trim_trailing_nul(b"ab\0"), b"ab");
        assert_eq!(trim_trailing_nul(b"ab\0\0\0"), b"ab");
        assert_eq!(trim_trailing_nul(b"ab"), b"ab");
        assert_eq!(trim_trailing_nul(b"\0"), b"");
        assert_eq!(trim_trailing_nul(b""), b"");
    }

    /// End-to-end check of the mutex + hidden window + `WM_COPYDATA` path.
    ///
    /// Everything it creates is namespaced with a unique `app_id`, so it is safe
    /// to run concurrently with the other tests in this binary.
    #[cfg(windows)]
    #[test]
    fn primary_election_and_ipc_round_trip() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let app_id = format!("test-{unique}");

        let primary = AppInstance::acquire(&app_id)
            .expect("acquire must succeed")
            .expect("the first acquire must win the election");

        // A second acquire in the same session must see the mutex.
        assert!(
            AppInstance::acquire(&app_id)
                .expect("the second acquire must not fail")
                .is_none(),
            "a second instance must not become primary"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        primary.set_handler(move |message| {
            let _ = tx.send(message);
        });

        assert!(send_to_primary(&app_id, &IpcMessage::Activate).expect("send"));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("activate"),
            IpcMessage::Activate
        );

        let paths = vec![
            PathBuf::from(r"C:\temp\a b.mp4"),
            PathBuf::from(r"C:\temp\多功能.mkv"),
        ];
        assert!(
            send_to_primary(&app_id, &IpcMessage::OpenPaths(paths.clone())).expect("send paths")
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("paths"),
            IpcMessage::OpenPaths(paths)
        );

        assert!(send_to_primary(&app_id, &IpcMessage::Quit).expect("send"));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("quit"),
            IpcMessage::Quit
        );

        primary.shutdown();
        // Idempotent, and `Drop` will call it a third time.
        primary.shutdown();

        // The listener is gone, so a fresh launch must be allowed to start.
        assert!(!send_to_primary(&app_id, &IpcMessage::Activate).expect("send after shutdown"));
        assert!(
            AppInstance::acquire(&app_id)
                .expect("acquire after shutdown")
                .is_some(),
            "the mutex must be released by shutdown"
        );
    }
}

use std::{
    fmt,
    sync::{Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
};

use crate::modules::{Category, Module, ModuleInfo, ModuleState};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::KeyboardAndMouse::GetDoubleClickTime,
            Shell::{IShellDispatch, Shell as ShellComServer},
            WindowsAndMessaging::{
                CallNextHookEx, DispatchMessageW, GA_ROOT, GetAncestor, GetClassNameW, GetMessageW,
                GetParent, GetSystemMetrics, HC_ACTION, HHOOK, LLMHF_INJECTED,
                LLMHF_LOWER_IL_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW,
                PostThreadMessageW, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SetWindowsHookExW,
                TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP, WM_LBUTTONUP, WM_NULL,
                WM_QUIT, WindowFromPoint,
            },
        },
    },
    core::Error,
};

const MODULE_NAME: &str = "BetterTouchpad";
const STOP_MESSAGE: u32 = WM_APP + 0x4E60;

static HOOK_CONTEXT: OnceLock<Mutex<HookContext>> = OnceLock::new();

pub struct BetterTouchpad {
    info: ModuleInfo,
    state: ModuleState,
    hook: Option<BetterTouchpadHook>,
}

impl BetterTouchpad {
    pub fn new() -> Self {
        Self {
            info: ModuleInfo::new(
                MODULE_NAME,
                "Double-tap the Windows desktop to minimize all windows.",
                Category::Player,
            ),
            state: ModuleState::new(),
            hook: None,
        }
    }

    fn mark_disabled_after_start_failure(&mut self) {
        let key_bind = self.state.key_bind();
        let config_saving = self.state.config_saving();
        self.state = ModuleState::new();
        self.state.set_key_bind(key_bind);
        self.state.set_config_saving(config_saving);
    }
}

impl Default for BetterTouchpad {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BetterTouchpad {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn state(&self) -> &ModuleState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut ModuleState {
        &mut self.state
    }

    fn on_enable(&mut self) {
        if self.hook.is_some() {
            return;
        }

        match BetterTouchpadHook::start() {
            Ok(hook) => {
                self.hook = Some(hook);
                println!("BetterTouchpad enabled: desktop double-tap hook active.");
            }
            Err(error) => {
                self.mark_disabled_after_start_failure();
                eprintln!("BetterTouchpad failed to start desktop double-tap hook: {error}");
            }
        }
    }

    fn on_disable(&mut self) {
        if let Some(mut hook) = self.hook.take() {
            hook.stop();
            println!("BetterTouchpad disabled: desktop double-tap hook stopped.");
        }
    }
}

#[derive(Debug)]
enum HookError {
    AlreadyRunning,
    StartupFailed,
    ModuleHandle(Error),
    MouseHook(Error),
}

impl fmt::Display for HookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("hook is already running"),
            Self::StartupFailed => formatter.write_str("hook thread startup failed"),
            Self::ModuleHandle(error) => write!(formatter, "GetModuleHandleW failed: {error}"),
            Self::MouseHook(error) => write!(formatter, "SetWindowsHookExW failed: {error}"),
        }
    }
}

struct BetterTouchpadHook {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl BetterTouchpadHook {
    fn start() -> Result<Self, HookError> {
        let context = HOOK_CONTEXT.get_or_init(|| Mutex::new(HookContext::default()));
        {
            let mut context = context.lock().map_err(|_| HookError::StartupFailed)?;
            if context.running {
                return Err(HookError::AlreadyRunning);
            }
            *context = HookContext {
                running: true,
                last_tap: None,
            };
        }

        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = match thread::Builder::new()
            .name("nyx-better-touchpad-hook".to_owned())
            .spawn(move || run_hook_thread(startup_tx))
        {
            Ok(worker) => worker,
            Err(_) => {
                clear_hook_context();
                return Err(HookError::StartupFailed);
            }
        };

        match startup_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                clear_hook_context();
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                clear_hook_context();
                let _ = worker.join();
                Err(HookError::StartupFailed)
            }
        }
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, STOP_MESSAGE, WPARAM(0), LPARAM(0));
            }
            let _ = worker.join();
        }
    }
}

impl Drop for BetterTouchpadHook {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct HookContext {
    running: bool,
    last_tap: Option<TapSnapshot>,
}

#[derive(Clone, Copy)]
struct TapSnapshot {
    point: POINT,
    time: u32,
}

struct InstalledHooks {
    mouse: HHOOK,
}

impl InstalledHooks {
    fn install() -> Result<Self, HookError> {
        let hmodule = unsafe { GetModuleHandleW(None) }.map_err(HookError::ModuleHandle)?;
        let mouse = unsafe {
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), Some(hmodule.into()), 0)
        }
        .map_err(HookError::MouseHook)?;

        Ok(Self { mouse })
    }
}

impl Drop for InstalledHooks {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.mouse);
        }
    }
}

fn run_hook_thread(startup_tx: mpsc::Sender<Result<u32, HookError>>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut message = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut message, None, WM_NULL, WM_NULL, PM_NOREMOVE);
    }

    let hooks = match InstalledHooks::install() {
        Ok(hooks) => hooks,
        Err(error) => {
            let _ = startup_tx.send(Err(error));
            clear_hook_context();
            return;
        }
    };

    let _ = startup_tx.send(Ok(thread_id));
    message_loop();
    drop(hooks);
    clear_hook_context();
}

fn message_loop() {
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 || message.message == STOP_MESSAGE || message.message == WM_QUIT {
            break;
        }

        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam.0 as u32 == WM_LBUTTONUP {
        let hook_data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let flags = hook_data.flags;
        if flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) == 0 {
            handle_left_button_up(hook_data.pt, hook_data.time);
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn handle_left_button_up(point: POINT, time: u32) {
    if !is_desktop_point(point) {
        clear_last_tap();
        return;
    }

    if register_tap_and_check_double_tap(point, time) {
        spawn_minimize_all_worker();
    }
}

fn register_tap_and_check_double_tap(point: POINT, time: u32) -> bool {
    let Some(context) = HOOK_CONTEXT.get() else {
        return false;
    };
    let Ok(mut context) = context.lock() else {
        return false;
    };

    let is_double_tap = context
        .last_tap
        .is_some_and(|last| is_double_tap(last, TapSnapshot { point, time }));
    context.last_tap = if is_double_tap {
        None
    } else {
        Some(TapSnapshot { point, time })
    };

    is_double_tap
}

fn clear_last_tap() {
    if let Some(context) = HOOK_CONTEXT.get()
        && let Ok(mut context) = context.lock()
    {
        context.last_tap = None;
    }
}

fn is_double_tap(first: TapSnapshot, second: TapSnapshot) -> bool {
    let max_time = unsafe { GetDoubleClickTime() };
    if second.time.wrapping_sub(first.time) > max_time {
        return false;
    }

    let max_x = unsafe { GetSystemMetrics(SM_CXDOUBLECLK) }.max(1);
    let max_y = unsafe { GetSystemMetrics(SM_CYDOUBLECLK) }.max(1);
    (second.point.x - first.point.x).abs() <= max_x
        && (second.point.y - first.point.y).abs() <= max_y
}

fn spawn_minimize_all_worker() {
    let _ = thread::Builder::new()
        .name("nyx-better-touchpad-minimize".to_owned())
        .spawn(|| {
            if let Err(error) = minimize_all_windows() {
                eprintln!("BetterTouchpad failed to minimize all windows: {error}");
            }
        });
}

fn minimize_all_windows() -> windows::core::Result<()> {
    let com_initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = unsafe {
        CoCreateInstance::<_, IShellDispatch>(&ShellComServer, None, CLSCTX_INPROC_SERVER)
            .and_then(|shell| shell.MinimizeAll())
    };

    if com_initialized {
        unsafe {
            CoUninitialize();
        }
    }

    result
}

fn is_desktop_point(point: POINT) -> bool {
    let hwnd = unsafe { WindowFromPoint(point) };
    if hwnd.0.is_null() {
        return false;
    }

    if is_desktop_window(hwnd) {
        return true;
    }

    let mut has_desktop_view = false;
    let mut current = hwnd;
    for _ in 0..16 {
        let class_name = class_name(current);
        if is_desktop_view_class(&class_name) {
            has_desktop_view = true;
        }
        if has_desktop_view && is_desktop_container_class(&class_name) {
            return true;
        }

        match unsafe { GetParent(current) } {
            Ok(parent) if !parent.0.is_null() => current = parent,
            _ => break,
        }
    }

    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    has_desktop_view && !root.0.is_null() && is_desktop_container_class(&class_name(root))
}

fn is_desktop_window(hwnd: HWND) -> bool {
    let class_name = class_name(hwnd);
    is_desktop_container_class(&class_name) || is_desktop_view_class(&class_name)
}

fn is_desktop_container_class(class_name: &str) -> bool {
    matches!(class_name, "Progman" | "WorkerW" | "#32769")
}

fn is_desktop_view_class(class_name: &str) -> bool {
    matches!(class_name, "SHELLDLL_DefView" | "SysListView32")
}

fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..len as usize])
    }
}

fn clear_hook_context() {
    if let Some(context) = HOOK_CONTEXT.get()
        && let Ok(mut context) = context.lock()
    {
        context.running = false;
        context.last_tap = None;
    }
}

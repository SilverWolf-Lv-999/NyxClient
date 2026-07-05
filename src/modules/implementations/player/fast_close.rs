use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
};

use crate::{
    modules::{BaseValue, Category, Module, ModuleHandler, ModuleInfo, ModuleState},
    utility::process_utility::{self, ProcessKillLevel},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::{
            Accessibility::{CUIAutomation8, IUIAutomation, IUIAutomationElement},
            WindowsAndMessaging::{
                CallNextHookEx, DispatchMessageW, EnumWindows, GA_ROOT, GetAncestor, GetClassNameW,
                GetMessageW, GetParent, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
                HHOOK, IsWindowVisible, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW,
                PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
                WH_MOUSE_LL, WM_APP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_NULL, WM_QUIT,
                WindowFromPoint,
            },
        },
    },
    core::BOOL,
};

const MODULE_NAME: &str = "FastClose";
const KILL_LEVEL_VALUE_NAME: &str = "Kill Level";
const PROCESS_TREE_VALUE_NAME: &str = "Process Tree";
const KILL_LEVEL_MODES: [&str; 3] = ["Close", "Terminate", "Privileged"];
const DEFAULT_KILL_LEVEL: &str = "Privileged";
const STOP_MESSAGE: u32 = WM_APP + 0x4E59;

type SharedModuleHandler = Arc<Mutex<ModuleHandler>>;

static SHARED_MODULES: OnceLock<SharedModuleHandler> = OnceLock::new();
static HOOK_CONTEXT: OnceLock<Mutex<HookContext>> = OnceLock::new();

thread_local! {
    static UI_AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

pub fn set_shared_module_handler(modules: SharedModuleHandler) {
    let _ = SHARED_MODULES.set(modules);
}

pub struct FastClose {
    info: ModuleInfo,
    state: ModuleState,
    values: Vec<BaseValue>,
    hook: Option<FastCloseHook>,
}

impl FastClose {
    pub fn new() -> Self {
        Self {
            info: ModuleInfo::new(
                MODULE_NAME,
                "Middle-click taskbar app icons to close their processes.",
                Category::Player,
            ),
            state: ModuleState::new(),
            values: vec![
                BaseValue::mode(KILL_LEVEL_MODES, KILL_LEVEL_VALUE_NAME, DEFAULT_KILL_LEVEL),
                BaseValue::boolean(false, PROCESS_TREE_VALUE_NAME),
            ],
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

impl Default for FastClose {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FastClose {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn state(&self) -> &ModuleState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut ModuleState {
        &mut self.state
    }

    fn values(&self) -> &[BaseValue] {
        &self.values
    }

    fn values_mut(&mut self) -> &mut [BaseValue] {
        &mut self.values
    }

    fn on_enable(&mut self) {
        if self.hook.is_some() {
            return;
        }

        match FastCloseHook::start() {
            Ok(hook) => {
                self.hook = Some(hook);
                println!("FastClose enabled: taskbar middle-click hook active.");
            }
            Err(error) => {
                self.mark_disabled_after_start_failure();
                eprintln!("FastClose failed to start taskbar middle-click hook: {error}");
            }
        }
    }

    fn on_disable(&mut self) {
        if let Some(mut hook) = self.hook.take() {
            hook.stop();
            println!("FastClose disabled: taskbar middle-click hook stopped.");
        }
    }

    fn config_aliases(&self) -> &[&'static str] {
        &["SuperKill", "super_kill"]
    }

    fn normalize_config_value(&self, key: &str, value: &str) -> String {
        if key.eq_ignore_ascii_case(KILL_LEVEL_VALUE_NAME) || key.eq_ignore_ascii_case("kill_level")
        {
            if value.eq_ignore_ascii_case("Debug") || value.eq_ignore_ascii_case("Force") {
                return DEFAULT_KILL_LEVEL.to_owned();
            }
        }

        value.to_owned()
    }
}

#[derive(Clone, Copy, Debug)]
struct KillSettings {
    level: ProcessKillLevel,
    process_tree: bool,
}

impl Default for KillSettings {
    fn default() -> Self {
        Self {
            level: ProcessKillLevel::Privileged,
            process_tree: false,
        }
    }
}

fn kill_level_from_mode(mode: &str) -> ProcessKillLevel {
    if mode.eq_ignore_ascii_case("Close") {
        ProcessKillLevel::CloseWindows
    } else if mode.eq_ignore_ascii_case("Terminate") {
        ProcessKillLevel::Terminate
    } else {
        ProcessKillLevel::Privileged
    }
}

fn current_settings() -> KillSettings {
    let Some(modules) = SHARED_MODULES.get() else {
        return KillSettings::default();
    };

    let Ok(modules) = modules.lock() else {
        return KillSettings::default();
    };

    let Some(module) = modules.get(MODULE_NAME) else {
        return KillSettings::default();
    };

    let level = module
        .value(KILL_LEVEL_VALUE_NAME)
        .and_then(BaseValue::as_mode)
        .map(|value| kill_level_from_mode(value.current_mode()))
        .unwrap_or(ProcessKillLevel::Privileged);
    let process_tree = module
        .value(PROCESS_TREE_VALUE_NAME)
        .and_then(BaseValue::as_boolean)
        .is_some_and(|value| value.value());

    KillSettings {
        level,
        process_tree,
    }
}

#[derive(Debug)]
enum HookError {
    AlreadyRunning,
    StartupFailed,
    ModuleHandle(windows::core::Error),
    MouseHook(windows::core::Error),
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

struct FastCloseHook {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl FastCloseHook {
    fn start() -> Result<Self, HookError> {
        let context = HOOK_CONTEXT.get_or_init(|| Mutex::new(HookContext::default()));
        {
            let mut context = context.lock().map_err(|_| HookError::StartupFailed)?;
            if context.running {
                return Err(HookError::AlreadyRunning);
            }
            context.running = true;
            context.suppress_middle_up = false;
        }

        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = match thread::Builder::new()
            .name("nyx-fast-close-hook".to_owned())
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

impl Drop for FastCloseHook {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct HookContext {
    running: bool,
    suppress_middle_up: bool,
}

struct InstalledHooks {
    mouse: HHOOK,
}

impl InstalledHooks {
    fn install() -> Result<Self, HookError> {
        let hmodule = unsafe { GetModuleHandleW(None) }.map_err(HookError::ModuleHandle)?;
        let mouse = match unsafe {
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), Some(hmodule.into()), 0)
        } {
            Ok(mouse) => mouse,
            Err(error) => return Err(HookError::MouseHook(error)),
        };

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

    let com_initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    if com_initialized {
        let automation = unsafe {
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
        }
        .ok();
        UI_AUTOMATION.with(|slot| {
            *slot.borrow_mut() = automation;
        });
    }

    let hooks = match InstalledHooks::install() {
        Ok(hooks) => hooks,
        Err(error) => {
            cleanup_hook_thread(com_initialized);
            let _ = startup_tx.send(Err(error));
            clear_hook_context();
            return;
        }
    };

    let _ = startup_tx.send(Ok(thread_id));
    message_loop();
    drop(hooks);
    cleanup_hook_thread(com_initialized);
    clear_hook_context();
}

fn cleanup_hook_thread(com_initialized: bool) {
    UI_AUTOMATION.with(|slot| {
        *slot.borrow_mut() = None;
    });
    if com_initialized {
        unsafe {
            CoUninitialize();
        }
    }
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
    if code >= 0 && wparam.0 as u32 == WM_MBUTTONDOWN {
        let hook_data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if is_taskbar_app_point_fast(hook_data.pt) {
            set_suppress_middle_up();
            spawn_taskbar_middle_click_worker(hook_data.pt);
            return LRESULT(1);
        }
    }
    if code >= 0 && wparam.0 as u32 == WM_MBUTTONUP && take_suppress_middle_up() {
        return LRESULT(1);
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn set_suppress_middle_up() {
    if let Some(context) = HOOK_CONTEXT.get()
        && let Ok(mut context) = context.lock()
    {
        context.suppress_middle_up = true;
    }
}

fn take_suppress_middle_up() -> bool {
    let Some(context) = HOOK_CONTEXT.get() else {
        return false;
    };
    let Ok(mut context) = context.lock() else {
        return false;
    };
    let suppress = context.suppress_middle_up;
    context.suppress_middle_up = false;
    suppress
}

fn spawn_taskbar_middle_click_worker(point: POINT) {
    let point_x = point.x;
    let point_y = point.y;
    let _ = thread::Builder::new()
        .name("nyx-fast-close-worker".to_owned())
        .spawn(move || {
            handle_taskbar_middle_click(POINT {
                x: point_x,
                y: point_y,
            });
        });
}

fn handle_taskbar_middle_click(point: POINT) {
    let class_chain = class_chain_at_point(point);
    if !is_taskbar_point(point) {
        fast_close_debug_log(|| {
            format!(
                "middle-click ignored: point {},{} is not on taskbar; class chain: {}",
                point.x,
                point.y,
                class_chain.join(" -> ")
            )
        });
        return;
    }

    let explorer_pid = taskbar_explorer_pid_at_point(point).unwrap_or_default();
    let uia_names = uia_names_at_point(point);
    let target_pids = target_processes_at_taskbar_point(point, explorer_pid, &uia_names)
        .into_iter()
        .filter(|pid| !is_ignored_target_pid(*pid, explorer_pid))
        .collect::<Vec<_>>();

    if !target_pids.is_empty() {
        fast_close_debug_log(|| {
            format!(
                "taskbar middle-click: point {},{}, explorer_pid={}, target_pids={:?}, uia_names={:?}, class_chain={}",
                point.x,
                point.y,
                explorer_pid,
                target_pids,
                uia_names,
                class_chain.join(" -> ")
            )
        });
        kill_targets(target_pids);
        return;
    }

    fast_close_debug_log(|| {
        format!(
            "middle-click suppressed without kill target at {},{}; explorer_pid={}, uia_names={:?}, class_chain={}",
            point.x,
            point.y,
            explorer_pid,
            uia_names,
            class_chain.join(" -> ")
        )
    });
}

fn clear_hook_context() {
    if let Some(context) = HOOK_CONTEXT.get() {
        if let Ok(mut context) = context.lock() {
            context.running = false;
            context.suppress_middle_up = false;
        }
    }
}

fn kill_targets(target_pids: Vec<u32>) {
    let settings = current_settings();
    let kill_pids = target_pids
        .iter()
        .copied()
        .map(|target_pid| {
            if settings.process_tree {
                process_utility::process_tree_root_for_termination(target_pid)
            } else {
                target_pid
            }
        })
        .filter(|pid| *pid != 0)
        .collect::<Vec<_>>();
    let kill_pids = unique_pids(kill_pids);

    fast_close_debug_log(|| {
        format!(
            "kill request: target_pids={:?}, kill_pids={:?}, level={:?}, process_tree={}",
            target_pids, kill_pids, settings.level, settings.process_tree
        )
    });

    let mut failures = Vec::new();
    for kill_pid in kill_pids {
        if let Err(error) =
            process_utility::kill_process(kill_pid, settings.level, settings.process_tree)
        {
            failures.push(format!("{kill_pid}: {error}"));
        }
    }

    if !failures.is_empty() {
        eprintln!(
            "FastClose failed to terminate process target(s): {}",
            failures.join("; ")
        );
    }
}

fn is_explorer_process(pid: u32) -> bool {
    process_utility::is_process_named(pid, "explorer.exe")
}

fn is_ignored_target_pid(pid: u32, explorer_pid: u32) -> bool {
    pid == 0
        || pid == explorer_pid
        || pid == unsafe { GetCurrentProcessId() }
        || is_explorer_process(pid)
        || process_utility::is_protected_process(pid)
}

fn target_processes_at_taskbar_point(
    point: POINT,
    explorer_pid: u32,
    uia_names: &[String],
) -> Vec<u32> {
    let native_pids = uia_native_target_pids(point, explorer_pid);
    if !native_pids.is_empty() {
        return native_pids;
    }

    if uia_names.is_empty() {
        return Vec::new();
    }

    let windows = visible_window_candidates();
    unique_pids(
        uia_names
            .iter()
            .flat_map(|name| match_taskbar_name_to_pids(name, &windows))
            .collect(),
    )
}

fn uia_native_target_pids(point: POINT, explorer_pid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    walk_uia_elements_at_point(point, |element| {
        let hwnd = unsafe { element.CurrentNativeWindowHandle().ok()? };
        if hwnd.0.is_null() {
            return None;
        }

        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if !is_ignored_target_pid(pid, explorer_pid) && !pids.contains(&pid) {
            pids.push(pid);
        }
        None::<()>
    });
    pids
}

fn uia_names_at_point(point: POINT) -> Vec<String> {
    let mut names = Vec::new();
    walk_uia_elements_at_point(point, |element| {
        if let Ok(name) = unsafe { element.CurrentName() } {
            let name = name.to_string();
            if !name.trim().is_empty() && !names.iter().any(|existing| existing == &name) {
                names.push(name);
            }
        }
        None::<()>
    });
    names
}

fn walk_uia_elements_at_point<T>(
    point: POINT,
    mut visit: impl FnMut(&IUIAutomationElement) -> Option<T>,
) -> Option<T> {
    UI_AUTOMATION.with(|slot| {
        let automation = slot.borrow();
        let automation = automation.as_ref()?;
        let mut element = unsafe { automation.ElementFromPoint(point).ok()? };
        let walker = unsafe { automation.RawViewWalker().ok() };

        for _ in 0..8 {
            if let Some(value) = visit(&element) {
                return Some(value);
            }

            let Some(walker) = &walker else {
                break;
            };
            let Ok(parent) = (unsafe { walker.GetParentElement(&element) }) else {
                break;
            };
            element = parent;
        }

        None
    })
}

#[derive(Clone)]
struct WindowCandidate {
    pid: u32,
    title: String,
    exe_name: String,
}

fn visible_window_candidates() -> Vec<WindowCandidate> {
    let process_names = process_utility::snapshot_processes()
        .into_iter()
        .map(|entry| (entry.pid, entry.exe_name))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::<WindowCandidate>::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_visible_windows_proc),
            LPARAM((&mut candidates as *mut Vec<WindowCandidate>) as isize),
        );
    }
    let current_pid = unsafe { GetCurrentProcessId() };
    for candidate in &mut candidates {
        candidate.exe_name = process_names
            .get(&candidate.pid)
            .cloned()
            .unwrap_or_else(|| format!("pid-{}", candidate.pid));
    }
    candidates.retain(|candidate| {
        candidate.pid != current_pid && !process_utility::is_protected_process(candidate.pid)
    });
    candidates
}

unsafe extern "system" fn enum_visible_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return true.into();
    }

    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid != 0 {
        let candidates = unsafe { &mut *(lparam.0 as *mut Vec<WindowCandidate>) };
        candidates.push(WindowCandidate {
            pid,
            title: window_text(hwnd),
            exe_name: String::new(),
        });
    }

    true.into()
}

fn match_taskbar_name_to_pids(taskbar_name: &str, windows: &[WindowCandidate]) -> Vec<u32> {
    let names = taskbar_app_name_candidates(taskbar_name);
    if names.is_empty() {
        return Vec::new();
    }

    for name in &names {
        let exact = windows
            .iter()
            .filter(|window| normalize_title(&window.title) == *name)
            .map(|window| window.pid)
            .collect::<Vec<_>>();
        let exact = unique_pids(exact);
        if !exact.is_empty() {
            return exact;
        }
    }

    for name in &names {
        let contains = windows
            .iter()
            .filter(|window| {
                let title = normalize_title(&window.title);
                title.len() >= 3 && (title.contains(name) || name.contains(&title))
            })
            .map(|window| window.pid)
            .collect::<Vec<_>>();
        let contains = unique_pids(contains);
        if !contains.is_empty() {
            return contains;
        }
    }

    for name in &names {
        let exe_matches = windows
            .iter()
            .filter_map(|window| {
                let exe_stem = normalize_exe_stem(&window.exe_name);
                if exe_stem.len() >= 3 && (name.contains(&exe_stem) || exe_stem.contains(name)) {
                    Some(window.pid)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let exe_matches = unique_pids(exe_matches);
        if !exe_matches.is_empty() {
            return exe_matches;
        }
    }

    let process_entries = process_utility::snapshot_processes();
    for name in &names {
        let process_matches = process_entries
            .iter()
            .filter_map(|process| {
                if process_utility::is_protected_process_name(&process.exe_name) {
                    return None;
                }

                let exe_stem = normalize_exe_stem(&process.exe_name);
                if exe_stem.len() >= 3 && (name.contains(&exe_stem) || exe_stem.contains(name)) {
                    Some(process.pid)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let process_matches = unique_pids(process_matches);
        if !process_matches.is_empty() {
            return process_matches;
        }
    }

    Vec::new()
}

fn unique_pids(pids: Vec<u32>) -> Vec<u32> {
    let mut seen = HashSet::new();
    pids.into_iter()
        .filter(|pid| *pid != 0 && seen.insert(*pid))
        .collect()
}

fn taskbar_app_name_candidates(name: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    push_candidate(&mut candidates, clean_taskbar_app_name(name));

    for separator in [",", "，", " - ", " – ", " — "] {
        if let Some((left, _)) = name.split_once(separator) {
            push_candidate(&mut candidates, clean_taskbar_app_name(left));
        }
    }

    candidates
}

fn is_taskbar_app_point_fast(point: POINT) -> bool {
    let hwnd = unsafe { WindowFromPoint(point) };
    if hwnd.0.is_null() {
        return false;
    }

    let mut saw_taskbar_shell = false;
    let mut saw_app_surface = false;
    visit_window_class_chain(hwnd, |class_name| {
        saw_taskbar_shell |= is_taskbar_shell_class(class_name);
        saw_app_surface |= is_taskbar_app_surface_class(class_name);
    });

    saw_taskbar_shell && saw_app_surface
}

fn visit_window_class_chain(hwnd: HWND, mut visit: impl FnMut(&str)) {
    let mut current = hwnd;
    for _ in 0..16 {
        let current_class = class_name(current);
        visit(&current_class);

        match unsafe { GetParent(current) } {
            Ok(parent) if !parent.0.is_null() => current = parent,
            _ => break,
        }
    }

    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if !root.0.is_null() && root != hwnd {
        let root_class = class_name(root);
        visit(&root_class);
    }
}

fn is_taskbar_shell_class(class_name: &str) -> bool {
    matches!(class_name, "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

fn is_taskbar_app_surface_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "MSTaskSwWClass"
            | "MSTaskListWClass"
            | "TaskListThumbnailWnd"
            | "Windows.UI.Composition.DesktopWindowContentBridge"
            | "XamlExplorerHostIslandWindow"
    )
}

fn push_candidate(candidates: &mut Vec<String>, candidate: String) {
    if candidate.len() >= 2 && !candidates.iter().any(|existing| existing == &candidate) {
        candidates.push(candidate);
    }
}

fn clean_taskbar_app_name(name: &str) -> String {
    let mut text = normalize_title(name);
    for noise in [
        "running windows",
        "running window",
        "open windows",
        "open window",
        "button",
        "taskbar",
        "pinned",
        "unpinned",
        "pin to taskbar",
        "unpin from taskbar",
        "windows",
        "window",
        "running",
        "opened",
        "open",
        "按钮",
        "任务栏",
        "固定到任务栏",
        "从任务栏取消固定",
        "已固定",
        "取消固定",
        "正在运行的窗口",
        "正在运行窗口",
        "正在运行",
        "运行窗口",
        "运行的窗口",
        "运行",
        "已打开的窗口",
        "已打开窗口",
        "已打开",
        "个窗口",
        "窗口",
        "个",
    ] {
        text = text.replace(noise, " ");
    }

    let cleaned = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_digit()
                || matches!(
                    ch,
                    ',' | '，'
                        | '.'
                        | '。'
                        | ':'
                        | '：'
                        | ';'
                        | '；'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                        | '['
                        | ']'
                        | '【'
                        | '】'
                        | '-'
                        | '–'
                        | '—'
                )
            {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>();

    normalize_title(&cleaned)
}

fn normalize_exe_stem(exe_name: &str) -> String {
    normalize_title(exe_name.trim_end_matches(".exe"))
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_taskbar_point(point: POINT) -> bool {
    taskbar_window_at_point(point).is_some()
}

fn taskbar_explorer_pid_at_point(point: POINT) -> Option<u32> {
    taskbar_window_at_point(point)
        .and_then(window_process_id)
        .filter(|pid| is_explorer_process(*pid))
}

fn taskbar_window_at_point(point: POINT) -> Option<HWND> {
    let hwnd = unsafe { WindowFromPoint(point) };
    if !hwnd.0.is_null()
        && let Some(taskbar) = taskbar_ancestor_at_point(hwnd, point)
    {
        return Some(taskbar);
    }

    taskbar_top_level_window_at_point(point)
}

fn taskbar_ancestor_at_point(hwnd: HWND, point: POINT) -> Option<HWND> {
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if !root.0.is_null() && is_taskbar_window_at_point(root, point) {
        return Some(root);
    }

    let mut current = hwnd;
    for _ in 0..16 {
        if is_taskbar_window_at_point(current, point) {
            return Some(current);
        }

        match unsafe { GetParent(current) } {
            Ok(parent) if !parent.0.is_null() => current = parent,
            _ => break,
        }
    }

    None
}

struct TaskbarWindowSearch {
    point: POINT,
    hwnd: HWND,
}

fn taskbar_top_level_window_at_point(point: POINT) -> Option<HWND> {
    let mut search = TaskbarWindowSearch {
        point,
        hwnd: HWND::default(),
    };

    unsafe {
        let _ = EnumWindows(
            Some(enum_taskbar_window_proc),
            LPARAM((&mut search as *mut TaskbarWindowSearch) as isize),
        );
    }

    if search.hwnd.0.is_null() {
        None
    } else {
        Some(search.hwnd)
    }
}

unsafe extern "system" fn enum_taskbar_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = unsafe { &mut *(lparam.0 as *mut TaskbarWindowSearch) };
    if is_taskbar_window_at_point(hwnd, search.point) {
        search.hwnd = hwnd;
        return false.into();
    }

    true.into()
}

fn is_taskbar_window_at_point(hwnd: HWND, point: POINT) -> bool {
    if hwnd.0.is_null() {
        return false;
    }

    let class = class_name(hwnd);
    if is_taskbar_class(&class) {
        return true;
    }

    is_explorer_edge_window_at_point(hwnd, point)
}

fn is_explorer_edge_window_at_point(hwnd: HWND, point: POINT) -> bool {
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return false;
    }

    let Some(pid) = window_process_id(hwnd) else {
        return false;
    };
    if !is_explorer_process(pid) {
        return false;
    }

    let Some(rect) = window_rect(hwnd) else {
        return false;
    };
    rect_contains_point(rect, point) && is_screen_edge_band(rect)
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(rect)
}

fn rect_contains_point(rect: RECT, point: POINT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

fn is_screen_edge_band(rect: RECT) -> bool {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return false;
    }

    const MAX_TASKBAR_THICKNESS: i32 = 160;
    const MIN_TASKBAR_LENGTH: i32 = 240;
    (height <= MAX_TASKBAR_THICKNESS && width >= MIN_TASKBAR_LENGTH)
        || (width <= MAX_TASKBAR_THICKNESS && height >= MIN_TASKBAR_LENGTH)
}

fn class_chain_at_point(point: POINT) -> Vec<String> {
    let hwnd = unsafe { WindowFromPoint(point) };
    if hwnd.0.is_null() {
        return vec!["<none>".to_owned()];
    }

    let mut chain = Vec::new();
    let mut current = hwnd;
    for _ in 0..16 {
        let class_name = class_name(current);
        let pid = window_process_id(current).unwrap_or_default();
        chain.push(format!("{class_name}(pid={pid})"));

        match unsafe { GetParent(current) } {
            Ok(parent) if !parent.0.is_null() => current = parent,
            _ => break,
        }
    }

    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if !root.0.is_null() && root != hwnd {
        let root_class = class_name(root);
        let root_pid = window_process_id(root).unwrap_or_default();
        chain.push(format!("root:{root_class}(pid={root_pid})"));
    }

    chain
}

fn is_taskbar_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "MSTaskSwWClass"
            | "MSTaskListWClass"
            | "TaskListThumbnailWnd"
            | "Windows.UI.Composition.DesktopWindowContentBridge"
            | "XamlExplorerHostIslandWindow"
    )
}

fn window_process_id(hwnd: HWND) -> Option<u32> {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 { None } else { Some(pid) }
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

fn window_text(hwnd: HWND) -> String {
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..len as usize])
    }
}

fn fast_close_debug_log(message: impl FnOnce() -> String) {
    if fast_close_debug_enabled() {
        eprintln!("FastClose debug: {}", message());
    }
}

fn fast_close_debug_enabled() -> bool {
    std::env::var_os("NYX_FAST_CLOSE_DEBUG").is_some()
        || std::env::var_os("NYX_SUPER_KILL_DEBUG").is_some()
}

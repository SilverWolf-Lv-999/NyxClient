use std::{
    collections::{HashMap, HashSet},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    modules::{BaseValue, Category, Module, ModuleInfo, ModuleState},
    utility::process_utility::{
        ProcessSnapshotEntry, force_terminate_process_tree, snapshot_processes,
    },
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        System::Threading::GetCurrentProcessId,
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
        },
    },
    core::BOOL,
};

const MODULE_NAME: &str = "KillAura";
const WINDOW_LIMIT_VALUE_NAME: &str = "Window Limit";
const TIME_WINDOW_VALUE_NAME: &str = "Time Window Seconds";
const SCAN_INTERVAL_VALUE_NAME: &str = "Scan Interval Ms";
const MAX_WIDTH_VALUE_NAME: &str = "Max Window Width";
const MAX_HEIGHT_VALUE_NAME: &str = "Max Window Height";

const DEFAULT_WINDOW_LIMIT: usize = 10;
const DEFAULT_TIME_WINDOW: Duration = Duration::from_secs(3);
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_MAX_WINDOW_WIDTH: i32 = 420;
const DEFAULT_MAX_WINDOW_HEIGHT: i32 = 320;
const MIN_WINDOW_WIDTH: i32 = 32;
const MIN_WINDOW_HEIGHT: i32 = 32;

pub struct KillAura {
    info: ModuleInfo,
    state: ModuleState,
    values: Vec<BaseValue>,
    monitor: Option<KillAuraMonitor>,
}

impl KillAura {
    pub fn new() -> Self {
        Self {
            info: ModuleInfo::new(
                MODULE_NAME,
                "Terminates processes that rapidly create many small windows.",
                Category::Combat,
            ),
            state: ModuleState::new(),
            values: vec![
                BaseValue::number(
                    DEFAULT_WINDOW_LIMIT as f64,
                    2.0,
                    100.0,
                    WINDOW_LIMIT_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_TIME_WINDOW.as_secs_f64(),
                    0.5,
                    30.0,
                    TIME_WINDOW_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_SCAN_INTERVAL.as_millis() as f64,
                    25.0,
                    1000.0,
                    SCAN_INTERVAL_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_MAX_WINDOW_WIDTH as f64,
                    80.0,
                    2000.0,
                    MAX_WIDTH_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_MAX_WINDOW_HEIGHT as f64,
                    80.0,
                    1600.0,
                    MAX_HEIGHT_VALUE_NAME,
                ),
            ],
            monitor: None,
        }
    }

    fn settings(&self) -> MonitorSettings {
        MonitorSettings {
            window_limit: number_value(self, WINDOW_LIMIT_VALUE_NAME, DEFAULT_WINDOW_LIMIT as f64)
                .round()
                .clamp(1.0, 1000.0) as usize,
            time_window: Duration::from_secs_f64(
                number_value(
                    self,
                    TIME_WINDOW_VALUE_NAME,
                    DEFAULT_TIME_WINDOW.as_secs_f64(),
                )
                .clamp(0.1, 300.0),
            ),
            scan_interval: Duration::from_millis(
                number_value(
                    self,
                    SCAN_INTERVAL_VALUE_NAME,
                    DEFAULT_SCAN_INTERVAL.as_millis() as f64,
                )
                .round()
                .clamp(10.0, 5000.0) as u64,
            ),
            max_width: number_value(self, MAX_WIDTH_VALUE_NAME, DEFAULT_MAX_WINDOW_WIDTH as f64)
                .round()
                .clamp(MIN_WINDOW_WIDTH as f64, 4000.0) as i32,
            max_height: number_value(
                self,
                MAX_HEIGHT_VALUE_NAME,
                DEFAULT_MAX_WINDOW_HEIGHT as f64,
            )
            .round()
            .clamp(MIN_WINDOW_HEIGHT as f64, 4000.0) as i32,
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

impl Default for KillAura {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for KillAura {
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
        if self.monitor.is_some() {
            return;
        }

        match KillAuraMonitor::start(self.settings()) {
            Ok(monitor) => {
                self.monitor = Some(monitor);
                println!("KillAura enabled: small-window burst monitor active.");
            }
            Err(error) => {
                self.mark_disabled_after_start_failure();
                eprintln!("KillAura failed to start monitor: {error}");
            }
        }
    }

    fn on_disable(&mut self) {
        if let Some(mut monitor) = self.monitor.take() {
            monitor.stop();
            println!("KillAura disabled: small-window burst monitor stopped.");
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MonitorSettings {
    window_limit: usize,
    time_window: Duration,
    scan_interval: Duration,
    max_width: i32,
    max_height: i32,
}

struct KillAuraMonitor {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl KillAuraMonitor {
    fn start(settings: MonitorSettings) -> Result<Self, String> {
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_running = std::sync::Arc::clone(&running);
        let worker = thread::Builder::new()
            .name("nyx-kill-aura-monitor".to_owned())
            .spawn(move || run_monitor(worker_running, settings))
            .map_err(|error| format!("failed to spawn monitor thread: {error}"))?;

        Ok(Self {
            running,
            worker: Some(worker),
        })
    }

    fn stop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for KillAuraMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_monitor(running: std::sync::Arc<std::sync::atomic::AtomicBool>, settings: MonitorSettings) {
    let current_pid = unsafe { GetCurrentProcessId() };
    let mut seen_windows = current_small_windows(&settings, current_pid)
        .into_iter()
        .map(|window| window.hwnd)
        .collect::<HashSet<_>>();
    let mut bursts = HashMap::<String, Vec<WindowBurstEvent>>::new();

    while running.load(std::sync::atomic::Ordering::Acquire) {
        thread::sleep(settings.scan_interval);

        let now = Instant::now();
        retain_recent_bursts(&mut bursts, now, settings.time_window);

        let windows = current_small_windows(&settings, current_pid);
        let active_windows = windows
            .iter()
            .map(|window| window.hwnd)
            .collect::<HashSet<_>>();
        seen_windows.retain(|hwnd| active_windows.contains(hwnd));

        for window in windows {
            if !seen_windows.insert(window.hwnd) {
                continue;
            }

            let events = bursts.entry(window.program_key.clone()).or_default();
            events.push(WindowBurstEvent {
                pid: window.pid,
                created_at: now,
            });
            events.retain(|event| now.duration_since(event.created_at) <= settings.time_window);

            if events.len() >= settings.window_limit {
                let event_count = events.len();
                let target_pids = unique_event_pids(events);
                let target_roots = target_tree_roots(&target_pids, current_pid);

                if let Err(error) = force_terminate_process_trees(&target_roots) {
                    eprintln!(
                        "KillAura failed to terminate {} process tree(s) for {} after {} small windows: {error}",
                        target_roots.len(),
                        window.exe_name,
                        event_count
                    );
                } else {
                    println!(
                        "KillAura terminated {} process tree(s) for {} after {} small windows in {:.2}s.",
                        target_roots.len(),
                        window.exe_name,
                        event_count,
                        settings.time_window.as_secs_f64()
                    );
                }
                events.clear();
            }
        }
    }
}

fn retain_recent_bursts(
    bursts: &mut HashMap<String, Vec<WindowBurstEvent>>,
    now: Instant,
    time_window: Duration,
) {
    bursts.retain(|_, events| {
        events.retain(|event| now.duration_since(event.created_at) <= time_window);
        !events.is_empty()
    });
}

#[derive(Clone, Copy, Debug)]
struct WindowBurstEvent {
    pid: u32,
    created_at: Instant,
}

fn unique_event_pids(events: &[WindowBurstEvent]) -> Vec<u32> {
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(|event| {
            if seen.insert(event.pid) {
                Some(event.pid)
            } else {
                None
            }
        })
        .collect()
}

fn target_tree_roots(pids: &[u32], current_pid: u32) -> Vec<u32> {
    let process_entries = snapshot_processes()
        .into_iter()
        .map(|entry| (entry.pid, entry))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();

    pids.iter()
        .filter_map(|pid| {
            let root_pid = target_tree_root(*pid, current_pid, &process_entries);
            if seen.insert(root_pid) {
                Some(root_pid)
            } else {
                None
            }
        })
        .collect()
}

fn target_tree_root(
    pid: u32,
    current_pid: u32,
    process_entries: &HashMap<u32, ProcessSnapshotEntry>,
) -> u32 {
    let mut selected_pid = pid;
    let mut cursor_pid = pid;
    let mut visited = HashSet::new();

    for _ in 0..16 {
        if !visited.insert(cursor_pid) {
            break;
        }

        let Some(entry) = process_entries.get(&cursor_pid) else {
            break;
        };
        let parent_pid = entry.parent_pid;
        if parent_pid == 0 || parent_pid == current_pid || parent_pid == cursor_pid {
            break;
        }

        let Some(parent) = process_entries.get(&parent_pid) else {
            break;
        };
        if is_protected_process_name(&parent.exe_name) {
            break;
        }

        if is_command_or_script_host(&parent.exe_name) {
            selected_pid = parent_pid;
            cursor_pid = parent_pid;
        } else {
            break;
        }
    }

    selected_pid
}

fn force_terminate_process_trees(pids: &[u32]) -> Result<(), String> {
    let mut failures = Vec::new();
    for pid in pids {
        if let Err(error) = force_terminate_process_tree(*pid) {
            failures.push(format!("{pid}: {error}"));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn current_small_windows(settings: &MonitorSettings, current_pid: u32) -> Vec<WindowSnapshot> {
    let process_names = snapshot_processes()
        .into_iter()
        .map(|entry| (entry.pid, entry.exe_name))
        .collect::<HashMap<_, _>>();

    visible_windows()
        .into_iter()
        .filter_map(|mut window| {
            if window.pid == 0
                || window.pid == current_pid
                || !is_small_window(&window, settings)
                || is_ignored_window_class(&window.class_name)
            {
                return None;
            }

            let exe_name = process_names
                .get(&window.pid)
                .cloned()
                .unwrap_or_else(|| format!("pid-{}", window.pid));
            if is_protected_process_name(&exe_name) {
                return None;
            }

            window.program_key = process_key(&exe_name);
            window.exe_name = exe_name;
            Some(window)
        })
        .collect()
}

fn visible_windows() -> Vec<WindowSnapshot> {
    let mut windows = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_visible_windows_proc),
            LPARAM((&mut windows as *mut Vec<WindowSnapshot>) as isize),
        );
    }
    windows
}

unsafe extern "system" fn enum_visible_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return true.into();
    }

    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return true.into();
    }

    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return true.into();
    }

    let windows = unsafe { &mut *(lparam.0 as *mut Vec<WindowSnapshot>) };
    windows.push(WindowSnapshot {
        hwnd: hwnd.0 as isize,
        pid,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
        class_name: class_name(hwnd),
        exe_name: String::new(),
        program_key: String::new(),
    });

    true.into()
}

#[derive(Clone, Debug)]
struct WindowSnapshot {
    hwnd: isize,
    pid: u32,
    width: i32,
    height: i32,
    class_name: String,
    exe_name: String,
    program_key: String,
}

fn is_small_window(window: &WindowSnapshot, settings: &MonitorSettings) -> bool {
    window.width >= MIN_WINDOW_WIDTH
        && window.height >= MIN_WINDOW_HEIGHT
        && window.width <= settings.max_width
        && window.height <= settings.max_height
}

fn is_ignored_window_class(class_name: &str) -> bool {
    let class_name = class_name.trim().to_ascii_lowercase();
    matches!(
        class_name.as_str(),
        "#32768"
            | "tooltips_class32"
            | "sysshadow"
            | "ime"
            | "default ime"
            | "msctfime ui"
            | "shell_traywnd"
            | "shell_secondarytraywnd"
            | "mstasklistwclass"
            | "tasklistthumbnailwnd"
    )
}

fn is_protected_process_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "explorer.exe"
            | "shellexperiencehost.exe"
            | "startmenuexperiencehost.exe"
            | "searchhost.exe"
            | "textinputhost.exe"
            | "applicationframehost.exe"
            | "dwm.exe"
            | "ctfmon.exe"
    )
}

fn is_command_or_script_host(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "wscript.exe"
            | "cscript.exe"
            | "mshta.exe"
            | "conhost.exe"
    )
}

fn process_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
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

fn number_value(module: &KillAura, key: &str, fallback: f64) -> f64 {
    module
        .value(key)
        .and_then(BaseValue::as_number)
        .map_or(fallback, |value| value.double_value())
}

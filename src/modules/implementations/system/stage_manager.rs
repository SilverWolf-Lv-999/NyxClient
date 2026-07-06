use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    fmt,
    mem::size_of,
    ptr::{self, null_mut},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::modules::{BaseValue, Category, Module, ModuleHandler, ModuleInfo, ModuleState};
use skija::{
    AlphaType, Canvas, Color as SkColor, Data, FilterMode, Font, FontMgr, FontStyle, Image,
    ImageInfo, Paint, PaintStyle, RRect, Rect as SkRect, SamplingOptions, Typeface,
    canvas::SrcRectConstraint, font_style, surfaces,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt,
            CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
            DeleteObject, GetDC, GetMonitorInfoW, GetWindowDC, HBITMAP, HDC, HGDIOBJ,
            MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
            ReleaseDC, SRCCOPY, SelectObject,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                CHILDID_SELF, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW,
                DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_CREATE,
                EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_FOREGROUND,
                EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetAncestor,
                GetClassNameW, GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindow,
                GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
                HTCLIENT, HTTRANSPARENT, IDC_ARROW, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
                KillTimer, LoadCursorW, MA_NOACTIVATE, MSG, OBJID_CLIENT, OBJID_WINDOW,
                PM_NOREMOVE, PeekMessageW, PostMessageW, PostQuitMessage, PostThreadMessageW,
                RegisterClassW, RegisterWindowMessageW, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE,
                SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetForegroundWindow, SetTimer,
                SetWindowLongPtrW, SetWindowPos, ShowWindow, ShowWindowAsync, TranslateMessage,
                ULW_ALPHA, UpdateLayeredWindow, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
                WM_APP, WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_NCCREATE,
                WM_NCDESTROY, WM_NCHITTEST, WM_NULL, WM_QUIT, WM_TIMER, WNDCLASSW, WS_CHILD,
                WS_DISABLED, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{BOOL, PCWSTR, w},
};

const MODULE_NAME: &str = "StageManager";
const RIGHT_SIDE_VALUE_NAME: &str = "Right Side";
const WINDOW_WIDTH_VALUE_NAME: &str = "Window Width";
const WINDOW_HEIGHT_VALUE_NAME: &str = "Window Height";
const RAIL_WIDTH_VALUE_NAME: &str = "Rail Width";
const THUMBNAIL_WIDTH_VALUE_NAME: &str = "Thumbnail Width";
const THUMBNAIL_HEIGHT_VALUE_NAME: &str = "Thumbnail Height";
const MAX_THUMBNAILS_VALUE_NAME: &str = "Max Thumbnails";

const DEFAULT_WINDOW_WIDTH: i32 = 1280;
const DEFAULT_WINDOW_HEIGHT: i32 = 820;
const DEFAULT_RAIL_WIDTH: i32 = 286;
const DEFAULT_THUMBNAIL_WIDTH: i32 = 220;
const DEFAULT_THUMBNAIL_HEIGHT: i32 = 136;
const DEFAULT_MAX_THUMBNAILS: usize = 7;
const MIN_WINDOW_WIDTH: i32 = 480;
const MIN_WINDOW_HEIGHT: i32 = 300;
const MIN_THUMBNAIL_WIDTH: i32 = 120;
const MIN_THUMBNAIL_HEIGHT: i32 = 72;
const MAX_WINDOW_WIDTH: i32 = 4096;
const MAX_WINDOW_HEIGHT: i32 = 2400;
const MAX_THUMBNAILS: usize = 12;
const LAYOUT_MARGIN: i32 = 30;
const THUMBNAIL_PADDING: f32 = 7.0;
const THUMBNAIL_TITLE_HEIGHT: f32 = 28.0;
const THUMBNAIL_RADIUS: f32 = 8.0;
const MIN_CANDIDATE_WIDTH: i32 = 80;
const MIN_CANDIDATE_HEIGHT: i32 = 64;
const FULLSCREEN_TOLERANCE: i32 = 3;
const LAYOUT_TIMER_ID: usize = 1;
const LAYOUT_POLL_MS: u32 = 120;
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(850);
const STOP_MESSAGE: u32 = WM_APP + 0x5A31;
const WAKE_MESSAGE: u32 = WM_APP + 0x5A32;
const STARTING_HWND: isize = -1;

type SharedModuleHandler = Arc<Mutex<ModuleHandler>>;

static SHARED_MODULES: OnceLock<SharedModuleHandler> = OnceLock::new();
static STAGE_CONTEXT: OnceLock<Mutex<Option<StageHookContext>>> = OnceLock::new();
static STAGE_HOOK_MESSAGE: OnceLock<u32> = OnceLock::new();
static START_REQUESTED: AtomicBool = AtomicBool::new(false);
static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn set_shared_module_handler(modules: SharedModuleHandler) {
    let _ = SHARED_MODULES.set(modules);
}

pub struct StageManager {
    info: ModuleInfo,
    state: ModuleState,
    values: Vec<BaseValue>,
    runtime: Option<StageManagerRuntime>,
}

impl StageManager {
    pub fn new() -> Self {
        Self {
            info: ModuleInfo::new(
                MODULE_NAME,
                "Arranges the foreground window and draws a Skia stage thumbnail rail.",
                Category::System,
            ),
            state: ModuleState::new(),
            values: vec![
                BaseValue::boolean(false, RIGHT_SIDE_VALUE_NAME),
                BaseValue::number(
                    DEFAULT_WINDOW_WIDTH as f64,
                    MIN_WINDOW_WIDTH as f64,
                    MAX_WINDOW_WIDTH as f64,
                    WINDOW_WIDTH_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_WINDOW_HEIGHT as f64,
                    MIN_WINDOW_HEIGHT as f64,
                    MAX_WINDOW_HEIGHT as f64,
                    WINDOW_HEIGHT_VALUE_NAME,
                ),
                BaseValue::number(286.0, 180.0, 520.0, RAIL_WIDTH_VALUE_NAME),
                BaseValue::number(
                    DEFAULT_THUMBNAIL_WIDTH as f64,
                    MIN_THUMBNAIL_WIDTH as f64,
                    420.0,
                    THUMBNAIL_WIDTH_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_THUMBNAIL_HEIGHT as f64,
                    MIN_THUMBNAIL_HEIGHT as f64,
                    260.0,
                    THUMBNAIL_HEIGHT_VALUE_NAME,
                ),
                BaseValue::number(
                    DEFAULT_MAX_THUMBNAILS as f64,
                    1.0,
                    MAX_THUMBNAILS as f64,
                    MAX_THUMBNAILS_VALUE_NAME,
                ),
            ],
            runtime: None,
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

impl Default for StageManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StageManager {
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

    fn main_value(&self) -> Option<&BaseValue> {
        self.value(RIGHT_SIDE_VALUE_NAME)
    }

    fn on_enable(&mut self) {
        if self.runtime.is_some() {
            return;
        }

        let Some(modules) = SHARED_MODULES.get().cloned() else {
            START_REQUESTED.store(true, Ordering::Release);
            eprintln!("StageManager cannot start before the module handler is shared.");
            self.mark_disabled_after_start_failure();
            return;
        };

        START_REQUESTED.store(true, Ordering::Release);
        match StageManagerRuntime::start(modules) {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                println!("StageManager enabled: foreground layout and thumbnail rail active.");
            }
            Err(error) => {
                START_REQUESTED.store(false, Ordering::Release);
                self.mark_disabled_after_start_failure();
                eprintln!("StageManager failed to start: {error}");
            }
        }
    }

    fn on_disable(&mut self) {
        START_REQUESTED.store(false, Ordering::Release);
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
            println!("StageManager disabled: thumbnail rail stopped.");
        }
    }
}

#[derive(Debug)]
enum StageHookError {
    AlreadyRunning,
    StartupFailed,
    ModuleHandle(windows::core::Error),
    Window(windows::core::Error),
    ObjectHook(windows::core::Error),
    SystemHook(windows::core::Error),
}

impl fmt::Display for StageHookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("stage manager is already running"),
            Self::StartupFailed => formatter.write_str("stage manager thread startup failed"),
            Self::ModuleHandle(error) => write!(formatter, "GetModuleHandleW failed: {error}"),
            Self::Window(error) => write!(formatter, "overlay window creation failed: {error}"),
            Self::ObjectHook(error) => write!(formatter, "object WinEvent hook failed: {error}"),
            Self::SystemHook(error) => write!(formatter, "system WinEvent hook failed: {error}"),
        }
    }
}

struct StageManagerRuntime {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl StageManagerRuntime {
    fn start(modules: SharedModuleHandler) -> Result<Self, StageHookError> {
        if OPEN_HWND
            .compare_exchange(0, STARTING_HWND, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(StageHookError::AlreadyRunning);
        }

        {
            let context = STAGE_CONTEXT.get_or_init(|| Mutex::new(None));
            let mut context = context.lock().map_err(|_| StageHookError::StartupFailed)?;
            if context.is_some() {
                OPEN_HWND.store(0, Ordering::Release);
                return Err(StageHookError::AlreadyRunning);
            }
            *context = Some(StageHookContext::default());
        }

        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = match thread::Builder::new()
            .name("nyx-stage-manager".to_owned())
            .spawn(move || run_stage_thread(modules, startup_tx))
        {
            Ok(worker) => worker,
            Err(_) => {
                clear_stage_context();
                OPEN_HWND.store(0, Ordering::Release);
                return Err(StageHookError::StartupFailed);
            }
        };

        match startup_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                clear_stage_context();
                OPEN_HWND.store(0, Ordering::Release);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                clear_stage_context();
                OPEN_HWND.store(0, Ordering::Release);
                Err(StageHookError::StartupFailed)
            }
        }
    }

    fn stop(&mut self) {
        if self.worker.take().is_some() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, STOP_MESSAGE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

impl Drop for StageManagerRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct StageHookContext {
    thread_id: u32,
    pending_event_count: u32,
}

struct InstalledStageHooks {
    object: HWINEVENTHOOK,
    system: HWINEVENTHOOK,
}

impl InstalledStageHooks {
    fn install() -> Result<Self, StageHookError> {
        let object = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_CREATE,
                EVENT_OBJECT_UNCLOAKED,
                None,
                Some(stage_win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if object.is_invalid() {
            return Err(StageHookError::ObjectHook(
                windows::core::Error::from_win32(),
            ));
        }

        let system = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_DESKTOPSWITCH,
                None,
                Some(stage_win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if system.is_invalid() {
            unsafe {
                let _ = UnhookWinEvent(object);
            }
            return Err(StageHookError::SystemHook(
                windows::core::Error::from_win32(),
            ));
        }

        Ok(Self { object, system })
    }
}

impl Drop for InstalledStageHooks {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.object);
            let _ = UnhookWinEvent(self.system);
        }
    }
}

fn run_stage_thread(
    modules: SharedModuleHandler,
    startup_tx: mpsc::Sender<Result<u32, StageHookError>>,
) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut message = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut message, None, WM_NULL, WM_NULL, PM_NOREMOVE);
    }

    if !set_stage_context_thread_id(thread_id) {
        let _ = startup_tx.send(Err(StageHookError::StartupFailed));
        clear_stage_context();
        OPEN_HWND.store(0, Ordering::Release);
        return;
    }

    let hwnd = match create_stage_window(modules) {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let _ = startup_tx.send(Err(error));
            clear_stage_context();
            OPEN_HWND.store(0, Ordering::Release);
            return;
        }
    };

    let hooks = match InstalledStageHooks::install() {
        Ok(hooks) => hooks,
        Err(error) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            let _ = startup_tx.send(Err(error));
            clear_stage_context();
            OPEN_HWND.store(0, Ordering::Release);
            return;
        }
    };

    let _ = startup_tx.send(Ok(thread_id));
    unsafe {
        let _ = PostThreadMessageW(thread_id, WAKE_MESSAGE, WPARAM(0), LPARAM(0));
    }
    stage_message_loop(hwnd);
    drop(hooks);
    clear_stage_context();
    OPEN_HWND.store(0, Ordering::Release);
}

fn create_stage_window(modules: SharedModuleHandler) -> Result<HWND, StageHookError> {
    let mut app = Box::new(StageApp::new(modules));
    let app_ptr = app.as_mut() as *mut StageApp;
    let screen = app.screen;

    let hmodule =
        unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(StageHookError::ModuleHandle)?;
    let hinstance = HINSTANCE(hmodule.0);
    let class_name = w!("NyxClientStageManagerOverlay");

    let window_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(stage_window_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: class_name,
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&window_class);
    }
    let _ = stage_hook_message();

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class_name,
            w!("NyxClient StageManager"),
            WS_POPUP,
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            None,
            None,
            Some(hinstance),
            Some(app_ptr.cast::<c_void>()),
        )
    }
    .map_err(StageHookError::Window)?;

    let _leaked_to_window = Box::into_raw(app);

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetTimer(Some(hwnd), LAYOUT_TIMER_ID, LAYOUT_POLL_MS, None);
    }

    Ok(hwnd)
}

fn stage_message_loop(hwnd: HWND) {
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 || message.message == WM_QUIT {
            break;
        }

        if message.message == STOP_MESSAGE {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            continue;
        }

        if message.message == WAKE_MESSAGE {
            if let Some(app) = unsafe { stage_app_from_hwnd(hwnd) } {
                app.tick();
            }
            continue;
        }

        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn stage_win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    object_id: i32,
    child_id: i32,
    _event_thread_id: u32,
    _event_time: u32,
) {
    if child_id != CHILDID_SELF as i32
        || (object_id != OBJID_WINDOW.0 && object_id != OBJID_CLIENT.0)
    {
        return;
    }

    record_stage_event(hwnd);
}

fn record_stage_event(hwnd: HWND) {
    let thread_id = {
        let Some(context) = STAGE_CONTEXT.get() else {
            return;
        };
        let Ok(mut context) = context.lock() else {
            return;
        };
        let Some(context) = context.as_mut() else {
            return;
        };
        context.pending_event_count = context.pending_event_count.saturating_add(1);
        context.thread_id
    };

    let current_thread_id = unsafe { GetCurrentThreadId() };
    if thread_id != 0 && thread_id != current_thread_id {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WAKE_MESSAGE, WPARAM(0), LPARAM(hwnd.0 as isize));
        }
    }
}

fn set_stage_context_thread_id(thread_id: u32) -> bool {
    let Some(context) = STAGE_CONTEXT.get() else {
        return false;
    };
    let Ok(mut context) = context.lock() else {
        return false;
    };
    let Some(context) = context.as_mut() else {
        return false;
    };
    context.thread_id = thread_id;
    true
}

fn clear_stage_context() {
    if let Some(context) = STAGE_CONTEXT.get()
        && let Ok(mut context) = context.lock()
    {
        *context = None;
    }
}

fn stage_hook_message() -> u32 {
    *STAGE_HOOK_MESSAGE
        .get_or_init(|| unsafe { RegisterWindowMessageW(w!("NyxClient.StageManager.WinEvent")) })
}

struct StageApp {
    hwnd: HWND,
    modules: SharedModuleHandler,
    screen: ScreenRect,
    recent_hwnds: Vec<isize>,
    thumbnails: HashMap<isize, CachedThumbnail>,
    layouts: Vec<ThumbnailLayout>,
    uploader: LayeredFrameUploader,
    typeface: Option<Typeface>,
    last_snapshot_at: Instant,
    last_active: isize,
}

impl StageApp {
    fn new(modules: SharedModuleHandler) -> Self {
        Self {
            hwnd: HWND(null_mut()),
            modules,
            screen: virtual_screen_rect(),
            recent_hwnds: Vec::new(),
            thumbnails: HashMap::new(),
            layouts: Vec::new(),
            uploader: LayeredFrameUploader::default(),
            typeface: match_typeface(),
            last_snapshot_at: Instant::now() - SNAPSHOT_INTERVAL,
            last_active: 0,
        }
    }

    fn tick(&mut self) {
        if self.should_close() {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return;
        }

        let settings = self.settings();
        self.ensure_screen_rect();

        if foreground_is_fullscreen(self.hwnd) {
            self.layouts.clear();
            self.render();
            return;
        }

        let candidates = visible_stage_windows(self.hwnd);
        let candidate_ids = candidates
            .iter()
            .map(|candidate| candidate.raw)
            .collect::<HashSet<_>>();
        let active = foreground_stage_window(self.hwnd);
        let active_raw = active.as_ref().map(|window| window.raw);

        if let Some(active) = active.as_ref() {
            self.remember_window(active.raw);
            if self.last_active != active.raw {
                self.last_active = active.raw;
                self.last_snapshot_at = Instant::now() - SNAPSHOT_INTERVAL;
            }
            arrange_foreground_window(active.hwnd, &settings);
        }

        self.recent_hwnds
            .retain(|hwnd| candidate_ids.contains(hwnd));
        for candidate in &candidates {
            if active_raw != Some(candidate.raw) && !self.recent_hwnds.contains(&candidate.raw) {
                self.recent_hwnds.push(candidate.raw);
            }
        }

        let thumbnail_candidates = self.thumbnail_candidates(&candidates, active_raw, &settings);
        self.refresh_thumbnail_cache(&thumbnail_candidates, &candidate_ids, &settings);
        self.layouts = build_thumbnail_layout(
            active
                .as_ref()
                .and_then(|window| monitor_rects_for_window(window.hwnd))
                .map(|monitor| monitor.work)
                .unwrap_or_else(|| self.screen.as_rect_i()),
            &thumbnail_candidates,
            &self.thumbnails,
            &settings,
        );
        self.render();
    }

    fn should_close(&self) -> bool {
        if !START_REQUESTED.load(Ordering::Acquire) {
            return true;
        }
        let Ok(modules) = self.modules.lock() else {
            return false;
        };
        let Some(module) = modules.get(MODULE_NAME) else {
            return true;
        };
        !module.is_enabled()
    }

    fn settings(&self) -> StageSettings {
        let Ok(modules) = self.modules.lock() else {
            return StageSettings::default();
        };
        let Some(module) = modules.get(MODULE_NAME) else {
            return StageSettings::default();
        };
        StageSettings::from_module(module)
    }

    fn ensure_screen_rect(&mut self) {
        let next = virtual_screen_rect();
        if next == self.screen {
            return;
        }

        self.screen = next;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                self.screen.x,
                self.screen.y,
                self.screen.width,
                self.screen.height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    fn remember_window(&mut self, raw: isize) {
        self.recent_hwnds.retain(|hwnd| *hwnd != raw);
        self.recent_hwnds.insert(0, raw);
        if self.recent_hwnds.len() > MAX_THUMBNAILS * 2 {
            self.recent_hwnds.truncate(MAX_THUMBNAILS * 2);
        }
    }

    fn thumbnail_candidates(
        &self,
        candidates: &[WindowCandidate],
        active: Option<isize>,
        settings: &StageSettings,
    ) -> Vec<WindowCandidate> {
        let by_id = candidates
            .iter()
            .map(|candidate| (candidate.raw, candidate.clone()))
            .collect::<HashMap<_, _>>();
        let mut selected = Vec::new();
        let mut seen = HashSet::new();

        for raw in &self.recent_hwnds {
            if active == Some(*raw) || !seen.insert(*raw) {
                continue;
            }
            if let Some(candidate) = by_id.get(raw) {
                selected.push(candidate.clone());
            }
            if selected.len() >= settings.max_thumbnails {
                return selected;
            }
        }

        for candidate in candidates {
            if active == Some(candidate.raw) || !seen.insert(candidate.raw) {
                continue;
            }
            selected.push(candidate.clone());
            if selected.len() >= settings.max_thumbnails {
                break;
            }
        }

        selected
    }

    fn refresh_thumbnail_cache(
        &mut self,
        candidates: &[WindowCandidate],
        valid_ids: &HashSet<isize>,
        settings: &StageSettings,
    ) {
        self.thumbnails.retain(|raw, _| valid_ids.contains(raw));

        let now = Instant::now();
        let should_capture = now.duration_since(self.last_snapshot_at) >= SNAPSHOT_INTERVAL;
        for candidate in candidates {
            let entry = self
                .thumbnails
                .entry(candidate.raw)
                .or_insert_with(|| CachedThumbnail::placeholder(candidate));
            entry.title = candidate.title.clone();

            if should_capture || entry.image.is_none() {
                if let Some(capture) = capture_window(candidate.hwnd, settings) {
                    entry.image = Some(capture.image);
                    entry.source_width = capture.width;
                    entry.source_height = capture.height;
                }
                entry.captured_at = now;
            }
        }

        if should_capture {
            self.last_snapshot_at = now;
        }
    }

    fn render(&mut self) {
        let mut frame = blank_frame(self.screen);
        let layouts = self.layouts.clone();
        let thumbnails = &self.thumbnails;
        let typeface = self.typeface.clone();

        if frame
            .with_canvas(|canvas| {
                canvas.clear(SkColor::TRANSPARENT);
                draw_thumbnail_rail(canvas, self.screen, &layouts, thumbnails, typeface);
            })
            .is_none()
        {
            return;
        }

        if let Err(error) = self.uploader.update(self.hwnd, self.screen, &frame.pixels) {
            eprintln!("StageManager overlay update failed: {error:?}");
        }
    }

    fn hit_test_screen_point(&self, x: i32, y: i32) -> Option<HWND> {
        for layout in &self.layouts {
            if layout.screen_rect.contains_point(x, y) {
                return Some(HWND(layout.raw as *mut c_void));
            }
        }

        None
    }

    fn handle_left_click(&mut self, client_x: i32, client_y: i32) {
        let screen_x = self.screen.x + client_x;
        let screen_y = self.screen.y + client_y;
        let Some(target) = self.hit_test_screen_point(screen_x, screen_y) else {
            return;
        };

        activate_window(target);
        self.remember_window(target.0 as isize);
        self.tick();
    }
}

#[derive(Debug, Clone, Copy)]
struct StageSettings {
    right_side: bool,
    window_width: i32,
    window_height: i32,
    rail_width: i32,
    thumbnail_width: i32,
    thumbnail_height: i32,
    max_thumbnails: usize,
}

impl StageSettings {
    fn from_module(module: &dyn Module) -> Self {
        let mut settings = Self {
            right_side: boolean_value(module, RIGHT_SIDE_VALUE_NAME, false),
            window_width: number_value(module, WINDOW_WIDTH_VALUE_NAME, DEFAULT_WINDOW_WIDTH as f64)
                .round() as i32,
            window_height: number_value(
                module,
                WINDOW_HEIGHT_VALUE_NAME,
                DEFAULT_WINDOW_HEIGHT as f64,
            )
            .round() as i32,
            rail_width: number_value(module, RAIL_WIDTH_VALUE_NAME, DEFAULT_RAIL_WIDTH as f64)
                .round() as i32,
            thumbnail_width: number_value(
                module,
                THUMBNAIL_WIDTH_VALUE_NAME,
                DEFAULT_THUMBNAIL_WIDTH as f64,
            )
            .round() as i32,
            thumbnail_height: number_value(
                module,
                THUMBNAIL_HEIGHT_VALUE_NAME,
                DEFAULT_THUMBNAIL_HEIGHT as f64,
            )
            .round() as i32,
            max_thumbnails: number_value(
                module,
                MAX_THUMBNAILS_VALUE_NAME,
                DEFAULT_MAX_THUMBNAILS as f64,
            )
            .round() as usize,
        };
        settings.sanitize();
        settings
    }

    fn sanitize(&mut self) {
        self.window_width = self.window_width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH);
        self.window_height = self
            .window_height
            .clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_HEIGHT);
        self.rail_width = self.rail_width.clamp(160, 560);
        self.thumbnail_width = self.thumbnail_width.clamp(MIN_THUMBNAIL_WIDTH, 460);
        self.thumbnail_height = self.thumbnail_height.clamp(MIN_THUMBNAIL_HEIGHT, 300);
        self.max_thumbnails = self.max_thumbnails.clamp(1, MAX_THUMBNAILS);
    }
}

impl Default for StageSettings {
    fn default() -> Self {
        Self {
            right_side: false,
            window_width: DEFAULT_WINDOW_WIDTH,
            window_height: DEFAULT_WINDOW_HEIGHT,
            rail_width: DEFAULT_RAIL_WIDTH,
            thumbnail_width: DEFAULT_THUMBNAIL_WIDTH,
            thumbnail_height: DEFAULT_THUMBNAIL_HEIGHT,
            max_thumbnails: DEFAULT_MAX_THUMBNAILS,
        }
    }
}

#[derive(Clone)]
struct WindowCandidate {
    hwnd: HWND,
    raw: isize,
    rect: RECT,
    title: String,
}

struct CachedThumbnail {
    title: String,
    image: Option<Image>,
    source_width: i32,
    source_height: i32,
    captured_at: Instant,
}

impl CachedThumbnail {
    fn placeholder(candidate: &WindowCandidate) -> Self {
        Self {
            title: candidate.title.clone(),
            image: None,
            source_width: rect_width(candidate.rect).max(1),
            source_height: rect_height(candidate.rect).max(1),
            captured_at: Instant::now(),
        }
    }
}

struct CapturedWindow {
    image: Image,
    width: i32,
    height: i32,
}

#[derive(Clone)]
struct ThumbnailLayout {
    raw: isize,
    screen_rect: RectI,
    local_rect: FloatRect,
    depth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScreenRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl ScreenRect {
    fn as_rect_i(self) -> RectI {
        RectI {
            left: self.x,
            top: self.y,
            right: self.x + self.width,
            bottom: self.y + self.height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RectI {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl RectI {
    fn width(self) -> i32 {
        self.right - self.left
    }

    fn height(self) -> i32 {
        self.bottom - self.top
    }

    fn center_y(self) -> i32 {
        self.top + self.height() / 2
    }

    fn contains_point(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

impl From<RECT> for RectI {
    fn from(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FloatRect {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl FloatRect {
    fn width(self) -> f32 {
        self.right - self.left
    }

    fn height(self) -> f32 {
        self.bottom - self.top
    }

    fn to_sk_rect(self) -> SkRect {
        SkRect::new(self.left, self.top, self.right, self.bottom)
    }
}

#[derive(Debug, Clone, Copy)]
struct MonitorRects {
    monitor: RectI,
    work: RectI,
}

fn visible_stage_windows(overlay_hwnd: HWND) -> Vec<WindowCandidate> {
    let mut windows: Vec<WindowCandidate> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_stage_window_proc),
            LPARAM((&mut windows as *mut Vec<WindowCandidate>) as isize),
        );
    }

    windows
        .into_iter()
        .filter(|candidate| candidate.hwnd != overlay_hwnd && !is_fullscreen_candidate(candidate))
        .collect()
}

unsafe extern "system" fn enum_stage_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = unsafe { &mut *(lparam.0 as *mut Vec<WindowCandidate>) };
    if let Some(candidate) = window_candidate(hwnd) {
        windows.push(candidate);
    }

    true.into()
}

fn foreground_stage_window(overlay_hwnd: HWND) -> Option<WindowCandidate> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() || hwnd == overlay_hwnd {
        return None;
    }
    let candidate = window_candidate(hwnd)?;
    if is_fullscreen_candidate(&candidate) {
        return None;
    }
    Some(candidate)
}

fn foreground_is_fullscreen(overlay_hwnd: HWND) -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() || hwnd == overlay_hwnd {
        return false;
    }

    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return false;
    }

    monitor_rects_for_window(hwnd)
        .is_some_and(|monitor| rect_covers(rect.into(), monitor.monitor, FULLSCREEN_TOLERANCE))
}

fn window_candidate(hwnd: HWND) -> Option<WindowCandidate> {
    if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)).as_bool() } {
        return None;
    }
    if !unsafe { IsWindowVisible(hwnd).as_bool() } || unsafe { IsIconic(hwnd).as_bool() } {
        return None;
    }

    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if !root.0.is_null() && root != hwnd {
        return None;
    }

    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if style & (WS_CHILD.0 | WS_DISABLED.0) != 0 {
        return None;
    }
    if ex_style & (WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0) != 0 {
        return None;
    }

    if let Ok(owner) = unsafe { GetWindow(hwnd, GW_OWNER) }
        && !owner.0.is_null()
        && ex_style & WS_EX_APPWINDOW.0 == 0
    {
        return None;
    }

    let pid = window_process_id(hwnd);
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return None;
    }

    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    if rect_width(rect) < MIN_CANDIDATE_WIDTH || rect_height(rect) < MIN_CANDIDATE_HEIGHT {
        return None;
    }

    let class_name = class_name(hwnd);
    if is_ignored_window_class(&class_name) {
        return None;
    }

    let title = window_text(hwnd);
    if title.trim().is_empty() && ex_style & WS_EX_APPWINDOW.0 == 0 {
        return None;
    }

    Some(WindowCandidate {
        hwnd,
        raw: hwnd.0 as isize,
        rect,
        title: if title.trim().is_empty() {
            class_name.clone()
        } else {
            title
        },
    })
}

fn is_ignored_window_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "Button"
            | "DV2ControlHost"
            | "MsgrIMEWindowClass"
            | "IME"
            | "SysShadow"
            | "TaskListThumbnailWnd"
            | "Windows.UI.Core.CoreWindow"
            | "NyxClientStageManagerOverlay"
            | "NyxClientClickGuiWindow"
            | "NyxClientLive2DOverlay"
    )
}

fn is_fullscreen_candidate(candidate: &WindowCandidate) -> bool {
    monitor_rects_for_window(candidate.hwnd).is_some_and(|monitor| {
        rect_covers(candidate.rect.into(), monitor.monitor, FULLSCREEN_TOLERANCE)
    })
}

fn arrange_foreground_window(hwnd: HWND, settings: &StageSettings) {
    let Some(monitor) = monitor_rects_for_window(hwnd) else {
        return;
    };
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return;
    }
    if rect_covers(rect.into(), monitor.monitor, FULLSCREEN_TOLERANCE) {
        return;
    }

    let work = monitor.work;
    let content_left = if settings.right_side {
        work.left + LAYOUT_MARGIN
    } else {
        work.left + settings.rail_width + LAYOUT_MARGIN
    };
    let content_right = if settings.right_side {
        work.right - settings.rail_width - LAYOUT_MARGIN
    } else {
        work.right - LAYOUT_MARGIN
    };
    let content_width = (content_right - content_left).max(MIN_WINDOW_WIDTH);
    let content_height = (work.height() - LAYOUT_MARGIN * 2).max(MIN_WINDOW_HEIGHT);
    let width = settings
        .window_width
        .min(content_width)
        .max(MIN_WINDOW_WIDTH);
    let height = settings
        .window_height
        .min(content_height)
        .max(MIN_WINDOW_HEIGHT);
    let x = content_left + ((content_width - width) / 2).max(0);
    let y = work.top + ((work.height() - height) / 2).max(LAYOUT_MARGIN);

    let current = RectI::from(rect);
    if (current.left - x).abs() <= 2
        && (current.top - y).abs() <= 2
        && (current.width() - width).abs() <= 2
        && (current.height() - height).abs() <= 2
    {
        return;
    }

    if unsafe { IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() } {
        unsafe {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }

    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
        );
    }
}

fn build_thumbnail_layout(
    work: RectI,
    candidates: &[WindowCandidate],
    thumbnails: &HashMap<isize, CachedThumbnail>,
    settings: &StageSettings,
) -> Vec<ThumbnailLayout> {
    let rail_left = if settings.right_side {
        work.right - settings.rail_width
    } else {
        work.left
    };
    let rail_right = if settings.right_side {
        work.right
    } else {
        work.left + settings.rail_width
    };

    let base_width = settings.thumbnail_width as f32;
    let base_height = settings.thumbnail_height as f32;
    let vertical_step = (base_height * 0.68).max(76.0);
    let center_y = work.center_y() as f32;
    let max_depth = ((candidates.len().saturating_sub(1) + 1) / 2).max(1);
    let mut layouts = Vec::new();

    for (index, candidate) in candidates.iter().enumerate() {
        let depth = if index == 0 { 0 } else { index.div_ceil(2) };
        let direction = if index == 0 {
            0.0
        } else if index % 2 == 1 {
            -1.0
        } else {
            1.0
        };
        let scale = (1.0 - depth as f32 * 0.075).clamp(0.62, 1.0);
        let width = base_width * scale;
        let height = base_height * scale;
        let cy = center_y + direction * depth as f32 * vertical_step;
        let top = (cy - height / 2.0)
            .max(work.top as f32 + 14.0)
            .min(work.bottom as f32 - height - 14.0);
        let inward = (max_depth.saturating_sub(depth) as f32 * 13.0).max(0.0);
        let depth_offset = depth as f32 * 19.0;
        let left = if settings.right_side {
            rail_left as f32 + 18.0 + depth_offset - inward * 0.25
        } else {
            rail_right as f32 - width - 18.0 - depth_offset + inward * 0.25
        }
        .max(rail_left as f32 + 8.0)
        .min(rail_right as f32 - width - 8.0);

        let screen_rect = RectI {
            left: left.round() as i32,
            top: top.round() as i32,
            right: (left + width).round() as i32,
            bottom: (top + height).round() as i32,
        };

        if !thumbnails.contains_key(&candidate.raw) {
            continue;
        }

        layouts.push(ThumbnailLayout {
            raw: candidate.raw,
            screen_rect,
            local_rect: FloatRect {
                left,
                top,
                right: left + width,
                bottom: top + height,
            },
            depth,
        });
    }

    layouts
}

fn draw_thumbnail_rail(
    canvas: &Canvas,
    screen: ScreenRect,
    layouts: &[ThumbnailLayout],
    thumbnails: &HashMap<isize, CachedThumbnail>,
    typeface: Option<Typeface>,
) {
    let mut draw_order = layouts.to_vec();
    draw_order.sort_by(|left, right| {
        right
            .depth
            .cmp(&left.depth)
            .then_with(|| right.raw.cmp(&left.raw))
    });

    for layout in &draw_order {
        let Some(thumbnail) = thumbnails.get(&layout.raw) else {
            continue;
        };
        draw_thumbnail(canvas, screen, layout, thumbnail, typeface.clone());
    }
}

fn draw_thumbnail(
    canvas: &Canvas,
    screen: ScreenRect,
    layout: &ThumbnailLayout,
    thumbnail: &CachedThumbnail,
    typeface: Option<Typeface>,
) {
    let rect = FloatRect {
        left: layout.local_rect.left - screen.x as f32,
        top: layout.local_rect.top - screen.y as f32,
        right: layout.local_rect.right - screen.x as f32,
        bottom: layout.local_rect.bottom - screen.y as f32,
    };
    let sk_rect = rect.to_sk_rect();
    let alpha = (236.0 - layout.depth as f32 * 22.0).clamp(150.0, 236.0) as u8;

    let mut shadow_paint = Paint::default();
    shadow_paint.set_anti_alias(true);
    shadow_paint.set_style(PaintStyle::Fill);
    shadow_paint.set_color(rgba(0, 0, 0, 58));
    canvas.draw_round_rect(
        SkRect::new(
            rect.left + 3.0,
            rect.top + 7.0,
            rect.right + 3.0,
            rect.bottom + 7.0,
        ),
        THUMBNAIL_RADIUS + 2.0,
        THUMBNAIL_RADIUS + 2.0,
        &shadow_paint,
    );

    let mut card_paint = Paint::default();
    card_paint.set_anti_alias(true);
    card_paint.set_style(PaintStyle::Fill);
    card_paint.set_color(rgba(18, 21, 25, alpha));
    canvas.draw_round_rect(sk_rect, THUMBNAIL_RADIUS, THUMBNAIL_RADIUS, &card_paint);

    let preview = FloatRect {
        left: rect.left + THUMBNAIL_PADDING,
        top: rect.top + THUMBNAIL_PADDING,
        right: rect.right - THUMBNAIL_PADDING,
        bottom: (rect.bottom - THUMBNAIL_TITLE_HEIGHT).max(rect.top + THUMBNAIL_PADDING + 10.0),
    };

    let mut preview_paint = Paint::default();
    preview_paint.set_anti_alias(true);
    preview_paint.set_color(rgba(255, 255, 255, 255));

    if let Some(image) = &thumbnail.image {
        let (src, dst) = cover_rect(
            thumbnail.source_width as f32,
            thumbnail.source_height as f32,
            preview,
        );
        canvas.save();
        canvas.clip_rrect(
            &RRect::new_rect_xy(preview.to_sk_rect(), 5.0, 5.0),
            None,
            true,
        );
        canvas.draw_image_rect_with_sampling_options(
            image,
            Some((&src, SrcRectConstraint::Fast)),
            dst.to_sk_rect(),
            SamplingOptions::from(FilterMode::Linear),
            &preview_paint,
        );
        canvas.restore();
    } else {
        let mut placeholder_paint = Paint::default();
        placeholder_paint.set_anti_alias(true);
        placeholder_paint.set_color(rgba(46, 51, 58, alpha.saturating_sub(20)));
        canvas.draw_round_rect(preview.to_sk_rect(), 5.0, 5.0, &placeholder_paint);
    }

    let mut stroke = Paint::default();
    stroke.set_anti_alias(true);
    stroke.set_style(PaintStyle::Stroke);
    stroke.set_stroke_width(1.0);
    stroke.set_color(rgba(255, 255, 255, 34));
    canvas.draw_round_rect(sk_rect, THUMBNAIL_RADIUS, THUMBNAIL_RADIUS, &stroke);

    let mut text_paint = Paint::default();
    text_paint.set_anti_alias(true);
    text_paint.set_color(rgba(238, 241, 245, alpha));
    let font = stage_font(typeface, (12.0 - layout.depth as f32 * 0.4).max(10.5));
    let title = ellipsize(
        &thumbnail.title,
        &font,
        &text_paint,
        (rect.width() - 20.0).max(32.0),
    );
    canvas.draw_str(
        title,
        (rect.left + 10.0, rect.bottom - 10.0),
        &font,
        &text_paint,
    );
}

fn cover_rect(source_width: f32, source_height: f32, dst: FloatRect) -> (SkRect, FloatRect) {
    let source_width = source_width.max(1.0);
    let source_height = source_height.max(1.0);
    let source_aspect = source_width / source_height;
    let dst_aspect = dst.width().max(1.0) / dst.height().max(1.0);

    if source_aspect > dst_aspect {
        let crop_width = source_height * dst_aspect;
        let left = (source_width - crop_width) / 2.0;
        (
            SkRect::new(left, 0.0, left + crop_width, source_height),
            dst,
        )
    } else {
        let crop_height = source_width / dst_aspect;
        let top = (source_height - crop_height) / 2.0;
        (SkRect::new(0.0, top, source_width, top + crop_height), dst)
    }
}

fn capture_window(hwnd: HWND, _settings: &StageSettings) -> Option<CapturedWindow> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    let width = rect_width(rect).clamp(1, 4096);
    let height = rect_height(rect).clamp(1, 4096);
    let row_bytes = width as usize * 4;
    let pixel_len = row_bytes.checked_mul(height as usize)?;

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.0.is_null() {
        return None;
    }

    let memory_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    if memory_dc.0.is_null() {
        unsafe {
            let _ = ReleaseDC(None, screen_dc);
        }
        return None;
    }

    let mut bitmap_info = BITMAPINFO::default();
    bitmap_info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: pixel_len as u32,
        ..Default::default()
    };

    let mut bits = null_mut::<c_void>();
    let bitmap = unsafe {
        CreateDIBSection(
            Some(screen_dc),
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    };
    unsafe {
        let _ = ReleaseDC(None, screen_dc);
    }

    let bitmap = match bitmap {
        Ok(bitmap) => bitmap,
        Err(_) => {
            unsafe {
                let _ = DeleteDC(memory_dc);
            }
            return None;
        }
    };

    let previous = unsafe { SelectObject(memory_dc, HGDIOBJ(bitmap.0)) };
    let captured = bit_blt_window(hwnd, memory_dc, width, height);

    let mut pixels = vec![0_u8; pixel_len];
    if captured && !bits.is_null() {
        unsafe {
            ptr::copy_nonoverlapping(bits.cast::<u8>(), pixels.as_mut_ptr(), pixels.len());
        }
        ensure_opaque_alpha(&mut pixels);
    }

    unsafe {
        let _ = SelectObject(memory_dc, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);
    }

    if !captured {
        return None;
    }

    let image = raster_image_from_pixels(width, height, row_bytes, pixels)?;
    Some(CapturedWindow {
        image,
        width,
        height,
    })
}

fn bit_blt_window(hwnd: HWND, memory_dc: HDC, width: i32, height: i32) -> bool {
    let window_dc = unsafe { GetWindowDC(Some(hwnd)) };
    if window_dc.0.is_null() {
        return false;
    }
    let result = unsafe {
        BitBlt(
            memory_dc,
            0,
            0,
            width,
            height,
            Some(window_dc),
            0,
            0,
            SRCCOPY | CAPTUREBLT,
        )
        .is_ok()
    };
    unsafe {
        let _ = ReleaseDC(Some(hwnd), window_dc);
    }
    result
}

fn ensure_opaque_alpha(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            pixel[3] = 255;
        }
    }
}

fn raster_image_from_pixels(
    width: i32,
    height: i32,
    row_bytes: usize,
    pixels: Vec<u8>,
) -> Option<Image> {
    let image_info = ImageInfo::new_n32((width, height), AlphaType::Premul, None);
    #[allow(deprecated)]
    Image::from_raster_data(&image_info, Data::new_copy(&pixels), row_bytes)
}

struct RenderedFrame {
    rect: ScreenRect,
    pixels: Vec<u8>,
    row_bytes: usize,
}

impl RenderedFrame {
    fn with_canvas<R>(&mut self, draw: impl FnOnce(&Canvas) -> R) -> Option<R> {
        let image_info = ImageInfo::new_n32(
            (self.rect.width.max(1), self.rect.height.max(1)),
            AlphaType::Premul,
            None,
        );
        let mut surface =
            surfaces::wrap_pixels(&image_info, &mut self.pixels, Some(self.row_bytes), None)?;
        Some(draw(surface.canvas()))
    }
}

fn blank_frame(rect: ScreenRect) -> RenderedFrame {
    let width = rect.width.max(1);
    let height = rect.height.max(1);
    let row_bytes = width as usize * 4;
    RenderedFrame {
        rect,
        pixels: vec![0; row_bytes * height as usize],
        row_bytes,
    }
}

#[derive(Default)]
struct LayeredFrameUploader {
    memory_dc: Option<HDC>,
    bitmap: Option<HBITMAP>,
    previous_object: Option<HGDIOBJ>,
    bits: *mut c_void,
    width: i32,
    height: i32,
}

impl LayeredFrameUploader {
    fn update(
        &mut self,
        hwnd: HWND,
        screen: ScreenRect,
        pixels: &[u8],
    ) -> windows::core::Result<()> {
        let width = screen.width.max(1);
        let height = screen.height.max(1);
        self.ensure_bitmap(width, height, pixels.len())?;

        if !self.bits.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(pixels.as_ptr(), self.bits.cast::<u8>(), pixels.len());
            }
        }

        let Some(memory_dc) = self.memory_dc else {
            return Ok(());
        };
        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.0.is_null() {
            return Ok(());
        }

        let destination = POINT {
            x: screen.x,
            y: screen.y,
        };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let result = unsafe {
            UpdateLayeredWindow(
                hwnd,
                Some(screen_dc),
                Some(&destination),
                Some(&size),
                Some(memory_dc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };

        unsafe {
            let _ = ReleaseDC(None, screen_dc);
        }
        result
    }

    fn ensure_bitmap(
        &mut self,
        width: i32,
        height: i32,
        pixel_len: usize,
    ) -> windows::core::Result<()> {
        if self.memory_dc.is_some()
            && self.bitmap.is_some()
            && !self.bits.is_null()
            && self.width == width
            && self.height == height
        {
            return Ok(());
        }

        self.release_bitmap();

        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.0.is_null() {
            return Ok(());
        }

        if self.memory_dc.is_none() {
            let memory_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
            if memory_dc.0.is_null() {
                unsafe {
                    let _ = ReleaseDC(None, screen_dc);
                }
                return Ok(());
            }
            self.memory_dc = Some(memory_dc);
        }

        let mut bitmap_info = BITMAPINFO::default();
        bitmap_info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: pixel_len as u32,
            ..Default::default()
        };

        let mut bits = null_mut::<c_void>();
        let bitmap = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bitmap_info,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
        };
        unsafe {
            let _ = ReleaseDC(None, screen_dc);
        }

        let bitmap = bitmap?;
        let Some(memory_dc) = self.memory_dc else {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            return Ok(());
        };

        let previous = unsafe { SelectObject(memory_dc, HGDIOBJ(bitmap.0)) };
        self.bitmap = Some(bitmap);
        self.previous_object = Some(previous);
        self.bits = bits;
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn release_bitmap(&mut self) {
        if let Some(memory_dc) = self.memory_dc {
            if let Some(previous) = self.previous_object.take() {
                unsafe {
                    let _ = SelectObject(memory_dc, previous);
                }
            }
        }
        if let Some(bitmap) = self.bitmap.take() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
        }
        self.bits = null_mut();
        self.width = 0;
        self.height = 0;
    }
}

impl Drop for LayeredFrameUploader {
    fn drop(&mut self) {
        self.release_bitmap();
        if let Some(memory_dc) = self.memory_dc.take() {
            unsafe {
                let _ = DeleteDC(memory_dc);
            }
        }
    }
}

unsafe extern "system" fn stage_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == stage_hook_message() {
        if let Some(app) = unsafe { stage_app_from_hwnd(hwnd) } {
            app.tick();
        }
        return LRESULT(0);
    }

    match message {
        WM_NCCREATE => {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let app_ptr = unsafe { (*create).lpCreateParams as *mut StageApp };
                if !app_ptr.is_null() {
                    unsafe {
                        (*app_ptr).hwnd = hwnd;
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);
                    }
                    OPEN_HWND.store(hwnd.0 as isize, Ordering::Release);
                    return LRESULT(1);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            let x = signed_loword(lparam.0);
            let y = signed_hiword(lparam.0);
            if let Some(app) = unsafe { stage_app_from_hwnd(hwnd) }
                && app.hit_test_screen_point(x, y).is_some()
            {
                return LRESULT(HTCLIENT as isize);
            }
            LRESULT(HTTRANSPARENT as isize)
        }
        WM_LBUTTONDOWN => {
            if let Some(app) = unsafe { stage_app_from_hwnd(hwnd) } {
                let x = signed_loword(lparam.0);
                let y = signed_hiword(lparam.0);
                app.handle_left_click(x, y);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == LAYOUT_TIMER_ID {
                if let Some(app) = unsafe { stage_app_from_hwnd(hwnd) } {
                    app.tick();
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), LAYOUT_TIMER_ID);
            }
            let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut StageApp };
            if !raw.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(raw));
                }
            }
            OPEN_HWND.store(0, Ordering::Release);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn stage_app_from_hwnd(hwnd: HWND) -> Option<&'static mut StageApp> {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut StageApp };
    if raw.is_null() {
        None
    } else {
        Some(unsafe { &mut *raw })
    }
}

fn activate_window(hwnd: HWND) {
    if hwnd.0.is_null() || !unsafe { IsWindow(Some(hwnd)).as_bool() } {
        return;
    }

    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindowAsync(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

fn monitor_rects_for_window(hwnd: HWND) -> Option<MonitorRects> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return None;
    }

    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        return None;
    }

    Some(MonitorRects {
        monitor: info.rcMonitor.into(),
        work: info.rcWork.into(),
    })
}

fn virtual_screen_rect() -> ScreenRect {
    let x = unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN) };
    let y = unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN) };
    let width =
        unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXVIRTUALSCREEN) }
            .max(1);
    let height =
        unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYVIRTUALSCREEN) }
            .max(1);

    if width > 0 && height > 0 {
        return ScreenRect {
            x,
            y,
            width,
            height,
        };
    }

    let monitor = unsafe { MonitorFromWindow(HWND(null_mut()), MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !monitor.0.is_null() && unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        let rect: RectI = info.rcMonitor.into();
        return ScreenRect {
            x: rect.left,
            y: rect.top,
            width: rect.width().max(1),
            height: rect.height().max(1),
        };
    }

    ScreenRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    }
}

fn rect_covers(rect: RectI, monitor: RectI, tolerance: i32) -> bool {
    rect.left <= monitor.left + tolerance
        && rect.top <= monitor.top + tolerance
        && rect.right >= monitor.right - tolerance
        && rect.bottom >= monitor.bottom - tolerance
}

fn rect_width(rect: RECT) -> i32 {
    rect.right - rect.left
}

fn rect_height(rect: RECT) -> i32 {
    rect.bottom - rect.top
}

fn window_process_id(hwnd: HWND) -> u32 {
    let mut process_id = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    process_id
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

fn number_value(module: &dyn Module, name: &str, default: f64) -> f64 {
    module
        .value(name)
        .and_then(BaseValue::as_number)
        .map(|value| value.value())
        .unwrap_or(default)
}

fn boolean_value(module: &dyn Module, name: &str, default: bool) -> bool {
    module
        .value(name)
        .and_then(BaseValue::as_boolean)
        .map(|value| value.value())
        .unwrap_or(default)
}

fn stage_font(typeface: Option<Typeface>, size: f32) -> Font {
    let mut font = if let Some(typeface) = typeface {
        Font::new(typeface, Some(size))
    } else {
        let mut font = Font::default();
        font.set_size(size);
        font
    };
    font.set_subpixel(true);
    font.set_linear_metrics(true);
    font
}

fn match_typeface() -> Option<Typeface> {
    let font_mgr = FontMgr::new();
    let style = FontStyle::new(
        font_style::Weight::MEDIUM,
        font_style::Width::NORMAL,
        font_style::Slant::Upright,
    );
    [
        "Microsoft YaHei UI",
        "Microsoft YaHei",
        "Noto Sans SC",
        "Noto Sans CJK SC",
        "Segoe UI",
        "Inter",
    ]
    .into_iter()
    .find_map(|family| font_mgr.match_family_style(family, style))
}

fn ellipsize(text: &str, font: &Font, paint: &Paint, max_width: f32) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "Window".to_owned();
    }

    let (width, _) = font.measure_str(text, Some(paint));
    if width <= max_width {
        return text.to_owned();
    }

    let ellipsis = "...";
    let mut output = String::new();
    for ch in text.chars() {
        let candidate = format!("{output}{ch}{ellipsis}");
        let (candidate_width, _) = font.measure_str(&candidate, Some(paint));
        if candidate_width > max_width {
            break;
        }
        output.push(ch);
    }

    if output.is_empty() {
        ellipsis.to_owned()
    } else {
        output.push_str(ellipsis);
        output
    }
}

fn signed_loword(value: isize) -> i32 {
    (value as u32 & 0xffff) as i16 as i32
}

fn signed_hiword(value: isize) -> i32 {
    ((value as u32 >> 16) & 0xffff) as i16 as i32
}

fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> SkColor {
    SkColor::from_argb(alpha, red, green, blue)
}

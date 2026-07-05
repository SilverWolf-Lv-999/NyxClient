use std::{
    sync::{Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WAIT_FAILED, WPARAM},
    Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, VREFRESH},
    Media::{timeBeginPeriod, timeEndPeriod},
    System::Threading::GetCurrentThreadId,
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        WindowsAndMessaging::{
            CHILDID_SELF, DispatchMessageW, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CONTENTSCROLLED,
            EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
            EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_REORDER, EVENT_OBJECT_SHOW,
            EVENT_OBJECT_STATECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_DESKTOPSWITCH,
            EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART,
            GetSystemMetrics, GetWindowThreadProcessId, MSG, MsgWaitForMultipleObjects,
            OBJID_CLIENT, OBJID_WINDOW, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW,
            QS_ALLINPUT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN, TranslateMessage, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
            WM_APP, WM_NULL, WM_QUIT,
        },
    },
};

use crate::{
    event::api::{EventBus, SharedEventBus},
    manager::notification_manager,
};

const STOP_MESSAGE: u32 = WM_APP + 0x4E5B;
const WAKE_MESSAGE: u32 = WM_APP + 0x4E5C;
const FALLBACK_REFRESH_RATE_HZ: u32 = 60;
const MIN_REFRESH_RATE_HZ: u32 = 30;
const MAX_REFRESH_RATE_HZ: u32 = 360;
const REFRESH_RATE_POLL_INTERVAL: Duration = Duration::from_secs(2);

static RENDER_CONTEXT: OnceLock<Mutex<Option<RenderHookContext>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render2DSource {
    WindowsDesktop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render2DTrigger {
    DisplayRefresh,
    SystemForeground,
    SystemMoveSizeStart,
    SystemMoveSizeEnd,
    SystemDesktopSwitch,
    ObjectCreate,
    ObjectDestroy,
    ObjectShow,
    ObjectHide,
    ObjectReorder,
    ObjectStateChange,
    ObjectLocationChange,
    ObjectContentScrolled,
    ObjectCloaked,
    ObjectUncloaked,
    Other(u32),
}

impl Render2DTrigger {
    pub const fn from_raw_event(event: u32) -> Self {
        match event {
            EVENT_SYSTEM_FOREGROUND => Self::SystemForeground,
            EVENT_SYSTEM_MOVESIZESTART => Self::SystemMoveSizeStart,
            EVENT_SYSTEM_MOVESIZEEND => Self::SystemMoveSizeEnd,
            EVENT_SYSTEM_DESKTOPSWITCH => Self::SystemDesktopSwitch,
            EVENT_OBJECT_CREATE => Self::ObjectCreate,
            EVENT_OBJECT_DESTROY => Self::ObjectDestroy,
            EVENT_OBJECT_SHOW => Self::ObjectShow,
            EVENT_OBJECT_HIDE => Self::ObjectHide,
            EVENT_OBJECT_REORDER => Self::ObjectReorder,
            EVENT_OBJECT_STATECHANGE => Self::ObjectStateChange,
            EVENT_OBJECT_LOCATIONCHANGE => Self::ObjectLocationChange,
            EVENT_OBJECT_CONTENTSCROLLED => Self::ObjectContentScrolled,
            EVENT_OBJECT_CLOAKED => Self::ObjectCloaked,
            EVENT_OBJECT_UNCLOAKED => Self::ObjectUncloaked,
            other => Self::Other(other),
        }
    }

    pub const fn is_visual_update(self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Render2DViewport {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Render2DViewport {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Render2DChange {
    pub trigger: Render2DTrigger,
    pub raw_event: u32,
    pub hwnd: isize,
    pub object_id: i32,
    pub child_id: i32,
    pub event_thread_id: u32,
    pub event_time: u32,
    pub process_id: u32,
}

impl Render2DChange {
    pub const fn new(
        trigger: Render2DTrigger,
        raw_event: u32,
        hwnd: isize,
        object_id: i32,
        child_id: i32,
        event_thread_id: u32,
        event_time: u32,
        process_id: u32,
    ) -> Self {
        Self {
            trigger,
            raw_event,
            hwnd,
            object_id,
            child_id,
            event_thread_id,
            event_time,
            process_id,
        }
    }

    pub const fn display_refresh() -> Self {
        Self::new(
            Render2DTrigger::DisplayRefresh,
            0,
            0,
            OBJID_WINDOW.0,
            CHILDID_SELF as i32,
            0,
            0,
            0,
        )
    }
}

#[derive(Debug, Clone)]
pub struct EventRender2D {
    pub source: Render2DSource,
    pub frame: u64,
    pub delta: Duration,
    pub timestamp: SystemTime,
    pub instant: Instant,
    pub viewport: Render2DViewport,
    pub change: Render2DChange,
    pub coalesced_events: u32,
    pub refresh_rate_hz: u32,
    pub target_frame_interval: Duration,
}

impl EventRender2D {
    pub fn windows_desktop(
        frame: u64,
        delta: Duration,
        viewport: Render2DViewport,
        change: Render2DChange,
        coalesced_events: u32,
        refresh_rate_hz: u32,
        target_frame_interval: Duration,
    ) -> Self {
        Self {
            source: Render2DSource::WindowsDesktop,
            frame,
            delta,
            timestamp: SystemTime::now(),
            instant: Instant::now(),
            viewport,
            change,
            coalesced_events,
            refresh_rate_hz,
            target_frame_interval,
        }
    }
}

#[derive(Debug)]
pub enum WindowsRender2DError {
    AlreadyRunning,
    ObjectHook(windows::core::Error),
    SystemHook(windows::core::Error),
    StartupFailed,
}

pub struct WindowsRender2DPublisher {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl WindowsRender2DPublisher {
    pub fn start(event_bus: SharedEventBus) -> Result<Self, WindowsRender2DError> {
        let timing = DisplayTiming::current();
        let context = RENDER_CONTEXT.get_or_init(|| Mutex::new(None));
        {
            let mut context = context
                .lock()
                .map_err(|_| WindowsRender2DError::StartupFailed)?;
            if context.is_some() {
                return Err(WindowsRender2DError::AlreadyRunning);
            }
            *context = Some(RenderHookContext::new(event_bus, timing));
        }

        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = match thread::Builder::new()
            .name("windows-render2d-publisher".to_owned())
            .spawn(move || run_render_hook_thread(startup_tx))
        {
            Ok(worker) => worker,
            Err(_) => {
                clear_context();
                return Err(WindowsRender2DError::StartupFailed);
            }
        };

        match startup_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                clear_context();
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                clear_context();
                let _ = worker.join();
                Err(WindowsRender2DError::StartupFailed)
            }
        }
    }

    pub const fn thread_id(&self) -> u32 {
        self.thread_id
    }

    pub fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, STOP_MESSAGE, WPARAM(0), LPARAM(0));
            }
            let _ = worker.join();
        }
    }
}

impl Drop for WindowsRender2DPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

struct RenderHookContext {
    event_bus: SharedEventBus,
    thread_id: u32,
    timing: DisplayTiming,
    last_refresh_rate_check_at: Instant,
    frame: u64,
    last_frame_at: Option<Instant>,
    pending_change: Option<Render2DChange>,
    pending_count: u32,
}

impl RenderHookContext {
    fn new(event_bus: SharedEventBus, timing: DisplayTiming) -> Self {
        Self {
            event_bus,
            thread_id: 0,
            timing,
            last_refresh_rate_check_at: Instant::now(),
            frame: 0,
            last_frame_at: None,
            pending_change: None,
            pending_count: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DisplayTiming {
    refresh_rate_hz: u32,
    frame_interval: Duration,
}

impl DisplayTiming {
    fn current() -> Self {
        let refresh_rate_hz = current_refresh_rate_hz();
        Self {
            refresh_rate_hz,
            frame_interval: frame_interval(refresh_rate_hz),
        }
    }
}

struct InstalledRenderHooks {
    object: HWINEVENTHOOK,
    system: HWINEVENTHOOK,
}

impl InstalledRenderHooks {
    fn install() -> Result<Self, WindowsRender2DError> {
        let object = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_CREATE,
                EVENT_OBJECT_UNCLOAKED,
                None,
                Some(render_win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if object.is_invalid() {
            return Err(WindowsRender2DError::ObjectHook(
                windows::core::Error::from_win32(),
            ));
        }

        let system = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_DESKTOPSWITCH,
                None,
                Some(render_win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if system.is_invalid() {
            unsafe {
                let _ = UnhookWinEvent(object);
            }
            return Err(WindowsRender2DError::SystemHook(
                windows::core::Error::from_win32(),
            ));
        }

        Ok(Self { object, system })
    }
}

impl Drop for InstalledRenderHooks {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.object);
            let _ = UnhookWinEvent(self.system);
        }
    }
}

fn run_render_hook_thread(startup_tx: mpsc::Sender<Result<u32, WindowsRender2DError>>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_NULL, WM_NULL, PM_NOREMOVE);
    }

    if !set_context_thread_id(thread_id) {
        let _ = startup_tx.send(Err(WindowsRender2DError::StartupFailed));
        clear_context();
        return;
    }

    let hooks = match InstalledRenderHooks::install() {
        Ok(hooks) => hooks,
        Err(error) => {
            let _ = startup_tx.send(Err(error));
            clear_context();
            return;
        }
    };

    let _ = startup_tx.send(Ok(thread_id));
    let _timer_resolution = TimerResolution::begin(1);
    render_message_loop();
    flush_due_render_event(true);
    drop(hooks);
    clear_context();
}

fn render_message_loop() {
    let mut stopping = false;

    while !stopping {
        let wait_result =
            unsafe { MsgWaitForMultipleObjects(None, false, render_wait_timeout(), QS_ALLINPUT) };
        if wait_result == WAIT_FAILED {
            break;
        }

        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
            if msg.message == STOP_MESSAGE || msg.message == WM_QUIT {
                stopping = true;
                break;
            }

            if msg.message != WAKE_MESSAGE {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        flush_due_render_event(stopping);
    }
}

unsafe extern "system" fn render_win_event_proc(
    _hook: HWINEVENTHOOK,
    raw_event: u32,
    hwnd: HWND,
    object_id: i32,
    child_id: i32,
    event_thread_id: u32,
    event_time: u32,
) {
    let trigger = Render2DTrigger::from_raw_event(raw_event);
    if !trigger.is_visual_update() {
        return;
    }

    if is_object_event(raw_event) && !is_render_object(object_id, child_id) {
        return;
    }

    let process_id = window_process_id(hwnd);
    record_render_change(Render2DChange::new(
        trigger,
        raw_event,
        hwnd.0 as isize,
        object_id,
        child_id,
        event_thread_id,
        event_time,
        process_id,
    ));
}

fn record_render_change(change: Render2DChange) {
    let Some(context) = RENDER_CONTEXT.get() else {
        return;
    };

    let thread_id = {
        let Ok(mut context) = context.lock() else {
            return;
        };
        let Some(context) = context.as_mut() else {
            return;
        };

        context.pending_change = Some(change);
        context.pending_count = context.pending_count.saturating_add(1);
        context.thread_id
    };

    let current_thread_id = unsafe { GetCurrentThreadId() };
    if thread_id != 0 && thread_id != current_thread_id {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WAKE_MESSAGE, WPARAM(0), LPARAM(0));
        }
    }
}

fn flush_due_render_event(force: bool) {
    if !notification_manager::has_visible_notifications()
        && !notification_manager::has_windows_desktop_frame()
    {
        let _ = take_due_render_event(force);
        return;
    }

    let Some((event_bus, mut event)) = take_due_render_event(force) else {
        return;
    };
    EventBus::dispatch_shared(&event_bus, &mut event);
}

fn take_due_render_event(force: bool) -> Option<(SharedEventBus, EventRender2D)> {
    let context = RENDER_CONTEXT.get()?;
    let mut context = context.lock().ok()?;
    let context = context.as_mut()?;
    if force && context.pending_count == 0 && context.pending_change.is_none() {
        return None;
    }

    let now = Instant::now();
    update_display_timing(context, now);

    if !force
        && let Some(last_frame_at) = context.last_frame_at
        && now.duration_since(last_frame_at) < context.timing.frame_interval
    {
        return None;
    }

    let change = context
        .pending_change
        .take()
        .unwrap_or_else(Render2DChange::display_refresh);
    let coalesced_events = context.pending_count;
    context.pending_count = 0;
    context.frame = context.frame.wrapping_add(1);

    let delta = context
        .last_frame_at
        .map_or(Duration::ZERO, |last_frame_at| {
            now.duration_since(last_frame_at)
        });
    context.last_frame_at = Some(now);

    let event = EventRender2D::windows_desktop(
        context.frame,
        delta,
        current_viewport(),
        change,
        coalesced_events,
        context.timing.refresh_rate_hz,
        context.timing.frame_interval,
    );

    Some((context.event_bus.clone(), event))
}

fn render_wait_timeout() -> u32 {
    let Some(context) = RENDER_CONTEXT.get() else {
        return 0;
    };
    let Ok(context) = context.lock() else {
        return 0;
    };
    let Some(context) = context.as_ref() else {
        return 0;
    };
    let Some(last_frame_at) = context.last_frame_at else {
        return 0;
    };

    let elapsed = last_frame_at.elapsed();
    if elapsed >= context.timing.frame_interval {
        return 0;
    }

    let remaining = context.timing.frame_interval - elapsed;
    let millis = remaining.as_millis();
    if millis == 0 {
        1
    } else {
        millis.min(u128::from(u32::MAX)) as u32
    }
}

fn update_display_timing(context: &mut RenderHookContext, now: Instant) {
    if now.duration_since(context.last_refresh_rate_check_at) < REFRESH_RATE_POLL_INTERVAL {
        return;
    }

    context.last_refresh_rate_check_at = now;
    context.timing = DisplayTiming::current();
}

fn set_context_thread_id(thread_id: u32) -> bool {
    let Some(context) = RENDER_CONTEXT.get() else {
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

fn clear_context() {
    if let Some(context) = RENDER_CONTEXT.get()
        && let Ok(mut context) = context.lock()
    {
        *context = None;
    }
}

fn is_object_event(event: u32) -> bool {
    (EVENT_OBJECT_CREATE..=EVENT_OBJECT_UNCLOAKED).contains(&event)
}

fn is_render_object(object_id: i32, child_id: i32) -> bool {
    child_id == CHILDID_SELF as i32 && (object_id == OBJID_WINDOW.0 || object_id == OBJID_CLIENT.0)
}

fn window_process_id(hwnd: HWND) -> u32 {
    if hwnd.0.is_null() {
        return 0;
    }

    let mut process_id = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    process_id
}

fn current_viewport() -> Render2DViewport {
    Render2DViewport::new(
        unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
        unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
        unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) },
        unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) },
    )
}

fn current_refresh_rate_hz() -> u32 {
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.0.is_null() {
        return FALLBACK_REFRESH_RATE_HZ;
    }

    let refresh = unsafe { GetDeviceCaps(Some(screen_dc), VREFRESH) };
    unsafe {
        let _ = ReleaseDC(None, screen_dc);
    }

    if refresh > 1 {
        (refresh as u32).clamp(MIN_REFRESH_RATE_HZ, MAX_REFRESH_RATE_HZ)
    } else {
        FALLBACK_REFRESH_RATE_HZ
    }
}

fn frame_interval(refresh_rate_hz: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(refresh_rate_hz.max(1)))
}

struct TimerResolution {
    period_ms: u32,
    active: bool,
}

impl TimerResolution {
    fn begin(period_ms: u32) -> Self {
        let active = unsafe { timeBeginPeriod(period_ms) } == 0;
        Self { period_ms, active }
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                let _ = timeEndPeriod(self.period_ms);
            }
        }
    }
}

use std::sync::{Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            Console::{
                CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
                CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
            },
            LibraryLoader::GetModuleHandleW,
            Threading::GetCurrentThreadId,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, ENDSESSION_LOGOFF,
            GetMessageW, HMENU, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
            PBT_POWERSETTINGCHANGE, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, RegisterClassW,
            TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_ENDSESSION, WM_NULL,
            WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_QUIT, WNDCLASSW,
        },
    },
    core::{BOOL, w},
};

use crate::event::{
    api::{EventBus, SharedEventBus},
    implementations::{EventWindows, WindowsSessionAction},
};

const STOP_MESSAGE: u32 = WM_APP + 0x4E5A;

static SESSION_CONTEXT: OnceLock<Mutex<Option<SharedEventBus>>> = OnceLock::new();

#[derive(Debug)]
pub enum WindowsSessionError {
    AlreadyRunning,
    ConsoleHandler(windows::core::Error),
    WindowClass,
    Window(windows::core::Error),
    StartupFailed,
}

pub struct WindowsSessionPublisher {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl WindowsSessionPublisher {
    pub fn start(event_bus: SharedEventBus) -> Result<Self, WindowsSessionError> {
        let context = SESSION_CONTEXT.get_or_init(|| Mutex::new(None));
        {
            let mut context = context
                .lock()
                .map_err(|_| WindowsSessionError::StartupFailed)?;
            if context.is_some() {
                return Err(WindowsSessionError::AlreadyRunning);
            }
            *context = Some(event_bus);
        }

        if let Err(error) = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), true) } {
            clear_context();
            return Err(WindowsSessionError::ConsoleHandler(error));
        }

        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = match thread::Builder::new()
            .name("windows-session-publisher".to_owned())
            .spawn(move || run_message_thread(startup_tx))
        {
            Ok(worker) => worker,
            Err(_) => {
                let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
                clear_context();
                return Err(WindowsSessionError::StartupFailed);
            }
        };

        match startup_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
                clear_context();
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
                clear_context();
                let _ = worker.join();
                Err(WindowsSessionError::StartupFailed)
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
            let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
            clear_context();
        }
    }
}

impl Drop for WindowsSessionPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_message_thread(startup_tx: mpsc::Sender<Result<u32, WindowsSessionError>>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_NULL, WM_NULL, PM_NOREMOVE);
    }

    let window = match MessageWindow::create() {
        Ok(window) => window,
        Err(error) => {
            let _ = startup_tx.send(Err(error));
            return;
        }
    };

    let _ = startup_tx.send(Ok(thread_id));
    message_loop();
    drop(window);
}

struct MessageWindow {
    hwnd: HWND,
}

impl MessageWindow {
    fn create() -> Result<Self, WindowsSessionError> {
        let hmodule =
            unsafe { GetModuleHandleW(None) }.map_err(|_| WindowsSessionError::WindowClass)?;
        let class_name = w!("NyxClientSessionPublisherWindow");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hmodule.into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        let _ = unsafe { RegisterClassW(&window_class) };

        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("NyxClient Session Publisher"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None::<HMENU>,
                Some(hmodule.into()),
                None,
            )
        }
        .map_err(WindowsSessionError::Window)?;

        Ok(Self { hwnd })
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn message_loop() {
    let mut msg = MSG::default();

    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 || msg.message == STOP_MESSAGE || msg.message == WM_QUIT {
            break;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_QUERYENDSESSION => {
            let mut event = EventWindows::raw(
                WindowsSessionAction::QueryEndSession,
                message,
                wparam.0,
                lparam.0,
            );
            dispatch(&mut event);
            if event.is_cancelled() {
                return LRESULT(0);
            }
            LRESULT(1)
        }
        WM_ENDSESSION => {
            let action = if lparam.0 as u32 & ENDSESSION_LOGOFF != 0 {
                WindowsSessionAction::Logoff
            } else if wparam.0 != 0 {
                WindowsSessionAction::Shutdown
            } else {
                WindowsSessionAction::EndSession
            };
            let mut event = EventWindows::raw(action, message, wparam.0, lparam.0);
            dispatch(&mut event);
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            let action = match wparam.0 as u32 {
                PBT_APMSUSPEND => WindowsSessionAction::Suspend,
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => WindowsSessionAction::Resume,
                PBT_POWERSETTINGCHANGE => WindowsSessionAction::PowerStatusChanged,
                _ => WindowsSessionAction::Unknown,
            };
            let mut event = EventWindows::raw(action, message, wparam.0, lparam.0);
            dispatch(&mut event);
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    let action = match ctrl_type {
        CTRL_C_EVENT => WindowsSessionAction::ConsoleCtrlC,
        CTRL_BREAK_EVENT => WindowsSessionAction::ConsoleBreak,
        CTRL_CLOSE_EVENT => WindowsSessionAction::ConsoleClose,
        CTRL_LOGOFF_EVENT => WindowsSessionAction::Logoff,
        CTRL_SHUTDOWN_EVENT => WindowsSessionAction::Shutdown,
        _ => WindowsSessionAction::Unknown,
    };

    let mut event = EventWindows::console(action, ctrl_type);
    dispatch(&mut event);
    event.is_cancelled().into()
}

fn dispatch<E>(event: &mut E)
where
    E: crate::event::api::Event + 'static,
{
    if let Some(context) = SESSION_CONTEXT.get() {
        if let Ok(context) = context.lock() {
            if let Some(event_bus) = context.as_ref() {
                EventBus::dispatch_shared(event_bus, event);
            }
        }
    }
}

fn clear_context() {
    if let Some(context) = SESSION_CONTEXT.get() {
        if let Ok(mut context) = context.lock() {
            *context = None;
        }
    }
}

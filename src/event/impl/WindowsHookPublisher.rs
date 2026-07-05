use std::{
    sync::{Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
};

use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL,
            VK_RMENU, VK_RSHIFT, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
            LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED, MSG,
            MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
            TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN,
            WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
            WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
            WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
        },
    },
};

use crate::event::{
    api::{EventBus, SharedEventBus},
    implementations::{
        event_keyboard::{EventKeyboard, KeyModifiers, KeyState},
        event_mouse::{EventMouse, MouseAction, MouseButton},
    },
};

const STOP_MESSAGE: u32 = WM_APP + 0x4E59;
const KEY_STATE_DOWN_MASK: i16 = 0x8000u16 as i16;

static HOOK_CONTEXT: OnceLock<Mutex<Option<SharedEventBus>>> = OnceLock::new();

#[derive(Debug)]
pub enum WindowsHookError {
    AlreadyRunning,
    KeyboardHook(windows::core::Error),
    MouseHook(windows::core::Error),
    StartupFailed,
}

pub struct WindowsHookPublisher {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl WindowsHookPublisher {
    pub fn start(event_bus: SharedEventBus) -> Result<Self, WindowsHookError> {
        let context = HOOK_CONTEXT.get_or_init(|| Mutex::new(None));
        {
            let mut context = context
                .lock()
                .map_err(|_| WindowsHookError::StartupFailed)?;
            if context.is_some() {
                return Err(WindowsHookError::AlreadyRunning);
            }
            *context = Some(event_bus);
        }

        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = match thread::Builder::new()
            .name("windows-hook-publisher".to_owned())
            .spawn(move || run_hook_thread(startup_tx))
        {
            Ok(worker) => worker,
            Err(_) => {
                clear_context();
                return Err(WindowsHookError::StartupFailed);
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
                Err(WindowsHookError::StartupFailed)
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

impl Drop for WindowsHookPublisher {
    fn drop(&mut self) {
        self.stop();
    }
}

struct InstalledHooks {
    keyboard: HHOOK,
    mouse: HHOOK,
}

impl InstalledHooks {
    fn install() -> Result<Self, WindowsHookError> {
        let keyboard =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0) }
                .map_err(WindowsHookError::KeyboardHook)?;
        let mouse = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0) }
        {
            Ok(mouse) => mouse,
            Err(error) => {
                unsafe {
                    let _ = UnhookWindowsHookEx(keyboard);
                }
                return Err(WindowsHookError::MouseHook(error));
            }
        };

        Ok(Self { keyboard, mouse })
    }
}

impl Drop for InstalledHooks {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.keyboard);
            let _ = UnhookWindowsHookEx(self.mouse);
        }
    }
}

fn run_hook_thread(startup_tx: mpsc::Sender<Result<u32, WindowsHookError>>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_NULL, WM_NULL, PM_NOREMOVE);
    }

    let hooks = match InstalledHooks::install() {
        Ok(hooks) => hooks,
        Err(error) => {
            let _ = startup_tx.send(Err(error));
            clear_context();
            return;
        }
    };

    let _ = startup_tx.send(Ok(thread_id));
    message_loop();
    drop(hooks);
    clear_context();
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

unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam.0 as u32;
        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
            let hook_data = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if publish_keyboard_event(message, hook_data) {
                return LRESULT(1);
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam.0 as u32;
        let hook_data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if publish_mouse_event(message, hook_data) {
            return LRESULT(1);
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn publish_keyboard_event(message: u32, hook_data: &KBDLLHOOKSTRUCT) -> bool {
    let state = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => KeyState::Pressed,
        WM_KEYUP | WM_SYSKEYUP => KeyState::Released,
        _ => return false,
    };

    let flags = hook_data.flags.0;
    let mut event = EventKeyboard::new(
        hook_data.vkCode,
        hook_data.scanCode,
        state,
        current_modifiers(),
        flags,
        hook_data.time,
        hook_data.dwExtraInfo,
        hook_data.flags.contains(LLKHF_INJECTED),
        hook_data.flags.contains(LLKHF_LOWER_IL_INJECTED),
    );

    dispatch(&mut event);
    event.is_cancelled()
}

fn publish_mouse_event(message: u32, hook_data: &MSLLHOOKSTRUCT) -> bool {
    let action = match message {
        WM_MOUSEMOVE => MouseAction::Move,
        WM_LBUTTONDOWN => MouseAction::Pressed(MouseButton::Left),
        WM_LBUTTONUP => MouseAction::Released(MouseButton::Left),
        WM_RBUTTONDOWN => MouseAction::Pressed(MouseButton::Right),
        WM_RBUTTONUP => MouseAction::Released(MouseButton::Right),
        WM_MBUTTONDOWN => MouseAction::Pressed(MouseButton::Middle),
        WM_MBUTTONUP => MouseAction::Released(MouseButton::Middle),
        WM_XBUTTONDOWN => MouseAction::Pressed(x_button(hook_data.mouseData)),
        WM_XBUTTONUP => MouseAction::Released(x_button(hook_data.mouseData)),
        WM_MOUSEWHEEL => MouseAction::Wheel {
            delta: high_word_signed(hook_data.mouseData),
        },
        WM_MOUSEHWHEEL => MouseAction::HorizontalWheel {
            delta: high_word_signed(hook_data.mouseData),
        },
        _ => return false,
    };

    let mut event = EventMouse::new(
        hook_data.pt.x,
        hook_data.pt.y,
        action,
        hook_data.mouseData,
        hook_data.flags,
        hook_data.time,
        hook_data.dwExtraInfo,
        hook_data.flags & LLMHF_INJECTED != 0,
        hook_data.flags & LLMHF_LOWER_IL_INJECTED != 0,
    );

    dispatch(&mut event);
    event.is_cancelled()
}

fn dispatch<E>(event: &mut E)
where
    E: crate::event::api::Event + 'static,
{
    if let Some(context) = HOOK_CONTEXT.get() {
        if let Ok(context) = context.lock() {
            if let Some(event_bus) = context.as_ref() {
                EventBus::dispatch_shared(event_bus, event);
            }
        }
    }
}

fn current_modifiers() -> KeyModifiers {
    KeyModifiers {
        shift: is_key_down(VK_SHIFT.0) || is_key_down(VK_LSHIFT.0) || is_key_down(VK_RSHIFT.0),
        control: is_key_down(VK_CONTROL.0)
            || is_key_down(VK_LCONTROL.0)
            || is_key_down(VK_RCONTROL.0),
        alt: is_key_down(VK_MENU.0) || is_key_down(VK_LMENU.0) || is_key_down(VK_RMENU.0),
    }
}

fn is_key_down(vk_code: u16) -> bool {
    unsafe { GetAsyncKeyState(vk_code as i32) & KEY_STATE_DOWN_MASK != 0 }
}

fn high_word_signed(value: u32) -> i16 {
    ((value >> 16) & 0xffff) as u16 as i16
}

fn x_button(mouse_data: u32) -> MouseButton {
    match ((mouse_data >> 16) & 0xffff) as u16 {
        XBUTTON1 => MouseButton::X1,
        XBUTTON2 => MouseButton::X2,
        _ => MouseButton::X1,
    }
}

fn clear_context() {
    if let Some(context) = HOOK_CONTEXT.get() {
        if let Ok(mut context) = context.lock() {
            *context = None;
        }
    }
}

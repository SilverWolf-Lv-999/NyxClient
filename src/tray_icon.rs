use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

use nyx_client::client_icon;
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
                NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
                DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
                GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HICON, HMENU, IMAGE_ICON,
                LR_LOADFROMFILE, LoadImageW, MF_STRING, MSG, PM_NOREMOVE, PeekMessageW,
                PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW, SM_CXSMICON,
                SM_CYSMICON, SetForegroundWindow, SetWindowLongPtrW, TPM_BOTTOMALIGN,
                TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_NCCREATE, WM_NCDESTROY, WM_NULL, WM_QUIT,
                WM_RBUTTONUP, WNDCLASSW,
            },
        },
    },
    core::{PCWSTR, w},
};

const TRAY_ICON_ID: u32 = 1;
const TRAY_EXIT_COMMAND: usize = 1001;
const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 0x4E59;
const TRAY_STOP_MESSAGE: u32 = WM_APP + 0x4E5A;

pub struct TrayIcon {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl TrayIcon {
    pub fn start(running: Arc<AtomicBool>) -> Result<Self, String> {
        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("nyx-tray-icon".to_owned())
            .spawn(move || run_tray_thread(startup_tx, running))
            .map_err(|error| format!("failed to spawn tray icon thread: {error}"))?;

        match startup_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                let _ = worker.join();
                Err(format!("tray icon thread stopped during startup: {error}"))
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, TRAY_STOP_MESSAGE, WPARAM(0), LPARAM(0));
            }
            let _ = worker.join();
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_tray_thread(startup_tx: mpsc::Sender<Result<u32, String>>, running: Arc<AtomicBool>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_NULL, WM_NULL, PM_NOREMOVE);
    }

    let window = match TrayWindow::create(running) {
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

struct TrayWindow {
    hwnd: HWND,
}

impl TrayWindow {
    fn create(running: Arc<AtomicBool>) -> Result<Self, String> {
        let hmodule = unsafe { GetModuleHandleW(None) }
            .map_err(|error| format!("failed to get module handle for tray icon: {error}"))?;
        let hinstance = HINSTANCE(hmodule.0);
        let class_name = w!("NyxClientTrayWindow");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };

        unsafe {
            let _ = RegisterClassW(&window_class);
        }

        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("NyxClient Tray"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None::<HMENU>,
                Some(hinstance),
                None,
            )
        }
        .map_err(|error| format!("failed to create tray icon window: {error}"))?;

        let hicon = match load_client_icon() {
            Ok(hicon) => hicon,
            Err(error) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(error);
            }
        };

        let menu = match create_tray_menu() {
            Ok(menu) => menu,
            Err(error) => {
                unsafe {
                    let _ = DestroyIcon(hicon);
                    let _ = DestroyWindow(hwnd);
                }
                return Err(error);
            }
        };

        let context = Box::into_raw(Box::new(TrayContext {
            running,
            hicon,
            menu,
        }));
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, context as isize);
        }

        if !add_tray_icon(hwnd, hicon) {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(context));
                let _ = DestroyWindow(hwnd);
            }
            return Err("failed to add NyxClient tray icon".to_owned());
        }

        Ok(Self { hwnd })
    }
}

impl Drop for TrayWindow {
    fn drop(&mut self) {
        remove_tray_icon(self.hwnd);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct TrayContext {
    running: Arc<AtomicBool>,
    hicon: HICON,
    menu: HMENU,
}

impl Drop for TrayContext {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.menu);
            let _ = DestroyIcon(self.hicon);
        }
    }
}

fn message_loop() {
    let mut msg = MSG::default();

    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 || msg.message == TRAY_STOP_MESSAGE || msg.message == WM_QUIT {
            break;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn load_client_icon() -> Result<HICON, String> {
    let path = client_icon::cached_ico_path()
        .ok_or_else(|| "failed to cache NyxClient icon".to_owned())?;
    let path_wide = path_to_wide_null(&path);
    let cx = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) }.max(16);
    let handle = unsafe {
        LoadImageW(
            None,
            PCWSTR(path_wide.as_ptr()),
            IMAGE_ICON,
            cx,
            cy,
            LR_LOADFROMFILE,
        )
    }
    .map_err(|error| format!("failed to load NyxClient tray icon: {error}"))?;

    Ok(HICON(handle.0))
}

fn create_tray_menu() -> Result<HMENU, String> {
    let menu = unsafe { CreatePopupMenu() }
        .map_err(|error| format!("failed to create tray menu: {error}"))?;
    unsafe {
        AppendMenuW(menu, MF_STRING, TRAY_EXIT_COMMAND, w!("退出"))
            .map_err(|error| format!("failed to add tray exit menu item: {error}"))?;
    }

    Ok(menu)
}

fn add_tray_icon(hwnd: HWND, hicon: HICON) -> bool {
    let mut data = notify_icon_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = TRAY_CALLBACK_MESSAGE;
    data.hIcon = hicon;
    copy_wide_tip(&mut data.szTip, "NyxClient");

    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data).0 != 0 };
    if added {
        unsafe {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }

    added
}

fn remove_tray_icon(hwnd: HWND) {
    let data = notify_icon_data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn notify_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        ..Default::default()
    }
}

fn copy_wide_tip(buffer: &mut [u16], value: &str) {
    let max_len = buffer.len().saturating_sub(1);
    for (slot, unit) in buffer.iter_mut().take(max_len).zip(value.encode_utf16()) {
        *slot = unit;
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        TRAY_CALLBACK_MESSAGE => {
            let event = (lparam.0 as u32) & 0xffff;
            if matches!(event, WM_CONTEXTMENU | WM_RBUTTONUP) {
                show_tray_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if wparam.0 & 0xffff == TRAY_EXIT_COMMAND {
                if let Some(context) = unsafe { context_from_hwnd(hwnd) } {
                    context.running.store(false, Ordering::Release);
                }
                unsafe {
                    PostQuitMessage(0);
                }
                return LRESULT(0);
            }

            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_NCCREATE => LRESULT(1),
        WM_NCDESTROY => {
            let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayContext };
            if !raw.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(raw));
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn show_tray_menu(hwnd: HWND) {
    let Some(context) = (unsafe { context_from_hwnd(hwnd) }) else {
        return;
    };
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return;
    }

    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            context.menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

unsafe fn context_from_hwnd(hwnd: HWND) -> Option<&'static mut TrayContext> {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayContext };
    if raw.is_null() {
        None
    } else {
        Some(unsafe { &mut *raw })
    }
}

fn path_to_wide_null(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

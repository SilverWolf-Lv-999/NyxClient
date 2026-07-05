use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::null_mut,
    sync::atomic::{AtomicIsize, Ordering},
    thread,
    time::Instant,
};

use crate::modules::{Category, Module, ModuleInfo, ModuleState};
use windows::{
    Win32::{
        Foundation::{COLORREF, GENERIC_READ, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_PIXEL_FORMAT,
                },
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
                D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
                D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory,
                ID2D1HwndRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_DEMI_BOLD,
                DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWriteCreateFactory,
                IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT},
            Imaging::{
                CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
                IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
                WICDecodeMetadataCacheOnLoad,
            },
        },
        System::{
            Com::Urlmon::URLDownloadToFileW,
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IBindStatusCallback,
            },
            LibraryLoader::GetModuleHandleW,
            WindowsProgramming::GetUserNameW,
        },
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, VK_ESCAPE},
            Shell::IsUserAnAdmin,
            WindowsAndMessaging::{
                CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW,
                DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetMessageW,
                GetSystemMetrics, GetWindowLongPtrW, HTCAPTION, IDC_ARROW, KillTimer, LWA_COLORKEY,
                LoadCursorW, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
                SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE, SendMessageW,
                SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, ShowWindow,
                TranslateMessage, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN,
                WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_NCLBUTTONDOWN, WM_PAINT, WM_SIZE,
                WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{IUnknown, PCWSTR, PWSTR, w},
};
use windows_numerics::{Matrix3x2, Vector2};

const WINDOW_WIDTH: i32 = 640;
const WINDOW_HEIGHT: i32 = 440;
const CLICK_GUI_SCALE: f32 = 1.7;
const WINDOW_MARGIN: f32 = 14.0;
const PANEL_MARGIN: f32 = 20.0;
const HEADER_DRAG_HEIGHT: f32 = 88.0;
const SIDEBAR_LEFT: f32 = 28.0;
const SIDEBAR_RIGHT: f32 = 176.0;
const CATEGORY_TOP: f32 = 112.0;
const CATEGORY_HEIGHT: f32 = 34.0;
const CATEGORY_STEP: f32 = 42.0;
const CONTENT_LEFT: f32 = 204.0;
const CARD_TOP: f32 = 136.0;
const CARD_HEIGHT: f32 = 62.0;
const CARD_STEP: f32 = 76.0;
const SWITCH_WIDTH: f32 = 44.0;
const SWITCH_HEIGHT: f32 = 24.0;
const ESC_CLOSE_TIMER_ID: usize = 1;
const ESC_CLOSE_POLL_MS: u32 = 30;
const ANIMATION_TIMER_ID: usize = 2;
const ANIMATION_FRAME_MS: u32 = 8;
const ENTRY_ANIMATION_MS: f32 = 620.0;
const EXIT_ANIMATION_MS: f32 = 920.0;
const ENTRY_START_SCALE: f32 = 0.58;
const EXIT_OVERSHOOT_SCALE: f32 = 1.05;
const EXIT_END_SCALE: f32 = 0.06;
const EXIT_OVERSHOOT_PORTION: f32 = 0.45;
const TRANSPARENT_KEY: COLORREF = COLORREF(0x0003_0201);
const KEY_STATE_DOWN_MASK: i16 = 0x8000_u16 as i16;
const STARTING_HWND: isize = -1;
const ICON_URL: &str = "https://raw.githubusercontent.com/github/explore/main/topics/rust/rust.png";

static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);

#[derive(Debug)]
pub struct ClickGui {
    info: ModuleInfo,
    state: ModuleState,
}

impl ClickGui {
    pub fn new() -> Self {
        let mut state = ModuleState::new();
        state.set_config_saving(false);

        Self {
            info: ModuleInfo::new("ClickGui", "Direct2D powered click GUI.", Category::System),
            state,
        }
    }

    fn reset_toggle_state(&mut self) {
        let key_bind = self.state.key_bind();
        self.state = ModuleState::new();
        self.state.set_key_bind(key_bind);
        self.state.set_config_saving(false);
    }
}

impl Default for ClickGui {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ClickGui {
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
        toggle_gui_window();
        self.reset_toggle_state();
    }

    fn should_notify_toggle(&self) -> bool {
        false
    }
}

fn toggle_gui_window() {
    let hwnd_value = OPEN_HWND.load(Ordering::Acquire);
    if hwnd_value > 0 {
        let hwnd = HWND(hwnd_value as *mut c_void);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        return;
    }

    if OPEN_HWND
        .compare_exchange(0, STARTING_HWND, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    if thread::Builder::new()
        .name("nyx-click-gui".to_owned())
        .spawn(|| {
            if let Err(error) = run_gui_window() {
                eprintln!("ClickGui failed to start: {error:?}");
                OPEN_HWND.store(0, Ordering::Release);
            }
        })
        .is_err()
    {
        OPEN_HWND.store(0, Ordering::Release);
    }
}

fn run_gui_window() -> windows::core::Result<()> {
    let coinit = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    coinit.ok()?;
    let _com_scope = ComScope;

    let mut app = Box::new(GuiApp::new()?);
    let app_ptr = app.as_mut() as *mut GuiApp;

    let hmodule = unsafe { GetModuleHandleW(PCWSTR::null())? };
    let hinstance = HINSTANCE(hmodule.0);
    let class_name = w!("NyxClientClickGuiWindow");

    let window_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: class_name,
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&window_class);
    }

    let (window_width, window_height) = animation_canvas_size();
    let (window_x, window_y) = centered_window_position(window_width, window_height);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class_name,
            w!("NyxClient ClickGui"),
            WS_POPUP,
            window_x,
            window_y,
            window_width,
            window_height,
            None,
            None,
            Some(hinstance),
            Some(app_ptr.cast::<c_void>()),
        )?
    };

    let _leaked_to_window = Box::into_raw(app);

    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, TRANSPARENT_KEY, 255, LWA_COLORKEY);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    Ok(())
}

fn centered_window_position(width: i32, height: i32) -> (i32, i32) {
    let screen_width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = ((screen_width - width) / 2).max(0);
    let y = ((screen_height - height) / 2).max(0);

    (x, y)
}

fn animation_canvas_size() -> (i32, i32) {
    (
        (WINDOW_WIDTH as f32 * CLICK_GUI_SCALE * EXIT_OVERSHOOT_SCALE).ceil() as i32,
        (WINDOW_HEIGHT as f32 * CLICK_GUI_SCALE * EXIT_OVERSHOOT_SCALE).ceil() as i32,
    )
}

struct ComScope;

impl Drop for ComScope {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct GuiApp {
    hwnd: HWND,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    wic_factory: IWICImagingFactory,
    render_target: Option<ID2D1HwndRenderTarget>,
    icon_bitmap: Option<ID2D1Bitmap>,
    icon_path: Option<PathBuf>,
    username: String,
    is_admin: bool,
    selected_category: Category,
    close_key_was_down: bool,
    animation: GuiAnimation,
    cards: Vec<GuiCard>,
}

impl GuiApp {
    fn new() -> windows::core::Result<Self> {
        let d2d_factory =
            unsafe { D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite_factory =
            unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };
        let wic_factory = unsafe {
            CoCreateInstance::<_, IWICImagingFactory>(
                &CLSID_WICImagingFactory,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )?
        };

        let icon_path = download_icon_to_cache();

        Ok(Self {
            hwnd: HWND(null_mut()),
            d2d_factory,
            dwrite_factory,
            wic_factory,
            render_target: None,
            icon_bitmap: None,
            icon_path,
            username: windows_username(),
            is_admin: unsafe { IsUserAnAdmin().0 != 0 },
            selected_category: Category::System,
            close_key_was_down: is_escape_key_down(),
            animation: GuiAnimation::entering(),
            cards: seed_cards(),
        })
    }

    fn render(&mut self) {
        if self.ensure_render_target().is_err() {
            return;
        }
        let Some(target) = self.render_target.clone() else {
            return;
        };
        let _ = self.ensure_icon_bitmap();

        unsafe {
            target.BeginDraw();
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.Clear(Some(&transparent_key_color()));
        }

        let transform = self.render_transform();
        unsafe {
            target.SetTransform(&transform);
        }
        self.draw_shell(&target, WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32);

        unsafe {
            let identity = Matrix3x2::identity();
            target.SetTransform(&identity);
            let _ = target.EndDraw(None, None);
        }
    }

    fn ensure_render_target(&mut self) -> windows::core::Result<()> {
        if self.render_target.is_some() {
            return Ok(());
        }

        let size = self.client_size();
        let render_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: size,
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };

        self.render_target = Some(unsafe {
            self.d2d_factory
                .CreateHwndRenderTarget(&render_props, &hwnd_props)?
        });
        Ok(())
    }

    fn ensure_icon_bitmap(&mut self) -> windows::core::Result<()> {
        if self.icon_bitmap.is_some() {
            return Ok(());
        }

        let Some(path) = self.icon_path.clone() else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let Some(target) = &self.render_target else {
            return Ok(());
        };

        let path_wide = path_to_wide_null(&path);
        let decoder = unsafe {
            self.wic_factory.CreateDecoderFromFilename(
                PCWSTR(path_wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnLoad,
            )?
        };
        let frame = unsafe { decoder.GetFrame(0)? };
        let converter = unsafe { self.wic_factory.CreateFormatConverter()? };
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            self.icon_bitmap = Some(target.CreateBitmapFromWicBitmap(&converter, None)?);
        }

        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Some(target) = &self.render_target {
            let size = D2D_SIZE_U { width, height };
            let _ = unsafe { target.Resize(&size) };
        }
    }

    fn handle_click(&mut self, x: f32, y: f32) {
        let width = WINDOW_WIDTH as f32;
        let switch_left = width - 86.0;

        for (index, category) in Category::ALL.iter().copied().enumerate() {
            let top = CATEGORY_TOP + index as f32 * CATEGORY_STEP;
            if hit(
                x,
                y,
                SIDEBAR_LEFT,
                top,
                SIDEBAR_RIGHT - SIDEBAR_LEFT,
                CATEGORY_HEIGHT,
            ) {
                self.selected_category = category;
                return;
            }
        }

        let mut row = 0;
        for card in self
            .cards
            .iter_mut()
            .filter(|card| card.category == self.selected_category)
        {
            let top = CARD_TOP + row as f32 * CARD_STEP;
            let switch_top = top + (CARD_HEIGHT - SWITCH_HEIGHT) / 2.0;
            if hit(x, y, switch_left, switch_top, SWITCH_WIDTH, SWITCH_HEIGHT) {
                card.enabled = !card.enabled;
                return;
            }
            row += 1;
        }
    }

    fn should_close_for_escape(&mut self) -> bool {
        let is_down = is_escape_key_down();
        let should_close = is_down && !self.close_key_was_down;
        self.close_key_was_down = is_down;

        should_close
    }

    fn start_entry_animation(&mut self) {
        self.animation.start_entry();
        unsafe {
            let _ = SetTimer(
                Some(self.hwnd),
                ANIMATION_TIMER_ID,
                ANIMATION_FRAME_MS,
                None,
            );
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn request_close(&mut self) {
        if self.animation.is_exiting() {
            return;
        }

        self.animation.start_exit();
        unsafe {
            let _ = SetTimer(
                Some(self.hwnd),
                ANIMATION_TIMER_ID,
                ANIMATION_FRAME_MS,
                None,
            );
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn tick_animation(&mut self) -> bool {
        if !self.animation.is_active() {
            return false;
        }

        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }

        if !self.animation.is_finished() {
            return false;
        }

        match self.animation.phase() {
            GuiAnimationPhase::Entering => {
                self.animation.finish_entry();
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), ANIMATION_TIMER_ID);
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
                false
            }
            GuiAnimationPhase::Exiting => true,
            GuiAnimationPhase::Idle => false,
        }
    }

    fn is_exiting(&self) -> bool {
        self.animation.is_exiting()
    }

    fn render_transform(&self) -> Matrix3x2 {
        let (scale_x, scale_y, offset_x, offset_y) = self.render_scale_and_offset();
        Matrix3x2 {
            M11: scale_x,
            M12: 0.0,
            M21: 0.0,
            M22: scale_y,
            M31: offset_x,
            M32: offset_y,
        }
    }

    fn to_logical_point(&self, x: f32, y: f32) -> (f32, f32) {
        let (scale_x, scale_y, offset_x, offset_y) = self.render_scale_and_offset();

        ((x - offset_x) / scale_x, (y - offset_y) / scale_y)
    }

    fn render_scale_and_offset(&self) -> (f32, f32, f32, f32) {
        let size = self.client_size();
        let animation_scale = self.animation.scale();
        let scale_x = animation_scale * CLICK_GUI_SCALE;
        let scale_y = animation_scale * CLICK_GUI_SCALE;
        let offset_x = (size.width as f32 - WINDOW_WIDTH as f32 * scale_x) * 0.5;
        let offset_y = (size.height as f32 - WINDOW_HEIGHT as f32 * scale_y) * 0.5;

        (scale_x.max(0.0001), scale_y.max(0.0001), offset_x, offset_y)
    }

    fn client_size(&self) -> D2D_SIZE_U {
        let mut rect = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut rect) }.is_ok() {
            D2D_SIZE_U {
                width: (rect.right - rect.left).max(1) as u32,
                height: (rect.bottom - rect.top).max(1) as u32,
            }
        } else {
            let (width, height) = animation_canvas_size();
            D2D_SIZE_U {
                width: width as u32,
                height: height as u32,
            }
        }
    }

    fn draw_shell(&self, target: &ID2D1HwndRenderTarget, width: f32, height: f32) {
        self.fill_round(
            target,
            rect(
                WINDOW_MARGIN,
                WINDOW_MARGIN,
                width - WINDOW_MARGIN,
                height - WINDOW_MARGIN,
            ),
            16.0,
            rgba(16, 18, 23, 255),
        );
        self.stroke_round(
            target,
            rect(
                WINDOW_MARGIN,
                WINDOW_MARGIN,
                width - WINDOW_MARGIN,
                height - WINDOW_MARGIN,
            ),
            16.0,
            rgba(48, 55, 68, 255),
            1.0,
        );
        self.fill_round(
            target,
            rect(
                PANEL_MARGIN,
                PANEL_MARGIN,
                width - PANEL_MARGIN,
                height - PANEL_MARGIN,
            ),
            13.0,
            rgba(19, 22, 29, 255),
        );

        self.draw_header(target, width);
        self.draw_categories(target, height);
        self.draw_content(target, width);
    }

    fn draw_header(&self, target: &ID2D1HwndRenderTarget, width: f32) {
        self.fill_round(
            target,
            rect(34.0, 34.0, 70.0, 70.0),
            9.0,
            rgba(36, 43, 55, 255),
        );
        if let Some(bitmap) = &self.icon_bitmap {
            unsafe {
                target.DrawBitmap(
                    bitmap,
                    Some(&rect(38.0, 38.0, 66.0, 66.0)),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        } else {
            self.fill_round(
                target,
                rect(38.0, 38.0, 66.0, 66.0),
                8.0,
                rgba(224, 118, 50, 255),
            );
            self.text(
                target,
                "N",
                38.0,
                38.0,
                66.0,
                64.0,
                16.0,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_TEXT_ALIGNMENT_CENTER,
                rgba(255, 255, 255, 255),
            );
        }
        self.text(
            target,
            "NyxClient",
            82.0,
            35.0,
            210.0,
            55.0,
            18.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            rgba(240, 244, 248, 255),
        );
        self.text(
            target,
            "ClickGui",
            82.0,
            56.0,
            210.0,
            72.0,
            11.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            rgba(133, 146, 166, 255),
        );

        self.draw_profile_badge(target, width);
        self.fill_round(
            target,
            rect(36.0, 88.0, width - 36.0, 89.0),
            0.5,
            rgba(42, 49, 61, 255),
        );
    }

    fn draw_profile_badge(&self, target: &ID2D1HwndRenderTarget, width: f32) {
        let left = (width - 220.0).max(234.0);
        let right = width - 34.0;
        self.fill_round(
            target,
            rect(left, 34.0, right, 70.0),
            12.0,
            rgba(28, 33, 42, 255),
        );

        let avatar = D2D1_ELLIPSE {
            point: Vector2 {
                X: left + 24.0,
                Y: 52.0,
            },
            radiusX: 14.0,
            radiusY: 14.0,
        };
        if let Ok(brush) = self.brush(target, rgba(65, 122, 159, 255)) {
            unsafe {
                target.FillEllipse(&avatar, &brush);
            }
        }

        let initial = self
            .username
            .chars()
            .next()
            .map(|ch| ch.to_uppercase().to_string())
            .unwrap_or_else(|| "U".to_owned());
        self.text(
            target,
            &initial,
            left + 10.0,
            38.0,
            left + 38.0,
            64.0,
            14.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            rgba(255, 255, 255, 255),
        );
        self.text(
            target,
            &self.username,
            left + 48.0,
            37.0,
            right - 48.0,
            57.0,
            12.0,
            DWRITE_FONT_WEIGHT_DEMI_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            rgba(236, 241, 247, 255),
        );

        let label = if self.is_admin { "Dev" } else { "User" };
        let label_bg = if self.is_admin {
            rgba(194, 80, 69, 255)
        } else {
            rgba(54, 95, 132, 255)
        };
        self.fill_round(
            target,
            rect(right - 42.0, 45.0, right - 12.0, 61.0),
            7.0,
            label_bg,
        );
        self.text(
            target,
            label,
            right - 42.0,
            45.0,
            right - 12.0,
            59.0,
            9.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            rgba(255, 255, 255, 255),
        );
    }

    fn draw_categories(&self, target: &ID2D1HwndRenderTarget, height: f32) {
        self.fill_round(
            target,
            rect(22.0, 100.0, SIDEBAR_RIGHT, height - 22.0),
            12.0,
            rgba(22, 25, 31, 255),
        );

        for (index, category) in Category::ALL.iter().copied().enumerate() {
            let top = CATEGORY_TOP + index as f32 * CATEGORY_STEP;
            let selected = category == self.selected_category;
            let bg = if selected {
                rgba(54, 95, 132, 255)
            } else {
                rgba(27, 31, 39, 255)
            };
            let fg = if selected {
                rgba(248, 251, 255, 255)
            } else {
                rgba(153, 166, 186, 255)
            };
            self.fill_round(
                target,
                rect(
                    SIDEBAR_LEFT,
                    top,
                    SIDEBAR_RIGHT - 12.0,
                    top + CATEGORY_HEIGHT,
                ),
                8.0,
                bg,
            );
            self.fill_round(
                target,
                rect(
                    SIDEBAR_LEFT + 10.0,
                    top + 12.0,
                    SIDEBAR_LEFT + 18.0,
                    top + 20.0,
                ),
                4.0,
                fg,
            );
            self.text(
                target,
                category.display_name(),
                SIDEBAR_LEFT + 26.0,
                top + 7.0,
                SIDEBAR_RIGHT - 22.0,
                top + 28.0,
                13.0,
                DWRITE_FONT_WEIGHT_DEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                fg,
            );
        }
    }

    fn draw_content(&self, target: &ID2D1HwndRenderTarget, width: f32) {
        self.text(
            target,
            self.selected_category.display_name(),
            CONTENT_LEFT,
            102.0,
            width - 38.0,
            126.0,
            18.0,
            DWRITE_FONT_WEIGHT_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            rgba(245, 248, 252, 255),
        );

        let mut visible_cards = 0;
        for (row, card) in self
            .cards
            .iter()
            .filter(|card| card.category == self.selected_category)
            .enumerate()
        {
            visible_cards += 1;
            let top = CARD_TOP + row as f32 * CARD_STEP;
            let switch_left = width - 86.0;
            let switch_top = top + (CARD_HEIGHT - SWITCH_HEIGHT) / 2.0;
            self.fill_round(
                target,
                rect(CONTENT_LEFT, top, width - 38.0, top + CARD_HEIGHT),
                10.0,
                rgba(28, 33, 42, 255),
            );
            self.stroke_round(
                target,
                rect(CONTENT_LEFT, top, width - 38.0, top + CARD_HEIGHT),
                10.0,
                rgba(42, 49, 61, 255),
                1.0,
            );
            self.text(
                target,
                card.name,
                CONTENT_LEFT + 18.0,
                top + 10.0,
                switch_left - 18.0,
                top + 30.0,
                14.0,
                DWRITE_FONT_WEIGHT_DEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                rgba(237, 242, 248, 255),
            );
            self.text(
                target,
                card.description,
                CONTENT_LEFT + 18.0,
                top + 33.0,
                switch_left - 18.0,
                top + 52.0,
                11.0,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                rgba(133, 148, 168, 255),
            );
            self.text(
                target,
                card.category.display_name(),
                width - 170.0,
                top + 10.0,
                switch_left - 18.0,
                top + 29.0,
                10.0,
                DWRITE_FONT_WEIGHT_DEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                rgba(122, 137, 157, 255),
            );
            self.draw_switch(target, switch_left, switch_top, card.enabled);
        }

        if visible_cards == 0 {
            self.fill_round(
                target,
                rect(CONTENT_LEFT, CARD_TOP, width - 38.0, CARD_TOP + CARD_HEIGHT),
                10.0,
                rgba(28, 33, 42, 255),
            );
            self.stroke_round(
                target,
                rect(CONTENT_LEFT, CARD_TOP, width - 38.0, CARD_TOP + CARD_HEIGHT),
                10.0,
                rgba(42, 49, 61, 255),
                1.0,
            );
            self.text(
                target,
                "No modules",
                CONTENT_LEFT + 18.0,
                CARD_TOP + 17.0,
                width - 56.0,
                CARD_TOP + 43.0,
                14.0,
                DWRITE_FONT_WEIGHT_DEMI_BOLD,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                rgba(133, 148, 168, 255),
            );
        }
    }

    fn draw_switch(&self, target: &ID2D1HwndRenderTarget, left: f32, top: f32, enabled: bool) {
        let bg = if enabled {
            rgba(70, 138, 105, 255)
        } else {
            rgba(58, 65, 77, 255)
        };
        self.fill_round(
            target,
            rect(left, top, left + SWITCH_WIDTH, top + SWITCH_HEIGHT),
            SWITCH_HEIGHT / 2.0,
            bg,
        );
        let knob_x = if enabled { left + 31.0 } else { left + 13.0 };
        let ellipse = D2D1_ELLIPSE {
            point: Vector2 {
                X: knob_x,
                Y: top + SWITCH_HEIGHT / 2.0,
            },
            radiusX: 8.0,
            radiusY: 8.0,
        };
        if let Ok(brush) = self.brush(target, rgba(250, 252, 255, 255)) {
            unsafe {
                target.FillEllipse(&ellipse, &brush);
            }
        }
    }

    fn text(
        &self,
        target: &ID2D1HwndRenderTarget,
        value: &str,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        align: DWRITE_TEXT_ALIGNMENT,
        text_color: D2D1_COLOR_F,
    ) {
        let Some(format) = self.text_format(size, weight, align) else {
            return;
        };
        let Ok(brush) = self.brush(target, text_color) else {
            return;
        };
        let wide: Vec<u16> = value.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }

        unsafe {
            target.DrawText(
                &wide,
                &format,
                &rect(left, top, right, bottom),
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    fn text_format(
        &self,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> Option<IDWriteTextFormat> {
        let format = unsafe {
            self.dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None::<&IDWriteFontCollection>,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
                .ok()?
        };
        unsafe {
            let _ = format.SetTextAlignment(align);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        }
        Some(format)
    }

    fn fill_round(
        &self,
        target: &ID2D1HwndRenderTarget,
        area: D2D_RECT_F,
        radius: f32,
        fill: D2D1_COLOR_F,
    ) {
        if let Ok(brush) = self.brush(target, fill) {
            let rounded = D2D1_ROUNDED_RECT {
                rect: area,
                radiusX: radius,
                radiusY: radius,
            };
            unsafe {
                target.FillRoundedRectangle(&rounded, &brush);
            }
        }
    }

    fn stroke_round(
        &self,
        target: &ID2D1HwndRenderTarget,
        area: D2D_RECT_F,
        radius: f32,
        stroke: D2D1_COLOR_F,
        width: f32,
    ) {
        if let Ok(brush) = self.brush(target, stroke) {
            let rounded = D2D1_ROUNDED_RECT {
                rect: area,
                radiusX: radius,
                radiusY: radius,
            };
            unsafe {
                target.DrawRoundedRectangle(&rounded, &brush, width, None::<&ID2D1StrokeStyle>);
            }
        }
    }

    fn brush(
        &self,
        target: &ID2D1HwndRenderTarget,
        brush_color: D2D1_COLOR_F,
    ) -> windows::core::Result<ID2D1SolidColorBrush> {
        unsafe { target.CreateSolidColorBrush(&brush_color, None) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiAnimationPhase {
    Entering,
    Idle,
    Exiting,
}

#[derive(Debug)]
struct GuiAnimation {
    phase: GuiAnimationPhase,
    started_at: Instant,
}

impl GuiAnimation {
    fn entering() -> Self {
        Self {
            phase: GuiAnimationPhase::Entering,
            started_at: Instant::now(),
        }
    }

    fn start_entry(&mut self) {
        self.phase = GuiAnimationPhase::Entering;
        self.started_at = Instant::now();
    }

    fn start_exit(&mut self) {
        self.phase = GuiAnimationPhase::Exiting;
        self.started_at = Instant::now();
    }

    fn finish_entry(&mut self) {
        self.phase = GuiAnimationPhase::Idle;
    }

    fn phase(&self) -> GuiAnimationPhase {
        self.phase
    }

    fn is_active(&self) -> bool {
        self.phase != GuiAnimationPhase::Idle
    }

    fn is_exiting(&self) -> bool {
        self.phase == GuiAnimationPhase::Exiting
    }

    fn is_finished(&self) -> bool {
        match self.phase {
            GuiAnimationPhase::Entering => self.progress(ENTRY_ANIMATION_MS) >= 1.0,
            GuiAnimationPhase::Exiting => self.progress(EXIT_ANIMATION_MS) >= 1.0,
            GuiAnimationPhase::Idle => false,
        }
    }

    fn scale(&self) -> f32 {
        match self.phase {
            GuiAnimationPhase::Entering => {
                let progress = ease_out_quad(self.progress(ENTRY_ANIMATION_MS));
                lerp(ENTRY_START_SCALE, 1.0, progress)
            }
            GuiAnimationPhase::Exiting => {
                let progress = self.progress(EXIT_ANIMATION_MS);
                if progress < EXIT_OVERSHOOT_PORTION {
                    lerp(
                        1.0,
                        EXIT_OVERSHOOT_SCALE,
                        ease_out_quad(progress / EXIT_OVERSHOOT_PORTION),
                    )
                } else {
                    let shrink_progress =
                        (progress - EXIT_OVERSHOOT_PORTION) / (1.0 - EXIT_OVERSHOOT_PORTION);
                    lerp(
                        EXIT_OVERSHOOT_SCALE,
                        EXIT_END_SCALE,
                        ease_in_out_cubic(shrink_progress),
                    )
                }
            }
            GuiAnimationPhase::Idle => 1.0,
        }
    }

    fn progress(&self, duration_ms: f32) -> f32 {
        (self.started_at.elapsed().as_secs_f32() * 1000.0 / duration_ms).clamp(0.0, 1.0)
    }
}

struct GuiCard {
    category: Category,
    name: &'static str,
    description: &'static str,
    enabled: bool,
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let app_ptr = unsafe { (*create).lpCreateParams as *mut GuiApp };
                if !app_ptr.is_null() {
                    unsafe {
                        (*app_ptr).hwnd = hwnd;
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);
                        (*app_ptr).start_entry_animation();
                    }
                    OPEN_HWND.store(hwnd.0 as isize, Ordering::Release);
                    unsafe {
                        let _ = SetTimer(Some(hwnd), ESC_CLOSE_TIMER_ID, ESC_CLOSE_POLL_MS, None);
                    }
                    return LRESULT(1);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut paint);
            }
            if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                app.render();
            }
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                let width = (lparam.0 as u32 & 0xffff).max(1);
                let height = ((lparam.0 as u32 >> 16) & 0xffff).max(1);
                app.resize(width, height);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                if app.is_exiting() {
                    return LRESULT(0);
                }

                let raw_x = (lparam.0 as u32 & 0xffff) as i16 as f32;
                let raw_y = ((lparam.0 as u32 >> 16) & 0xffff) as i16 as f32;
                let (x, y) = app.to_logical_point(raw_x, raw_y);
                let width = WINDOW_WIDTH as f32;
                if y <= HEADER_DRAG_HEIGHT && x >= WINDOW_MARGIN && x <= width - WINDOW_MARGIN {
                    unsafe {
                        let _ = ReleaseCapture();
                        SendMessageW(
                            hwnd,
                            WM_NCLBUTTONDOWN,
                            Some(WPARAM(HTCAPTION as usize)),
                            Some(LPARAM(0)),
                        );
                    }
                    return LRESULT(0);
                }
                app.handle_click(x, y);
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == ESC_CLOSE_TIMER_ID {
                if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                    if app.should_close_for_escape() {
                        app.request_close();
                    }
                }
                return LRESULT(0);
            }
            if wparam.0 == ANIMATION_TIMER_ID {
                if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                    if app.tick_animation() {
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CLOSE => {
            if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                app.request_close();
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
            let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiApp };
            unsafe {
                let _ = KillTimer(Some(hwnd), ESC_CLOSE_TIMER_ID);
                let _ = KillTimer(Some(hwnd), ANIMATION_TIMER_ID);
            }
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

unsafe fn app_from_hwnd(hwnd: HWND) -> Option<&'static mut GuiApp> {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiApp };
    if raw.is_null() {
        None
    } else {
        Some(unsafe { &mut *raw })
    }
}

fn seed_cards() -> Vec<GuiCard> {
    vec![
        GuiCard {
            category: Category::Other,
            name: "Fun",
            description: "Base module for other features.",
            enabled: true,
        },
        GuiCard {
            category: Category::System,
            name: "ClickGui",
            description: "Rust Direct2D click interface.",
            enabled: true,
        },
    ]
}

fn download_icon_to_cache() -> Option<PathBuf> {
    let mut path = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    path.push("NyxClient");
    let _ = std::fs::create_dir_all(&path);
    path.push("clickgui-icon.png");

    if path.exists() {
        return Some(path);
    }

    let download_path = path.clone();
    let _ = thread::Builder::new()
        .name("nyx-click-gui-icon".to_owned())
        .spawn(move || {
            let coinit = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if coinit.is_err() {
                return;
            }
            let _com_scope = ComScope;

            let url = wide_null(ICON_URL);
            let dest = path_to_wide_null(&download_path);
            let _ = unsafe {
                URLDownloadToFileW(
                    None::<&IUnknown>,
                    PCWSTR(url.as_ptr()),
                    PCWSTR(dest.as_ptr()),
                    0,
                    None::<&IBindStatusCallback>,
                )
            };
        });

    Some(path)
}

fn windows_username() -> String {
    let mut buffer = [0u16; 257];
    let mut size = buffer.len() as u32;
    if unsafe { GetUserNameW(Some(PWSTR(buffer.as_mut_ptr())), &mut size) }.is_ok() && size > 1 {
        String::from_utf16_lossy(&buffer[..size as usize - 1])
    } else {
        std::env::var("USERNAME").unwrap_or_else(|_| "Windows User".to_owned())
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_to_wide_null(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn is_escape_key_down() -> bool {
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) & KEY_STATE_DOWN_MASK != 0 }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> D2D1_COLOR_F {
    color(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    )
}

fn transparent_key_color() -> D2D1_COLOR_F {
    rgba(1, 2, 3, 255)
}

fn lerp(start: f32, end: f32, progress: f32) -> f32 {
    start + (end - start) * progress.clamp(0.0, 1.0)
}

fn ease_out_quad(progress: f32) -> f32 {
    let inverse = 1.0 - progress.clamp(0.0, 1.0);
    1.0 - inverse * inverse
}

fn ease_in_out_cubic(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress * progress * progress
    } else {
        1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
    }
}

fn hit(x: f32, y: f32, left: f32, top: f32, width: f32, height: f32) -> bool {
    x >= left && x <= left + width && y >= top && y <= top + height
}

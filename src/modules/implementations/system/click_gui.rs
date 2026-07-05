use std::{
    ffi::c_void,
    mem::size_of,
    path::PathBuf,
    ptr::null_mut,
    sync::atomic::{AtomicIsize, Ordering},
    thread,
    time::Instant,
};

use crate::{
    client_icon,
    modules::{Category, Module, ModuleInfo, ModuleState},
};
use skija::{
    AlphaType, Canvas, Color, Color4f, Data, Font, FontMgr, FontStyle, Image, ImageInfo, Paint,
    PaintStyle, Point, RRect, Rect as SkRect, TileMode, Typeface, font_style, gradient, surfaces,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, DIB_RGB_COLORS, EndPaint, HDC,
            InvalidateRect, PAINTSTRUCT, SetDIBitsToDevice,
        },
        System::{LibraryLoader::GetModuleHandleW, WindowsProgramming::GetUserNameW},
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
    core::{PCWSTR, PWSTR, w},
};

const WINDOW_WIDTH: i32 = 960;
const WINDOW_HEIGHT: i32 = 660;
const CLICK_GUI_SCALE: f32 = 1.0;
const PANEL_LEFT: f32 = 20.0;
const PANEL_TOP: f32 = 20.0;
const PANEL_WIDTH: f32 = 920.0;
const PANEL_HEIGHT: f32 = 620.0;
const PANEL_RIGHT: f32 = PANEL_LEFT + PANEL_WIDTH;
const PANEL_BOTTOM: f32 = PANEL_TOP + PANEL_HEIGHT;
const SIDEBAR_WIDTH: f32 = 220.0;
const SIDEBAR_RIGHT: f32 = PANEL_LEFT + SIDEBAR_WIDTH;
const HEADER_HEIGHT: f32 = 64.0;
const HEADER_DRAG_HEIGHT: f32 = PANEL_TOP + HEADER_HEIGHT;
const LOGO_SIZE: f32 = 32.0;
const NAV_TOP: f32 = PANEL_TOP + 94.0;
const NAV_HEADER_HEIGHT: f32 = 22.0;
const NAV_ITEM_HEIGHT: f32 = 38.0;
const NAV_ITEM_STEP: f32 = 40.0;
const CONTENT_LEFT: f32 = SIDEBAR_RIGHT;
const CONTENT_PADDING: f32 = 16.0;
const CONTENT_TOP: f32 = PANEL_TOP + HEADER_HEIGHT + 26.0;
const COLUMN_GAP: f32 = 20.0;
const COLUMN_WIDTH: f32 = (PANEL_RIGHT - CONTENT_LEFT - CONTENT_PADDING * 2.0 - COLUMN_GAP) / 2.0;
const MODULE_ROW_HEIGHT: f32 = 46.0;
const SETTING_ROW_HEIGHT: f32 = 38.0;
const SLIDER_ROW_HEIGHT: f32 = 52.0;
const SWITCH_WIDTH: f32 = 32.0;
const SWITCH_HEIGHT: f32 = 16.0;
const ESC_CLOSE_TIMER_ID: usize = 1;
const ESC_CLOSE_POLL_MS: u32 = 30;
const ANIMATION_TIMER_ID: usize = 2;
const ANIMATION_FRAME_MS: u32 = 8;
const ENTRY_ANIMATION_MS: f32 = 360.0;
const EXIT_ANIMATION_MS: f32 = 520.0;
const ENTRY_START_SCALE: f32 = 0.84;
const EXIT_OVERSHOOT_SCALE: f32 = 1.03;
const EXIT_END_SCALE: f32 = 0.08;
const EXIT_OVERSHOOT_PORTION: f32 = 0.35;
const TRANSPARENT_KEY: COLORREF = COLORREF(0x0003_0201);
const KEY_STATE_DOWN_MASK: i16 = 0x8000_u16 as i16;
const STARTING_HWND: isize = -1;

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
            info: ModuleInfo::new("ClickGui", "Skia powered click GUI.", Category::System),
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
    let mut app = Box::new(GuiApp::new());
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

struct GuiApp {
    hwnd: HWND,
    icon_image: Option<Image>,
    icon_path: Option<PathBuf>,
    username: String,
    is_admin: bool,
    selected_category: Category,
    close_key_was_down: bool,
    animation: GuiAnimation,
    cards: Vec<GuiCard>,
    regular_typeface: Option<Typeface>,
    medium_typeface: Option<Typeface>,
    semibold_typeface: Option<Typeface>,
    bold_typeface: Option<Typeface>,
    black_typeface: Option<Typeface>,
}

impl GuiApp {
    fn new() -> Self {
        let font_mgr = FontMgr::new();
        let regular_typeface = match_typeface(&font_mgr, font_style::Weight::NORMAL);
        let medium_typeface = match_typeface(&font_mgr, font_style::Weight::MEDIUM);
        let semibold_typeface = match_typeface(&font_mgr, font_style::Weight::SEMI_BOLD);
        let bold_typeface = match_typeface(&font_mgr, font_style::Weight::BOLD);
        let black_typeface = match_typeface(&font_mgr, font_style::Weight::BLACK);
        let icon_path = client_icon::cached_png_path();
        let icon_image = icon_path.as_ref().and_then(|path| load_skia_image(path));

        Self {
            hwnd: HWND(null_mut()),
            icon_image,
            icon_path,
            username: windows_username(),
            is_admin: unsafe { IsUserAnAdmin().0 != 0 },
            selected_category: Category::System,
            close_key_was_down: is_escape_key_down(),
            animation: GuiAnimation::entering(),
            cards: seed_cards(),
            regular_typeface,
            medium_typeface,
            semibold_typeface,
            bold_typeface,
            black_typeface,
        }
    }

    fn render(&mut self, hdc: HDC) {
        self.ensure_icon_image();

        let (client_width, client_height) = self.client_size();
        let width = client_width.max(1) as i32;
        let height = client_height.max(1) as i32;
        let row_bytes = width as usize * 4;
        let mut pixels = vec![0_u8; row_bytes * height as usize];
        let image_info = ImageInfo::new_n32((width, height), AlphaType::Premul, None);

        {
            let Some(mut surface) =
                surfaces::wrap_pixels(&image_info, &mut pixels, Some(row_bytes), None)
            else {
                return;
            };
            let canvas = surface.canvas();

            canvas.clear(transparent_key_color());

            let (scale_x, scale_y, offset_x, offset_y) =
                self.render_scale_and_offset((client_width, client_height));
            canvas.save();
            canvas.translate((offset_x, offset_y));
            canvas.scale((scale_x, scale_y));
            self.draw_shell(canvas);
            canvas.restore();
        }

        blit_pixels(hdc, &pixels, width, height);
    }

    fn ensure_icon_image(&mut self) {
        if self.icon_image.is_some() {
            return;
        }

        self.icon_image = self
            .icon_path
            .as_ref()
            .and_then(|path| load_skia_image(path));
    }

    fn resize(&mut self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn handle_click(&mut self, x: f32, y: f32) {
        for (index, category) in Category::ALL.iter().copied().enumerate() {
            let top = nav_item_top(index);
            if hit(x, y, PANEL_LEFT, top, SIDEBAR_WIDTH - 1.0, NAV_ITEM_HEIGHT) {
                self.selected_category = category;
                return;
            }
        }

        let (module_left, module_top, module_width) = module_group_layout();
        let switch_left = module_left + module_width - 52.0;
        let mut row = 0;
        for card in self
            .cards
            .iter_mut()
            .filter(|card| card.category == self.selected_category)
        {
            let row_top = module_top + 4.0 + row as f32 * MODULE_ROW_HEIGHT;
            let switch_top = row_top + (MODULE_ROW_HEIGHT - SWITCH_HEIGHT) * 0.5;
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

    fn to_logical_point(&self, x: f32, y: f32) -> (f32, f32) {
        let (scale_x, scale_y, offset_x, offset_y) =
            self.render_scale_and_offset(self.client_size());

        ((x - offset_x) / scale_x, (y - offset_y) / scale_y)
    }

    fn render_scale_and_offset(&self, (width, height): (u32, u32)) -> (f32, f32, f32, f32) {
        let animation_scale = self.animation.scale();
        let scale_x = animation_scale * CLICK_GUI_SCALE;
        let scale_y = animation_scale * CLICK_GUI_SCALE;
        let offset_x = (width as f32 - WINDOW_WIDTH as f32 * scale_x) * 0.5;
        let offset_y = (height as f32 - WINDOW_HEIGHT as f32 * scale_y) * 0.5;

        (scale_x.max(0.0001), scale_y.max(0.0001), offset_x, offset_y)
    }

    fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut rect) }.is_ok() {
            (
                (rect.right - rect.left).max(1) as u32,
                (rect.bottom - rect.top).max(1) as u32,
            )
        } else {
            let (width, height) = animation_canvas_size();
            (width as u32, height as u32)
        }
    }

    fn draw_shell(&self, canvas: &Canvas) {
        let panel_rect = sk_rect(PANEL_LEFT, PANEL_TOP, PANEL_RIGHT, PANEL_BOTTOM);
        let panel_rrect = RRect::new_rect_xy(panel_rect, 12.0, 12.0);
        fill_rrect_with_antialias(canvas, &panel_rrect, rgba(12, 13, 17, 255), false);

        canvas.save();
        canvas.clip_rrect(&panel_rrect, None, true);
        self.draw_sidebar(canvas);
        self.draw_header(canvas);
        self.draw_content(canvas);
        canvas.restore();

        stroke_rrect_with_antialias(canvas, &panel_rrect, rgba(255, 255, 255, 26), 1.0, false);
    }

    fn draw_sidebar(&self, canvas: &Canvas) {
        fill_rect(
            canvas,
            PANEL_LEFT,
            PANEL_TOP,
            SIDEBAR_WIDTH,
            PANEL_HEIGHT,
            rgba(0, 0, 0, 51),
        );
        fill_rect(
            canvas,
            SIDEBAR_RIGHT - 1.0,
            PANEL_TOP + 12.0,
            1.0,
            PANEL_HEIGHT - 24.0,
            rgba(255, 255, 255, 10),
        );

        self.draw_logo_area(canvas);
        self.draw_navigation(canvas);
        self.draw_user_area(canvas);
    }

    fn draw_logo_area(&self, canvas: &Canvas) {
        let logo_x = PANEL_LEFT + 24.0;
        let logo_y = PANEL_TOP + 16.0;
        fill_gradient_round(
            canvas,
            logo_x,
            logo_y,
            LOGO_SIZE,
            LOGO_SIZE,
            7.0,
            rgba(68, 137, 255, 255),
            rgba(26, 77, 163, 255),
        );

        if let Some(image) = &self.icon_image {
            canvas.save();
            canvas.clip_rrect(
                &RRect::new_rect_xy(
                    sk_rect(logo_x + 4.0, logo_y + 4.0, logo_x + 28.0, logo_y + 28.0),
                    5.0,
                    5.0,
                ),
                None,
                true,
            );
            let mut image_paint = Paint::default();
            image_paint.set_anti_alias(true);
            canvas.draw_image_rect(
                image,
                None,
                sk_rect(logo_x + 4.0, logo_y + 4.0, logo_x + 28.0, logo_y + 28.0),
                &image_paint,
            );
            canvas.restore();
        } else {
            self.text(
                canvas,
                "NX",
                logo_x,
                logo_y,
                LOGO_SIZE,
                LOGO_SIZE,
                14.0,
                TextWeight::Black,
                rgba(255, 255, 255, 255),
                TextAlign::Center,
            );
        }

        self.text(
            canvas,
            "NyxClient",
            logo_x + 44.0,
            logo_y + 2.0,
            126.0,
            18.0,
            16.0,
            TextWeight::Black,
            rgba(255, 255, 255, 255),
            TextAlign::Left,
        );
        self.text(
            canvas,
            "Skija ClickGUI",
            logo_x + 44.0,
            logo_y + 20.0,
            126.0,
            14.0,
            9.0,
            TextWeight::Normal,
            rgba(75, 82, 99, 255),
            TextAlign::Left,
        );

        fill_rect(
            canvas,
            PANEL_LEFT + 16.0,
            PANEL_TOP + HEADER_HEIGHT - 1.0,
            SIDEBAR_WIDTH - 32.0,
            1.0,
            rgba(255, 255, 255, 10),
        );
    }

    fn draw_navigation(&self, canvas: &Canvas) {
        self.text(
            canvas,
            "MODULES",
            PANEL_LEFT + 24.0,
            NAV_TOP,
            160.0,
            12.0,
            9.0,
            TextWeight::Black,
            rgba(75, 82, 99, 255),
            TextAlign::Left,
        );

        for (index, category) in Category::ALL.iter().copied().enumerate() {
            let top = nav_item_top(index);
            let selected = category == self.selected_category;

            if selected {
                fill_rect(
                    canvas,
                    PANEL_LEFT,
                    top,
                    SIDEBAR_WIDTH,
                    NAV_ITEM_HEIGHT,
                    rgba(61, 129, 247, 20),
                );
                fill_rect(canvas, PANEL_LEFT, top, 2.0, NAV_ITEM_HEIGHT, nl_accent());
            }

            let color = if selected {
                nl_accent()
            } else {
                rgba(108, 113, 126, 255)
            };
            self.draw_nav_icon(canvas, category, PANEL_LEFT + 24.0, top + 11.0, color);
            self.text(
                canvas,
                category.display_name(),
                PANEL_LEFT + 52.0,
                top + 4.0,
                140.0,
                28.0,
                12.0,
                TextWeight::SemiBold,
                color,
                TextAlign::Left,
            );
        }
    }

    fn draw_nav_icon(&self, canvas: &Canvas, category: Category, x: f32, y: f32, color: Color) {
        match category {
            Category::Combat => {
                stroke_circle(canvas, x + 8.0, y + 8.0, 6.0, color, 1.4);
                line(canvas, x + 8.0, y + 1.0, x + 8.0, y + 5.0, color, 1.2);
                line(canvas, x + 8.0, y + 11.0, x + 8.0, y + 15.0, color, 1.2);
                line(canvas, x + 1.0, y + 8.0, x + 5.0, y + 8.0, color, 1.2);
                line(canvas, x + 11.0, y + 8.0, x + 15.0, y + 8.0, color, 1.2);
            }
            Category::Other => {
                stroke_round(canvas, x + 2.0, y + 2.0, 12.0, 12.0, 3.0, color, 1.3);
                fill_rect(canvas, x + 5.0, y + 5.0, 6.0, 1.4, color);
                fill_rect(canvas, x + 5.0, y + 9.0, 6.0, 1.4, color);
            }
            Category::Player => {
                stroke_circle(canvas, x + 8.0, y + 5.0, 3.2, color, 1.3);
                stroke_round(canvas, x + 3.0, y + 10.0, 10.0, 5.0, 3.0, color, 1.3);
            }
            Category::System => {
                stroke_circle(canvas, x + 8.0, y + 8.0, 4.2, color, 1.3);
                for angle in [0.0_f32, 45.0, 90.0, 135.0] {
                    let radians = angle.to_radians();
                    let dx = radians.cos() * 7.0;
                    let dy = radians.sin() * 7.0;
                    line(
                        canvas,
                        x + 8.0 - dx,
                        y + 8.0 - dy,
                        x + 8.0 + dx,
                        y + 8.0 + dy,
                        color,
                        1.1,
                    );
                }
                fill_circle(canvas, x + 8.0, y + 8.0, 2.2, rgba(12, 13, 17, 255));
            }
            Category::Visual => {
                stroke_round(canvas, x + 2.0, y + 3.0, 12.0, 10.0, 2.5, color, 1.3);
                fill_circle(canvas, x + 6.0, y + 7.0, 1.4, color);
                line(canvas, x + 4.0, y + 12.0, x + 8.0, y + 9.0, color, 1.2);
                line(canvas, x + 8.0, y + 9.0, x + 13.0, y + 13.0, color, 1.2);
            }
        }
    }

    fn draw_user_area(&self, canvas: &Canvas) {
        let top = PANEL_BOTTOM - 70.0;
        fill_rect(
            canvas,
            PANEL_LEFT + 16.0,
            top,
            SIDEBAR_WIDTH - 32.0,
            1.0,
            rgba(255, 255, 255, 10),
        );

        let avatar_x = PANEL_LEFT + 40.0;
        let avatar_y = top + 35.0;
        fill_circle(canvas, avatar_x, avatar_y, 16.0, rgba(24, 26, 33, 255));
        stroke_circle(
            canvas,
            avatar_x,
            avatar_y,
            16.0,
            rgba(255, 255, 255, 13),
            1.0,
        );

        let initial = self
            .username
            .chars()
            .next()
            .map(|ch| ch.to_uppercase().to_string())
            .unwrap_or_else(|| "U".to_owned());
        self.text(
            canvas,
            &initial,
            avatar_x - 16.0,
            avatar_y - 15.0,
            32.0,
            30.0,
            12.0,
            TextWeight::Bold,
            rgba(108, 113, 126, 255),
            TextAlign::Center,
        );

        self.text(
            canvas,
            &self.username,
            PANEL_LEFT + 64.0,
            top + 21.0,
            112.0,
            14.0,
            11.0,
            TextWeight::Bold,
            rgba(229, 233, 242, 255),
            TextAlign::Left,
        );
        let role = if self.is_admin {
            "Dev access"
        } else {
            "User access"
        };
        self.text(
            canvas,
            role,
            PANEL_LEFT + 64.0,
            top + 36.0,
            112.0,
            12.0,
            9.0,
            TextWeight::Medium,
            rgba(75, 82, 99, 255),
            TextAlign::Left,
        );
    }

    fn draw_header(&self, canvas: &Canvas) {
        fill_rect(
            canvas,
            CONTENT_LEFT,
            PANEL_TOP + HEADER_HEIGHT - 1.0,
            PANEL_RIGHT - CONTENT_LEFT - 16.0,
            1.0,
            rgba(255, 255, 255, 10),
        );

        let header_y = PANEL_TOP + 18.0;
        self.draw_header_button(
            canvas,
            CONTENT_LEFT + 16.0,
            header_y,
            154.0,
            "default.cfg",
            HeaderIcon::Save,
        );
        self.draw_header_button(
            canvas,
            CONTENT_LEFT + 184.0,
            header_y,
            118.0,
            self.selected_category.display_name(),
            HeaderIcon::Chevron,
        );
        self.draw_search_icon(canvas, PANEL_RIGHT - 46.0, PANEL_TOP + 24.0);
    }

    fn draw_header_button(
        &self,
        canvas: &Canvas,
        x: f32,
        y: f32,
        width: f32,
        label: &str,
        icon: HeaderIcon,
    ) {
        fill_round(canvas, x, y, width, 28.0, 4.0, rgba(12, 13, 17, 255));
        stroke_round(canvas, x, y, width, 28.0, 4.0, rgba(255, 255, 255, 10), 1.0);
        fill_rect(canvas, x + 34.0, y, 1.0, 28.0, rgba(255, 255, 255, 10));

        match icon {
            HeaderIcon::Save => {
                self.draw_save_icon(canvas, x + 11.0, y + 8.0, rgba(108, 113, 126, 255))
            }
            HeaderIcon::Chevron => {
                self.draw_chevron_down(canvas, x + width - 18.0, y + 11.0, rgba(108, 113, 126, 255))
            }
        }

        self.text(
            canvas,
            label,
            x + 45.0,
            y + 4.0,
            width - 66.0,
            20.0,
            11.0,
            TextWeight::Bold,
            rgba(160, 165, 181, 255),
            TextAlign::Left,
        );
    }

    fn draw_save_icon(&self, canvas: &Canvas, x: f32, y: f32, color: Color) {
        stroke_round(canvas, x, y, 12.0, 12.0, 1.5, color, 1.2);
        fill_rect(canvas, x + 3.0, y + 2.5, 6.0, 3.0, color);
        line(canvas, x + 3.0, y + 9.0, x + 9.0, y + 9.0, color, 1.2);
    }

    fn draw_chevron_down(&self, canvas: &Canvas, x: f32, y: f32, color: Color) {
        line(canvas, x, y, x + 4.0, y + 4.0, color, 1.4);
        line(canvas, x + 4.0, y + 4.0, x + 8.0, y, color, 1.4);
    }

    fn draw_search_icon(&self, canvas: &Canvas, x: f32, y: f32) {
        stroke_circle(canvas, x + 8.0, y + 8.0, 5.2, rgba(75, 82, 99, 255), 1.5);
        line(
            canvas,
            x + 12.0,
            y + 12.0,
            x + 16.0,
            y + 16.0,
            rgba(75, 82, 99, 255),
            1.5,
        );
    }

    fn draw_content(&self, canvas: &Canvas) {
        let left_column = CONTENT_LEFT + CONTENT_PADDING;
        let right_column = left_column + COLUMN_WIDTH + COLUMN_GAP;

        self.draw_module_group(canvas, left_column, CONTENT_TOP, COLUMN_WIDTH);
        self.draw_category_settings(canvas, right_column, CONTENT_TOP, COLUMN_WIDTH);
    }

    fn draw_module_group(&self, canvas: &Canvas, x: f32, y: f32, width: f32) {
        self.group_header(canvas, "Modules", x, y);

        let visible_cards: Vec<_> = self
            .cards
            .iter()
            .filter(|card| card.category == self.selected_category)
            .collect();
        let row_count = visible_cards.len().max(1);
        let box_top = y + 18.0;
        let box_height = 8.0 + row_count as f32 * MODULE_ROW_HEIGHT;

        fill_round(
            canvas,
            x,
            box_top,
            width,
            box_height,
            8.0,
            rgba(20, 22, 29, 255),
        );
        stroke_round(
            canvas,
            x,
            box_top,
            width,
            box_height,
            8.0,
            rgba(255, 255, 255, 10),
            1.0,
        );

        if visible_cards.is_empty() {
            self.text(
                canvas,
                "No modules",
                x + 16.0,
                box_top + 10.0,
                width - 32.0,
                24.0,
                11.0,
                TextWeight::SemiBold,
                rgba(108, 113, 126, 255),
                TextAlign::Left,
            );
            return;
        }

        for (row, card) in visible_cards.into_iter().enumerate() {
            let row_top = box_top + 4.0 + row as f32 * MODULE_ROW_HEIGHT;
            if row > 0 {
                fill_rect(
                    canvas,
                    x + 12.0,
                    row_top,
                    width - 24.0,
                    1.0,
                    rgba(255, 255, 255, 5),
                );
            }
            self.text(
                canvas,
                card.name,
                x + 16.0,
                row_top + 7.0,
                width - 78.0,
                16.0,
                11.0,
                TextWeight::SemiBold,
                rgba(219, 225, 237, 255),
                TextAlign::Left,
            );
            self.text(
                canvas,
                card.description,
                x + 16.0,
                row_top + 24.0,
                width - 78.0,
                14.0,
                9.5,
                TextWeight::Medium,
                rgba(90, 97, 112, 255),
                TextAlign::Left,
            );
            self.draw_switch(
                canvas,
                x + width - 52.0,
                row_top + (MODULE_ROW_HEIGHT - SWITCH_HEIGHT) * 0.5,
                card.enabled,
            );
        }
    }

    fn draw_category_settings(&self, canvas: &Canvas, x: f32, y: f32, width: f32) {
        self.group_header(canvas, "Main", x, y);
        let main_top = y + 18.0;
        let main_height = 8.0 + SETTING_ROW_HEIGHT * 3.0 + SLIDER_ROW_HEIGHT;
        fill_round(
            canvas,
            x,
            main_top,
            width,
            main_height,
            8.0,
            rgba(20, 22, 29, 255),
        );
        stroke_round(
            canvas,
            x,
            main_top,
            width,
            main_height,
            8.0,
            rgba(255, 255, 255, 10),
            1.0,
        );

        let category_enabled = self
            .cards
            .iter()
            .any(|card| card.category == self.selected_category && card.enabled);
        self.draw_setting_switch_row(
            canvas,
            x,
            main_top + 4.0,
            width,
            "Enabled",
            category_enabled,
        );
        self.draw_setting_dropdown_row(
            canvas,
            x,
            main_top + 4.0 + SETTING_ROW_HEIGHT,
            width,
            "Preset",
            self.selected_category.display_name(),
        );
        self.draw_slider_row(
            canvas,
            x,
            main_top + 4.0 + SETTING_ROW_HEIGHT * 2.0,
            width,
            "Opacity",
            "85%",
            0.85,
        );
        self.draw_setting_value_row(
            canvas,
            x,
            main_top + 4.0 + SETTING_ROW_HEIGHT * 2.0 + SLIDER_ROW_HEIGHT,
            width,
            "Bind",
            "None",
        );

        let second_y = main_top + main_height + 24.0;
        self.group_header(canvas, "Selection", x, second_y);
        let box_top = second_y + 18.0;
        let box_height = 8.0 + SETTING_ROW_HEIGHT * 3.0;
        fill_round(
            canvas,
            x,
            box_top,
            width,
            box_height,
            8.0,
            rgba(20, 22, 29, 255),
        );
        stroke_round(
            canvas,
            x,
            box_top,
            width,
            box_height,
            8.0,
            rgba(255, 255, 255, 10),
            1.0,
        );
        self.draw_setting_dropdown_row(
            canvas,
            x,
            box_top + 4.0,
            width,
            "Target",
            "Highest priority",
        );
        self.draw_setting_switch_row(
            canvas,
            x,
            box_top + 4.0 + SETTING_ROW_HEIGHT,
            width,
            "Show in HUD",
            true,
        );
        self.draw_setting_value_row(
            canvas,
            x,
            box_top + 4.0 + SETTING_ROW_HEIGHT * 2.0,
            width,
            "Status",
            if category_enabled { "Active" } else { "Idle" },
        );
    }

    fn group_header(&self, canvas: &Canvas, label: &str, x: f32, y: f32) {
        self.text(
            canvas,
            label,
            x,
            y,
            160.0,
            12.0,
            9.0,
            TextWeight::Black,
            rgba(75, 82, 99, 255),
            TextAlign::Left,
        );
    }

    fn draw_setting_switch_row(
        &self,
        canvas: &Canvas,
        x: f32,
        y: f32,
        width: f32,
        label: &str,
        enabled: bool,
    ) {
        self.setting_label(canvas, label, x + 16.0, y, width - 84.0, SETTING_ROW_HEIGHT);
        self.draw_switch(
            canvas,
            x + width - 52.0,
            y + (SETTING_ROW_HEIGHT - SWITCH_HEIGHT) * 0.5,
            enabled,
        );
        self.row_divider(canvas, x, y + SETTING_ROW_HEIGHT, width);
    }

    fn draw_setting_dropdown_row(
        &self,
        canvas: &Canvas,
        x: f32,
        y: f32,
        width: f32,
        label: &str,
        value: &str,
    ) {
        self.setting_label(
            canvas,
            label,
            x + 16.0,
            y,
            width - 140.0,
            SETTING_ROW_HEIGHT,
        );
        let dropdown_width = 124.0;
        let dropdown_x = x + width - dropdown_width - 16.0;
        let dropdown_y = y + 7.0;
        fill_round(
            canvas,
            dropdown_x,
            dropdown_y,
            dropdown_width,
            24.0,
            4.0,
            rgba(12, 13, 17, 255),
        );
        stroke_round(
            canvas,
            dropdown_x,
            dropdown_y,
            dropdown_width,
            24.0,
            4.0,
            rgba(255, 255, 255, 10),
            1.0,
        );
        self.text(
            canvas,
            value,
            dropdown_x + 8.0,
            dropdown_y + 4.0,
            dropdown_width - 28.0,
            16.0,
            10.0,
            TextWeight::Bold,
            nl_accent(),
            TextAlign::Left,
        );
        self.draw_chevron_down(
            canvas,
            dropdown_x + dropdown_width - 18.0,
            dropdown_y + 9.0,
            rgba(108, 113, 126, 255),
        );
        self.row_divider(canvas, x, y + SETTING_ROW_HEIGHT, width);
    }

    fn draw_setting_value_row(
        &self,
        canvas: &Canvas,
        x: f32,
        y: f32,
        width: f32,
        label: &str,
        value: &str,
    ) {
        self.setting_label(
            canvas,
            label,
            x + 16.0,
            y,
            width - 110.0,
            SETTING_ROW_HEIGHT,
        );
        fill_round(
            canvas,
            x + width - 72.0,
            y + 10.0,
            56.0,
            18.0,
            4.0,
            rgba(12, 13, 17, 255),
        );
        stroke_round(
            canvas,
            x + width - 72.0,
            y + 10.0,
            56.0,
            18.0,
            4.0,
            rgba(255, 255, 255, 10),
            1.0,
        );
        self.text(
            canvas,
            value,
            x + width - 72.0,
            y + 11.0,
            56.0,
            14.0,
            9.0,
            TextWeight::Bold,
            rgba(108, 113, 126, 255),
            TextAlign::Center,
        );
        self.row_divider(canvas, x, y + SETTING_ROW_HEIGHT, width);
    }

    fn draw_slider_row(
        &self,
        canvas: &Canvas,
        x: f32,
        y: f32,
        width: f32,
        label: &str,
        value: &str,
        percent: f32,
    ) {
        self.setting_label(canvas, label, x + 16.0, y + 2.0, width - 110.0, 24.0);
        fill_round(
            canvas,
            x + width - 58.0,
            y + 10.0,
            42.0,
            18.0,
            4.0,
            rgba(12, 13, 17, 255),
        );
        stroke_round(
            canvas,
            x + width - 58.0,
            y + 10.0,
            42.0,
            18.0,
            4.0,
            rgba(255, 255, 255, 10),
            1.0,
        );
        self.text(
            canvas,
            value,
            x + width - 58.0,
            y + 11.0,
            42.0,
            14.0,
            9.0,
            TextWeight::Bold,
            rgba(108, 113, 126, 255),
            TextAlign::Center,
        );

        let bar_x = x + 16.0;
        let bar_y = y + 36.0;
        let bar_w = width - 32.0;
        fill_round(canvas, bar_x, bar_y, bar_w, 3.0, 1.5, rgba(32, 34, 43, 255));
        fill_round(
            canvas,
            bar_x,
            bar_y,
            bar_w * percent.clamp(0.0, 1.0),
            3.0,
            1.5,
            nl_accent(),
        );
        fill_circle(
            canvas,
            bar_x + bar_w * percent.clamp(0.0, 1.0),
            bar_y + 1.5,
            4.5,
            rgba(255, 255, 255, 255),
        );
        self.row_divider(canvas, x, y + SLIDER_ROW_HEIGHT, width);
    }

    fn setting_label(&self, canvas: &Canvas, label: &str, x: f32, y: f32, width: f32, height: f32) {
        self.text(
            canvas,
            label,
            x,
            y,
            width,
            height,
            11.0,
            TextWeight::Medium,
            rgba(160, 165, 181, 255),
            TextAlign::Left,
        );
    }

    fn row_divider(&self, canvas: &Canvas, x: f32, y: f32, width: f32) {
        fill_rect(
            canvas,
            x + 12.0,
            y,
            width - 24.0,
            1.0,
            rgba(255, 255, 255, 5),
        );
    }

    fn draw_switch(&self, canvas: &Canvas, left: f32, top: f32, enabled: bool) {
        let bg = if enabled {
            nl_accent()
        } else {
            rgba(32, 34, 43, 255)
        };
        fill_round(
            canvas,
            left,
            top,
            SWITCH_WIDTH,
            SWITCH_HEIGHT,
            SWITCH_HEIGHT / 2.0,
            bg,
        );
        if enabled {
            stroke_round(
                canvas,
                left,
                top,
                SWITCH_WIDTH,
                SWITCH_HEIGHT,
                SWITCH_HEIGHT / 2.0,
                rgba(61, 129, 247, 50),
                1.0,
            );
        }
        let knob_x = if enabled { left + 24.0 } else { left + 8.0 };
        fill_circle(
            canvas,
            knob_x,
            top + SWITCH_HEIGHT * 0.5,
            6.0,
            rgba(255, 255, 255, 255),
        );
    }

    fn text(
        &self,
        canvas: &Canvas,
        value: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        size: f32,
        weight: TextWeight,
        color: Color,
        align: TextAlign,
    ) {
        if value.is_empty() || width <= 0.0 || height <= 0.0 {
            return;
        }

        let font = self.font(size, weight);
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(color);

        let (text_width, _) = font.measure_str(value, Some(&paint));
        let draw_x = match align {
            TextAlign::Left => x,
            TextAlign::Center => x + (width - text_width) * 0.5,
        };
        let (_, metrics) = font.metrics();
        let baseline = y + (height - metrics.ascent - metrics.descent) * 0.5;

        canvas.save();
        canvas.clip_rect(sk_rect(x, y, x + width, y + height), None, true);
        canvas.draw_str(value, (draw_x, baseline), &font, &paint);
        canvas.restore();
    }

    fn font(&self, size: f32, weight: TextWeight) -> Font {
        let typeface = match weight {
            TextWeight::Normal => &self.regular_typeface,
            TextWeight::Medium => &self.medium_typeface,
            TextWeight::SemiBold => &self.semibold_typeface,
            TextWeight::Bold => &self.bold_typeface,
            TextWeight::Black => &self.black_typeface,
        }
        .as_ref()
        .or(self.regular_typeface.as_ref());

        let mut font = if let Some(typeface) = typeface {
            Font::new(typeface.clone(), Some(size))
        } else {
            let mut font = Font::default();
            font.set_size(size);
            font
        };
        font.set_subpixel(true);
        font.set_linear_metrics(true);
        font
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

#[derive(Clone, Copy)]
enum TextWeight {
    Normal,
    Medium,
    SemiBold,
    Bold,
    Black,
}

#[derive(Clone, Copy)]
enum TextAlign {
    Left,
    Center,
}

#[derive(Clone, Copy)]
enum HeaderIcon {
    Save,
    Chevron,
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
                app.render(paint.hdc);
            }
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if let Some(app) = unsafe { app_from_hwnd(hwnd) } {
                app.resize();
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
                if y <= HEADER_DRAG_HEIGHT && x >= PANEL_LEFT && x <= PANEL_RIGHT {
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
            description: "Rust Skia click interface.",
            enabled: true,
        },
    ]
}

fn match_typeface(font_mgr: &FontMgr, weight: font_style::Weight) -> Option<Typeface> {
    let style = FontStyle::new(
        weight,
        font_style::Width::NORMAL,
        font_style::Slant::Upright,
    );
    font_mgr
        .match_family_style("Segoe UI", style)
        .or_else(|| font_mgr.match_family_style("Inter", style))
}

fn load_skia_image(path: &PathBuf) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    Image::from_encoded(Data::new_copy(&bytes))
}

fn blit_pixels(hdc: HDC, pixels: &[u8], width: i32, height: i32) {
    let mut bitmap_info = BITMAPINFO::default();
    bitmap_info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: pixels.len() as u32,
        ..Default::default()
    };

    unsafe {
        let _ = SetDIBitsToDevice(
            hdc,
            0,
            0,
            width as u32,
            height as u32,
            0,
            0,
            0,
            height as u32,
            pixels.as_ptr().cast::<c_void>(),
            &bitmap_info,
            DIB_RGB_COLORS,
        );
    }
}

fn module_group_layout() -> (f32, f32, f32) {
    (
        CONTENT_LEFT + CONTENT_PADDING,
        CONTENT_TOP + 18.0,
        COLUMN_WIDTH,
    )
}

fn nav_item_top(index: usize) -> f32 {
    NAV_TOP + NAV_HEADER_HEIGHT + index as f32 * NAV_ITEM_STEP
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

fn is_escape_key_down() -> bool {
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) & KEY_STATE_DOWN_MASK != 0 }
}

fn sk_rect(left: f32, top: f32, right: f32, bottom: f32) -> SkRect {
    SkRect::new(left, top, right, bottom)
}

fn fill_rect(canvas: &Canvas, x: f32, y: f32, width: f32, height: f32, color: Color) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color);
    paint.set_style(PaintStyle::Fill);
    canvas.draw_rect(sk_rect(x, y, x + width, y + height), &paint);
}

fn fill_round(canvas: &Canvas, x: f32, y: f32, width: f32, height: f32, radius: f32, color: Color) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    fill_rrect(
        canvas,
        &RRect::new_rect_xy(sk_rect(x, y, x + width, y + height), radius, radius),
        color,
    );
}

fn fill_gradient_round(
    canvas: &Canvas,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    start: Color,
    end: Color,
) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let colors = [Color4f::from(start), Color4f::from(end)];
    let gradient_colors = gradient::Colors::new_evenly_spaced(&colors, TileMode::Clamp, None);
    let gradient = gradient::Gradient::new(gradient_colors, gradient::Interpolation::default());
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    if let Some(shader) = gradient::shaders::linear_gradient(
        (Point::new(x, y), Point::new(x + width, y + height)),
        &gradient,
        None,
    ) {
        paint.set_shader(shader);
    } else {
        paint.set_color(start);
    }
    canvas.draw_rrect(
        RRect::new_rect_xy(sk_rect(x, y, x + width, y + height), radius, radius),
        &paint,
    );
}

fn fill_rrect(canvas: &Canvas, rect: &RRect, color: Color) {
    fill_rrect_with_antialias(canvas, rect, color, true);
}

fn fill_rrect_with_antialias(canvas: &Canvas, rect: &RRect, color: Color, anti_alias: bool) {
    let mut paint = Paint::default();
    paint.set_anti_alias(anti_alias);
    paint.set_color(color);
    paint.set_style(PaintStyle::Fill);
    canvas.draw_rrect(rect, &paint);
}

fn stroke_round(
    canvas: &Canvas,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    color: Color,
    stroke_width: f32,
) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    stroke_rrect(
        canvas,
        &RRect::new_rect_xy(sk_rect(x, y, x + width, y + height), radius, radius),
        color,
        stroke_width,
    );
}

fn stroke_rrect(canvas: &Canvas, rect: &RRect, color: Color, stroke_width: f32) {
    stroke_rrect_with_antialias(canvas, rect, color, stroke_width, true);
}

fn stroke_rrect_with_antialias(
    canvas: &Canvas,
    rect: &RRect,
    color: Color,
    stroke_width: f32,
    anti_alias: bool,
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(anti_alias);
    paint.set_color(color);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(stroke_width);
    canvas.draw_rrect(rect, &paint);
}

fn fill_circle(canvas: &Canvas, x: f32, y: f32, radius: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color);
    paint.set_style(PaintStyle::Fill);
    canvas.draw_circle((x, y), radius, &paint);
}

fn stroke_circle(canvas: &Canvas, x: f32, y: f32, radius: f32, color: Color, stroke_width: f32) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(stroke_width);
    canvas.draw_circle((x, y), radius, &paint);
}

fn line(canvas: &Canvas, x1: f32, y1: f32, x2: f32, y2: f32, color: Color, stroke_width: f32) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(stroke_width);
    canvas.draw_line((x1, y1), (x2, y2), &paint);
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_argb(a, r, g, b)
}

fn nl_accent() -> Color {
    rgba(61, 129, 247, 179)
}

fn transparent_key_color() -> Color {
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

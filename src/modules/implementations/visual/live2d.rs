use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    ffi::{CStr, OsStr, c_void},
    fs,
    hash::{Hash, Hasher},
    hint::spin_loop,
    io,
    mem::{size_of, transmute},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{self, null_mut},
    slice,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use crate::modules::{BaseValue, Category, Module, ModuleHandler, ModuleInfo, ModuleState};
use serde_json::{Map as JsonMap, Value as JsonValue};
use skija::{
    AlphaType, BlendMode, Canvas, Color as SkColor, Data, FilterMode, Font, FontMgr, FontStyle,
    Image, ImageInfo, Paint, PaintStyle, Point, Rect as SkRect, SamplingOptions, TileMode,
    Typeface, Vertices, canvas::SaveLayerRec, font_style, surfaces, vertices::VertexMode,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
            GetDeviceCaps, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject, VREFRESH,
        },
        Media::{timeBeginPeriod, timeEndPeriod},
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW},
        UI::WindowsAndMessaging::{
            CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
            DispatchMessageW, GWLP_USERDATA, GetSystemMetrics, GetWindowLongPtrW, IDC_ARROW,
            LoadCursorW, MA_NOACTIVATE, MSG, PM_REMOVE, PeekMessageW, PostMessageW,
            PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
            SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNOACTIVATE,
            SetWindowLongPtrW, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
            WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY,
            WM_QUIT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
            WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
    core::{PCSTR, PCWSTR, w},
};

const MODULE_NAME: &str = "Live2D";
const MODEL_VALUE_NAME: &str = "Model";
const EXPRESSION_VALUE_NAME: &str = "Expression";
const X_VALUE_NAME: &str = "X";
const Y_VALUE_NAME: &str = "Y";
const SCALE_VALUE_NAME: &str = "Scale";
const ALPHA_VALUE_NAME: &str = "Alpha";
const MIRROR_VALUE_NAME: &str = "Mirror";
const FPS_VALUE_NAME: &str = "FPS";
const NO_MODEL_MODE: &str = "None";
const NO_EXPRESSION_MODE: &str = "None";
const STARTING_HWND: isize = -1;
const FALLBACK_TARGET_FPS: f32 = 165.0;
const MIN_TARGET_FPS: f32 = 30.0;
const MAX_TARGET_FPS: f32 = 300.0;
const FRAME_SLEEP_GUARD: Duration = Duration::from_millis(1);
const MODEL_FRAME_PADDING: f32 = 32.0;
const STATUS_FRAME_WIDTH: i32 = 340;
const STATUS_FRAME_HEIGHT: i32 = 34;
const MODEL_SCAN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_MODEL_JSON_DEPTH: usize = 3;
const MAX_FINGERPRINT_DEPTH: usize = 6;
const MEMORY_ALIGNMENT: usize = 64;
const FLAG_BLEND_ADDITIVE: u8 = 1;
const FLAG_BLEND_MULTIPLICATIVE: u8 = 2;
const FLAG_IS_INVERTED_MASK: u8 = 8;
const FLAG_IS_VISIBLE: u8 = 1;

type SharedModuleHandler = Arc<Mutex<ModuleHandler>>;
type CsmReviveMocInPlace = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type CsmGetSizeofModel = unsafe extern "C" fn(*mut c_void) -> i32;
type CsmInitializeModelInPlace = unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> *mut c_void;
type CsmModelCall = unsafe extern "C" fn(*mut c_void);
type CsmReadCanvasInfo =
    unsafe extern "C" fn(*mut c_void, *mut CsmVector2, *mut CsmVector2, *mut f32);
type CsmGetInt = unsafe extern "C" fn(*mut c_void) -> i32;
type CsmGetPointer = unsafe extern "C" fn(*mut c_void) -> *const c_void;

static SHARED_MODULES: OnceLock<SharedModuleHandler> = OnceLock::new();
static MODEL_MONITOR_STARTED: AtomicBool = AtomicBool::new(false);
static START_REQUESTED: AtomicBool = AtomicBool::new(false);
static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn set_shared_module_handler(modules: SharedModuleHandler) {
    let _ = SHARED_MODULES.set(Arc::clone(&modules));
    start_model_monitor(Arc::clone(&modules));
    if START_REQUESTED.load(Ordering::Acquire) || live2d_enabled(&modules) {
        start_overlay_window();
    }
}

#[derive(Debug)]
pub struct Live2D {
    info: ModuleInfo,
    state: ModuleState,
    values: Vec<BaseValue>,
}

impl Live2D {
    pub fn new() -> Self {
        let models = scan_model_dirs(&model_dirs());
        let modes = model_modes(&models);
        let default_model = modes
            .first()
            .cloned()
            .unwrap_or_else(|| NO_MODEL_MODE.to_owned());
        let expressions = expression_modes_for_selection(&models, &default_model);

        Self {
            info: ModuleInfo::new(
                MODULE_NAME,
                "Draws a Live2D model overlay from AppData/Roaming/.nyx_client/models.",
                Category::Visual,
            ),
            state: ModuleState::new(),
            values: vec![
                BaseValue::mode(modes, MODEL_VALUE_NAME, default_model),
                BaseValue::mode(expressions, EXPRESSION_VALUE_NAME, NO_EXPRESSION_MODE),
                BaseValue::number(18.0, -4096.0, 4096.0, X_VALUE_NAME),
                BaseValue::number(72.0, -4096.0, 4096.0, Y_VALUE_NAME),
                BaseValue::number(0.28, 0.03, 4.0, SCALE_VALUE_NAME),
                BaseValue::percentage(1.0, 0.0, 1.0, ALPHA_VALUE_NAME),
                BaseValue::boolean(false, MIRROR_VALUE_NAME),
                BaseValue::number(
                    default_target_fps() as f64,
                    MIN_TARGET_FPS as f64,
                    MAX_TARGET_FPS as f64,
                    FPS_VALUE_NAME,
                ),
            ],
        }
    }
}

impl Default for Live2D {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Live2D {
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
        self.value(MODEL_VALUE_NAME)
    }

    fn on_enable(&mut self) {
        START_REQUESTED.store(true, Ordering::Release);
        start_overlay_window();
    }

    fn on_disable(&mut self) {
        stop_overlay_window();
    }
}

fn start_overlay_window() {
    let Some(modules) = SHARED_MODULES.get().cloned() else {
        START_REQUESTED.store(true, Ordering::Release);
        return;
    };

    if OPEN_HWND
        .compare_exchange(0, STARTING_HWND, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    if thread::Builder::new()
        .name("nyx-live2d-overlay".to_owned())
        .spawn(move || {
            if let Err(error) = run_overlay_window(modules) {
                eprintln!("Live2D overlay failed to start: {error:?}");
                OPEN_HWND.store(0, Ordering::Release);
            }
        })
        .is_err()
    {
        OPEN_HWND.store(0, Ordering::Release);
    }
}

fn stop_overlay_window() {
    START_REQUESTED.store(false, Ordering::Release);
    let hwnd_value = OPEN_HWND.load(Ordering::Acquire);
    if hwnd_value > 0 {
        let hwnd = HWND(hwnd_value as *mut c_void);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn start_model_monitor(modules: SharedModuleHandler) {
    if MODEL_MONITOR_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    if thread::Builder::new()
        .name("nyx-live2d-model-watch".to_owned())
        .spawn(move || monitor_model_directory(modules))
        .is_err()
    {
        MODEL_MONITOR_STARTED.store(false, Ordering::Release);
    }
}

fn monitor_model_directory(modules: SharedModuleHandler) {
    let model_dirs = model_dirs();
    let mut fingerprint = DirectoryFingerprint::default();
    let mut models = Vec::new();
    loop {
        let next = directories_fingerprint(&model_dirs);
        if next != fingerprint {
            fingerprint = next;
            models = scan_model_dirs(&model_dirs);
            update_module_model_modes(&modules, &models);
        } else {
            update_module_expression_modes(&modules, &models);
        }
        thread::sleep(MODEL_SCAN_INTERVAL);
    }
}

fn run_overlay_window(modules: SharedModuleHandler) -> windows::core::Result<()> {
    if !START_REQUESTED.load(Ordering::Acquire) || !live2d_enabled(&modules) {
        OPEN_HWND.store(0, Ordering::Release);
        return Ok(());
    }

    let mut app = Box::new(OverlayApp::new(modules));
    let app_ptr = app.as_mut() as *mut OverlayApp;
    let screen = app.screen;

    let hmodule = unsafe { GetModuleHandleW(PCWSTR::null())? };
    let hinstance = HINSTANCE(hmodule.0);
    let class_name = w!("NyxClientLive2DOverlay");

    let window_class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(overlay_window_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: class_name,
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&window_class);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            class_name,
            w!("NyxClient Live2D Overlay"),
            WS_POPUP,
            screen.x,
            screen.y,
            1,
            1,
            None,
            None,
            Some(hinstance),
            Some(app_ptr.cast::<c_void>()),
        )?
    };

    let _leaked_to_window = Box::into_raw(app);

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    let _timer_resolution = TimerResolution::begin(1);
    let mut message = MSG::default();
    let mut next_frame_at = Instant::now();
    loop {
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() } {
            if message.message == WM_QUIT {
                return Ok(());
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        if OPEN_HWND.load(Ordering::Acquire) != hwnd.0 as isize {
            break;
        }

        let now = Instant::now();
        if now >= next_frame_at {
            let Some(app) = (unsafe { overlay_app_from_hwnd(hwnd) }) else {
                break;
            };
            let interval = app.frame_interval();
            app.tick();
            let after_render = Instant::now();
            next_frame_at += interval;
            if next_frame_at <= after_render {
                next_frame_at = after_render + interval;
            }
        } else {
            sleep_until_frame(next_frame_at, now);
        }
    }

    Ok(())
}

struct OverlayApp {
    hwnd: HWND,
    modules: SharedModuleHandler,
    model_dirs: Vec<PathBuf>,
    models: Vec<Live2DModel>,
    directory_fingerprint: DirectoryFingerprint,
    last_model_scan: Instant,
    screen: ScreenRect,
    core: Option<CubismCore>,
    core_error: String,
    loaded_model_key: Option<PathBuf>,
    loaded_model: Option<CubismModel>,
    loaded_model_error: String,
    last_frame_at: Instant,
    submitted_frame_cache_key: Option<FrameCacheKey>,
    uploader: LayeredFrameUploader,
    typeface: Option<Typeface>,
}

impl OverlayApp {
    fn new(modules: SharedModuleHandler) -> Self {
        let model_dirs = model_dirs();
        let models = scan_model_dirs(&model_dirs);
        update_module_model_modes(&modules, &models);

        Self {
            hwnd: HWND(null_mut()),
            modules,
            directory_fingerprint: directories_fingerprint(&model_dirs),
            last_model_scan: Instant::now(),
            model_dirs,
            models,
            screen: virtual_screen_rect(),
            core: None,
            core_error: String::new(),
            loaded_model_key: None,
            loaded_model: None,
            loaded_model_error: String::new(),
            last_frame_at: Instant::now(),
            submitted_frame_cache_key: None,
            uploader: LayeredFrameUploader::default(),
            typeface: match_typeface(),
        }
    }

    fn tick(&mut self) {
        if self.should_close() {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return;
        }

        self.ensure_screen_rect();
        self.refresh_models_if_changed();
        update_module_expression_modes(&self.modules, &self.models);
        self.render_frame();
    }

    fn frame_interval(&self) -> Duration {
        self.overlay_config().frame_interval()
    }

    fn should_close(&self) -> bool {
        !START_REQUESTED.load(Ordering::Acquire) || !live2d_enabled(&self.modules)
    }

    fn refresh_models_if_changed(&mut self) {
        if self.last_model_scan.elapsed() < MODEL_SCAN_INTERVAL {
            return;
        }
        self.last_model_scan = Instant::now();

        let next_fingerprint = directories_fingerprint(&self.model_dirs);
        if next_fingerprint == self.directory_fingerprint {
            return;
        }

        self.directory_fingerprint = next_fingerprint;
        self.models = scan_model_dirs(&self.model_dirs);
        update_module_model_modes(&self.modules, &self.models);
        self.loaded_model_key = None;
        self.loaded_model = None;
        self.loaded_model_error.clear();
        self.last_frame_at = Instant::now();
        self.submitted_frame_cache_key = None;
    }

    fn ensure_screen_rect(&mut self) {
        let next = virtual_screen_rect();
        if next == self.screen {
            return;
        }

        self.screen = next;
        self.submitted_frame_cache_key = None;
    }

    fn render_frame(&mut self) {
        let config = self.overlay_config();
        if !config.enabled {
            return;
        }
        let now = Instant::now();
        let delta = now.saturating_duration_since(self.last_frame_at);
        self.last_frame_at = now;

        if let Some(cache_key) = self.current_frame_cache_key(&config) {
            if self.submitted_frame_cache_key.as_ref() == Some(&cache_key) {
                return;
            }
        }

        let frame = self.build_frame(&config, delta);
        let cache_key = self.current_frame_cache_key(&config);
        match self.uploader.update(self.hwnd, frame.rect, &frame.pixels) {
            Ok(()) => {
                self.submitted_frame_cache_key = cache_key;
            }
            Err(error) => {
                self.submitted_frame_cache_key = None;
                eprintln!("Live2D overlay update failed: {error:?}");
            }
        }
    }

    fn current_frame_cache_key(&self, config: &OverlayConfig) -> Option<FrameCacheKey> {
        let config_key = FrameConfigKey::from_config(config);
        if config.alpha <= 0.0 || config.selected_model == NO_MODEL_MODE {
            return Some(FrameCacheKey {
                kind: FrameCacheKind::Transparent,
                model_key: None,
                config: config_key,
                rect: transparent_frame_rect(config),
            });
        }

        let model = self.loaded_model.as_ref()?;
        if !model.frame_cacheable(&config.selected_expression) {
            return None;
        }

        let render_width =
            (model.canvas_width() * config.scale).clamp(16.0, self.screen.width as f32 * 2.0);
        let render_height =
            (model.canvas_height() * config.scale).clamp(16.0, self.screen.height as f32 * 2.0);
        Some(FrameCacheKey {
            kind: FrameCacheKind::Model,
            model_key: self.loaded_model_key.clone(),
            config: config_key,
            rect: frame_placement(config.x, config.y, render_width, render_height).rect,
        })
    }

    fn build_frame(&mut self, config: &OverlayConfig, delta: Duration) -> RenderedFrame {
        if config.alpha <= 0.0 || config.selected_model == NO_MODEL_MODE {
            return transparent_frame(config);
        }

        let Some(model) = self
            .models
            .iter()
            .find(|model| model.name == config.selected_model)
            .cloned()
        else {
            return self.status_frame(config, "No Live2D model selected");
        };

        if !self.ensure_cubism_model(&model) {
            let status = if !self.loaded_model_error.is_empty() {
                self.loaded_model_error.clone()
            } else if !self.core_error.is_empty() {
                self.core_error.clone()
            } else if !model.valid() {
                model.error.clone()
            } else {
                "Live2D 模型加载失败".to_owned()
            };
            return self.status_frame(config, &status);
        }

        let Some(core) = self.core.as_ref() else {
            return self.status_frame(config, "Cubism Core 未加载");
        };
        let Some(model) = self.loaded_model.as_mut() else {
            return self.status_frame(config, "Live2D 模型未初始化");
        };

        model.animate(duration_secs(delta), &config.selected_expression);
        if let Err(error) = model.update(core) {
            self.loaded_model_error = format!("Cubism 更新失败: {error}");
            return self.status_frame(config, &self.loaded_model_error);
        }

        let render_width =
            (model.canvas_width() * config.scale).clamp(16.0, self.screen.width as f32 * 2.0);
        let render_height =
            (model.canvas_height() * config.scale).clamp(16.0, self.screen.height as f32 * 2.0);
        let placement = frame_placement(config.x, config.y, render_width, render_height);
        let mut frame = blank_frame(placement.rect);

        if frame
            .with_canvas(|canvas| {
                canvas.clear(SkColor::TRANSPARENT);
                model.draw(
                    canvas,
                    config,
                    placement.local_x,
                    placement.local_y,
                    render_width,
                    render_height,
                );
            })
            .is_none()
        {
            return transparent_frame(config);
        }

        unsafe {
            (core.reset_drawable_dynamic_flags)(model.model_pointer);
        }
        frame
    }

    fn status_frame(&self, config: &OverlayConfig, text: &str) -> RenderedFrame {
        let mut frame = blank_frame(ScreenRect {
            x: config.x.floor() as i32,
            y: config.y.floor() as i32,
            width: STATUS_FRAME_WIDTH,
            height: STATUS_FRAME_HEIGHT,
        });

        if frame
            .with_canvas(|canvas| {
                canvas.clear(SkColor::TRANSPARENT);
                let mut local_config = config.clone();
                local_config.x = 0.0;
                local_config.y = 0.0;
                self.draw_status(canvas, &local_config, text);
            })
            .is_none()
        {
            return transparent_frame(config);
        }

        frame
    }

    fn ensure_cubism_model(&mut self, model: &Live2DModel) -> bool {
        if !model.valid() {
            self.loaded_model_key = None;
            self.loaded_model = None;
            self.loaded_model_error = model.error.clone();
            return false;
        }

        let key = model
            .model_json
            .clone()
            .or_else(|| model.moc.clone())
            .unwrap_or_else(|| PathBuf::from(&model.name))
            .normalize_path();
        if self.loaded_model_key.as_ref() == Some(&key) && self.loaded_model.is_some() {
            return true;
        }

        self.loaded_model_key = Some(key);
        self.loaded_model = None;
        self.loaded_model_error.clear();

        if !self.ensure_cubism_core() {
            self.loaded_model_key = None;
            return false;
        }

        let Some(core) = self.core.as_ref() else {
            self.loaded_model_error = "Cubism Core 未加载".to_owned();
            return false;
        };

        match CubismModel::load(core, model) {
            Ok(cubism_model) => {
                self.loaded_model = Some(cubism_model);
                self.submitted_frame_cache_key = None;
                true
            }
            Err(error) => {
                self.loaded_model_key = None;
                self.submitted_frame_cache_key = None;
                self.loaded_model_error = format!("Cubism 加载失败: {error}");
                false
            }
        }
    }

    fn ensure_cubism_core(&mut self) -> bool {
        if self.core.is_some() {
            return true;
        }

        match CubismCore::load() {
            Ok(core) => {
                self.core = Some(core);
                self.core_error.clear();
                true
            }
            Err(error) => {
                self.core_error = error;
                false
            }
        }
    }

    fn draw_status(&self, canvas: &Canvas, config: &OverlayConfig, text: &str) {
        let message = if text.trim().is_empty() {
            "Live2D 模型暂不可显示"
        } else {
            text.trim()
        };
        let font = self.font(13.0);
        let mut text_paint = Paint::default();
        text_paint.set_anti_alias(true);
        text_paint.set_color(rgba(255, 154, 154, (230.0 * config.alpha).round() as u8));

        let clipped = clip_text(message, &font, &text_paint, 300.0);
        let (text_width, _) = font.measure_str(&clipped, Some(&text_paint));
        let width = text_width + 20.0;
        let height = 30.0;
        let x = config.x;
        let y = config.y;

        let mut bg_paint = Paint::default();
        bg_paint.set_anti_alias(true);
        bg_paint.set_style(PaintStyle::Fill);
        bg_paint.set_color(rgba(8, 9, 12, (190.0 * config.alpha).round() as u8));
        canvas.draw_round_rect(
            SkRect::new(x, y, x + width, y + height),
            6.0,
            6.0,
            &bg_paint,
        );
        canvas.draw_str(clipped, (x + 10.0, y + 20.0), &font, &text_paint);
    }

    fn overlay_config(&self) -> OverlayConfig {
        let mut config = OverlayConfig::default();
        let Ok(modules) = self.modules.lock() else {
            return config;
        };
        let Some(module) = modules.get(MODULE_NAME) else {
            return config;
        };

        config.enabled = module.is_enabled();
        config.selected_model = module
            .value(MODEL_VALUE_NAME)
            .and_then(BaseValue::as_mode)
            .map(|value| value.current_mode().to_owned())
            .unwrap_or_else(|| NO_MODEL_MODE.to_owned());
        config.selected_expression = module
            .value(EXPRESSION_VALUE_NAME)
            .and_then(BaseValue::as_mode)
            .map(|value| value.current_mode().to_owned())
            .unwrap_or_else(|| NO_EXPRESSION_MODE.to_owned());
        config.x = number_value(module, X_VALUE_NAME, config.x as f64) as f32;
        config.y = number_value(module, Y_VALUE_NAME, config.y as f64) as f32;
        config.scale = number_value(module, SCALE_VALUE_NAME, config.scale as f64) as f32;
        config.alpha = number_value(module, ALPHA_VALUE_NAME, config.alpha as f64) as f32;
        config.mirror = boolean_value(module, MIRROR_VALUE_NAME, config.mirror);
        config.target_fps = number_value(module, FPS_VALUE_NAME, config.target_fps as f64) as f32;
        config.sanitize();
        config
    }

    fn font(&self, size: f32) -> Font {
        let mut font = if let Some(typeface) = &self.typeface {
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

struct RenderedFrame {
    rect: ScreenRect,
    pixels: Vec<u8>,
    row_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameCacheKey {
    kind: FrameCacheKind,
    model_key: Option<PathBuf>,
    config: FrameConfigKey,
    rect: ScreenRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameCacheKind {
    Transparent,
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameConfigKey {
    selected_model: String,
    selected_expression: String,
    x: u32,
    y: u32,
    scale: u32,
    alpha: u32,
    mirror: bool,
}

impl FrameConfigKey {
    fn from_config(config: &OverlayConfig) -> Self {
        Self {
            selected_model: config.selected_model.clone(),
            selected_expression: config.selected_expression.clone(),
            x: f32_cache_bits(config.x),
            y: f32_cache_bits(config.y),
            scale: f32_cache_bits(config.scale),
            alpha: f32_cache_bits(config.alpha),
            mirror: config.mirror,
        }
    }
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

struct FramePlacement {
    rect: ScreenRect,
    local_x: f32,
    local_y: f32,
}

fn frame_placement(x: f32, y: f32, width: f32, height: f32) -> FramePlacement {
    let rect_x = (x - MODEL_FRAME_PADDING).floor() as i32;
    let rect_y = (y - MODEL_FRAME_PADDING).floor() as i32;
    let local_x = x - rect_x as f32;
    let local_y = y - rect_y as f32;
    let rect_width = (local_x + width + MODEL_FRAME_PADDING).ceil().max(1.0) as i32;
    let rect_height = (local_y + height + MODEL_FRAME_PADDING).ceil().max(1.0) as i32;
    FramePlacement {
        rect: ScreenRect {
            x: rect_x,
            y: rect_y,
            width: rect_width,
            height: rect_height,
        },
        local_x,
        local_y,
    }
}

fn blank_frame(rect: ScreenRect) -> RenderedFrame {
    let width = rect.width.max(1);
    let height = rect.height.max(1);
    let row_bytes = width as usize * 4;
    RenderedFrame {
        rect: ScreenRect {
            width,
            height,
            ..rect
        },
        pixels: vec![0_u8; row_bytes * height as usize],
        row_bytes,
    }
}

fn transparent_frame(config: &OverlayConfig) -> RenderedFrame {
    blank_frame(transparent_frame_rect(config))
}

fn transparent_frame_rect(config: &OverlayConfig) -> ScreenRect {
    ScreenRect {
        x: config.x.floor() as i32,
        y: config.y.floor() as i32,
        width: 1,
        height: 1,
    }
}

#[derive(Debug, Clone)]
struct Live2DModel {
    name: String,
    model_json: Option<PathBuf>,
    moc: Option<PathBuf>,
    textures: Vec<PathBuf>,
    idle_motions: Vec<MotionSource>,
    expressions: Vec<ExpressionSource>,
    error: String,
}

impl Live2DModel {
    fn valid(&self) -> bool {
        self.error.is_empty()
            && self.model_json.is_some()
            && self.moc.is_some()
            && !self.textures.is_empty()
    }
}

#[derive(Debug, Clone)]
struct MotionSource {
    path: PathBuf,
    fade_in: Option<f32>,
}

#[derive(Debug, Clone)]
struct ExpressionSource {
    name: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct OverlayConfig {
    enabled: bool,
    selected_model: String,
    selected_expression: String,
    x: f32,
    y: f32,
    scale: f32,
    alpha: f32,
    mirror: bool,
    target_fps: f32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            selected_model: NO_MODEL_MODE.to_owned(),
            selected_expression: NO_EXPRESSION_MODE.to_owned(),
            x: 18.0,
            y: 72.0,
            scale: 0.28,
            alpha: 1.0,
            mirror: false,
            target_fps: default_target_fps(),
        }
    }
}

impl OverlayConfig {
    fn sanitize(&mut self) {
        self.x = finite_clamp(self.x, -4096.0, 4096.0);
        self.y = finite_clamp(self.y, -4096.0, 4096.0);
        self.scale = finite_clamp(self.scale, 0.03, 4.0);
        self.alpha = finite_clamp(self.alpha, 0.0, 1.0);
        self.target_fps = finite_clamp(self.target_fps, MIN_TARGET_FPS, MAX_TARGET_FPS);
    }

    fn frame_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.target_fps.max(MIN_TARGET_FPS) as f64)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DirectoryFingerprint {
    hash: u64,
    entries: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScreenRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct CsmVector2 {
    x: f32,
    y: f32,
}

macro_rules! required_core_proc {
    ($library:expr, $name:literal, $ty:ty) => {{
        let proc = unsafe { GetProcAddress($library, PCSTR(concat!($name, "\0").as_ptr())) };
        let Some(proc) = proc else {
            return Err(format!("缺少 Core 函数 {}", $name));
        };
        unsafe { transmute::<unsafe extern "system" fn() -> isize, $ty>(proc) }
    }};
}

macro_rules! optional_core_proc {
    ($library:expr, $name:literal, $ty:ty) => {{
        unsafe { GetProcAddress($library, PCSTR(concat!($name, "\0").as_ptr())) }
            .map(|proc| unsafe { transmute::<unsafe extern "system" fn() -> isize, $ty>(proc) })
    }};
}

struct CubismCore {
    _library: HMODULE,
    revive_moc_in_place: CsmReviveMocInPlace,
    get_sizeof_model: CsmGetSizeofModel,
    initialize_model_in_place: CsmInitializeModelInPlace,
    update_model: CsmModelCall,
    reset_drawable_dynamic_flags: CsmModelCall,
    read_canvas_info: CsmReadCanvasInfo,
    get_parameter_count: CsmGetInt,
    get_parameter_ids: CsmGetPointer,
    get_parameter_values: CsmGetPointer,
    get_parameter_minimum_values: CsmGetPointer,
    get_parameter_maximum_values: CsmGetPointer,
    get_parameter_default_values: CsmGetPointer,
    get_part_count: Option<CsmGetInt>,
    get_part_ids: Option<CsmGetPointer>,
    get_part_opacities: Option<CsmGetPointer>,
    get_drawable_count: CsmGetInt,
    get_drawable_constant_flags: CsmGetPointer,
    get_drawable_dynamic_flags: CsmGetPointer,
    get_drawable_texture_indices: CsmGetPointer,
    get_drawable_render_orders: Option<CsmGetPointer>,
    get_render_orders: Option<CsmGetPointer>,
    get_drawable_draw_orders: Option<CsmGetPointer>,
    get_drawable_opacities: CsmGetPointer,
    get_drawable_mask_counts: CsmGetPointer,
    get_drawable_masks: Option<CsmGetPointer>,
    get_drawable_vertex_counts: CsmGetPointer,
    get_drawable_vertex_positions: CsmGetPointer,
    get_drawable_vertex_uvs: CsmGetPointer,
    get_drawable_index_counts: CsmGetPointer,
    get_drawable_indices: CsmGetPointer,
}

impl CubismCore {
    fn load() -> Result<Self, String> {
        let _ = fs::create_dir_all(live2d_core_dir());
        let mut errors = Vec::new();

        for candidate in core_candidates() {
            if !candidate.exists() {
                continue;
            }

            match load_library_path(&candidate).and_then(Self::from_library) {
                Ok(core) => return Ok(core),
                Err(error) => errors.push(format!("{}: {error}", candidate.display())),
            }
        }

        for name in ["Live2DCubismCore.dll", "Live2DCubismCore64.dll"] {
            match load_library_name(name).and_then(Self::from_library) {
                Ok(core) => return Ok(core),
                Err(error) => errors.push(format!("{name}: {error}")),
            }
        }

        let expected = live2d_core_dir().join("Live2DCubismCore.dll");
        let detail = errors
            .first()
            .map(|error| format!(" / {error}"))
            .unwrap_or_default();
        Err(format!(
            "Cubism Core 未找到：请放到 {}{detail}",
            expected.display()
        ))
    }

    fn from_library(library: HMODULE) -> Result<Self, String> {
        Ok(Self {
            _library: library,
            revive_moc_in_place: required_core_proc!(
                library,
                "csmReviveMocInPlace",
                CsmReviveMocInPlace
            ),
            get_sizeof_model: required_core_proc!(library, "csmGetSizeofModel", CsmGetSizeofModel),
            initialize_model_in_place: required_core_proc!(
                library,
                "csmInitializeModelInPlace",
                CsmInitializeModelInPlace
            ),
            update_model: required_core_proc!(library, "csmUpdateModel", CsmModelCall),
            reset_drawable_dynamic_flags: required_core_proc!(
                library,
                "csmResetDrawableDynamicFlags",
                CsmModelCall
            ),
            read_canvas_info: required_core_proc!(library, "csmReadCanvasInfo", CsmReadCanvasInfo),
            get_parameter_count: required_core_proc!(library, "csmGetParameterCount", CsmGetInt),
            get_parameter_ids: required_core_proc!(library, "csmGetParameterIds", CsmGetPointer),
            get_parameter_values: required_core_proc!(
                library,
                "csmGetParameterValues",
                CsmGetPointer
            ),
            get_parameter_minimum_values: required_core_proc!(
                library,
                "csmGetParameterMinimumValues",
                CsmGetPointer
            ),
            get_parameter_maximum_values: required_core_proc!(
                library,
                "csmGetParameterMaximumValues",
                CsmGetPointer
            ),
            get_parameter_default_values: required_core_proc!(
                library,
                "csmGetParameterDefaultValues",
                CsmGetPointer
            ),
            get_part_count: optional_core_proc!(library, "csmGetPartCount", CsmGetInt),
            get_part_ids: optional_core_proc!(library, "csmGetPartIds", CsmGetPointer),
            get_part_opacities: optional_core_proc!(library, "csmGetPartOpacities", CsmGetPointer),
            get_drawable_count: required_core_proc!(library, "csmGetDrawableCount", CsmGetInt),
            get_drawable_constant_flags: required_core_proc!(
                library,
                "csmGetDrawableConstantFlags",
                CsmGetPointer
            ),
            get_drawable_dynamic_flags: required_core_proc!(
                library,
                "csmGetDrawableDynamicFlags",
                CsmGetPointer
            ),
            get_drawable_texture_indices: required_core_proc!(
                library,
                "csmGetDrawableTextureIndices",
                CsmGetPointer
            ),
            get_drawable_render_orders: optional_core_proc!(
                library,
                "csmGetDrawableRenderOrders",
                CsmGetPointer
            ),
            get_render_orders: optional_core_proc!(library, "csmGetRenderOrders", CsmGetPointer),
            get_drawable_draw_orders: optional_core_proc!(
                library,
                "csmGetDrawableDrawOrders",
                CsmGetPointer
            ),
            get_drawable_opacities: required_core_proc!(
                library,
                "csmGetDrawableOpacities",
                CsmGetPointer
            ),
            get_drawable_mask_counts: required_core_proc!(
                library,
                "csmGetDrawableMaskCounts",
                CsmGetPointer
            ),
            get_drawable_masks: optional_core_proc!(library, "csmGetDrawableMasks", CsmGetPointer),
            get_drawable_vertex_counts: required_core_proc!(
                library,
                "csmGetDrawableVertexCounts",
                CsmGetPointer
            ),
            get_drawable_vertex_positions: required_core_proc!(
                library,
                "csmGetDrawableVertexPositions",
                CsmGetPointer
            ),
            get_drawable_vertex_uvs: required_core_proc!(
                library,
                "csmGetDrawableVertexUvs",
                CsmGetPointer
            ),
            get_drawable_index_counts: required_core_proc!(
                library,
                "csmGetDrawableIndexCounts",
                CsmGetPointer
            ),
            get_drawable_indices: required_core_proc!(
                library,
                "csmGetDrawableIndices",
                CsmGetPointer
            ),
        })
    }

    fn drawable_render_orders(&self, model_pointer: *mut c_void) -> *const c_void {
        for function in [
            self.get_drawable_render_orders,
            self.get_render_orders,
            self.get_drawable_draw_orders,
        ]
        .into_iter()
        .flatten()
        {
            let pointer = unsafe { function(model_pointer) };
            if !pointer.is_null() {
                return pointer;
            }
        }

        ptr::null()
    }
}

struct AlignedMemory {
    allocation: Vec<u8>,
    aligned_offset: usize,
    size: usize,
}

impl AlignedMemory {
    fn new(size: usize) -> Self {
        let allocation = vec![0_u8; size.saturating_add(MEMORY_ALIGNMENT)];
        let base = allocation.as_ptr() as usize;
        let aligned = (base + MEMORY_ALIGNMENT - 1) & !(MEMORY_ALIGNMENT - 1);
        Self {
            allocation,
            aligned_offset: aligned.saturating_sub(base),
            size,
        }
    }

    fn pointer(&mut self) -> *mut c_void {
        unsafe { self.allocation.as_mut_ptr().add(self.aligned_offset).cast() }
    }

    fn copy_from(&mut self, bytes: &[u8]) {
        let copy_len = bytes.len().min(self.size);
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), self.pointer().cast::<u8>(), copy_len);
        }
    }
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

struct ModelParameters {
    values: *mut f32,
    default_values: Vec<f32>,
    minimum_values: Vec<f32>,
    maximum_values: Vec<f32>,
    indices: HashMap<String, usize>,
}

impl ModelParameters {
    fn load(core: &CubismCore, model_pointer: *mut c_void) -> Self {
        let count = unsafe { (core.get_parameter_count)(model_pointer) }.max(0) as usize;
        let values = unsafe { (core.get_parameter_values)(model_pointer) }
            .cast_mut()
            .cast::<f32>();
        let default_values = read_f32_array(
            unsafe { (core.get_parameter_default_values)(model_pointer) },
            count,
            0.0,
        );
        let minimum_values = read_f32_array(
            unsafe { (core.get_parameter_minimum_values)(model_pointer) },
            count,
            f32::NEG_INFINITY,
        );
        let maximum_values = read_f32_array(
            unsafe { (core.get_parameter_maximum_values)(model_pointer) },
            count,
            f32::INFINITY,
        );
        let indices = read_cstring_array(unsafe { (core.get_parameter_ids)(model_pointer) }, count)
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();

        Self {
            values,
            default_values,
            minimum_values,
            maximum_values,
            indices,
        }
    }

    fn reset_to_defaults(&mut self) {
        if self.values.is_null() || self.default_values.is_empty() {
            return;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                self.default_values.as_ptr(),
                self.values,
                self.default_values.len(),
            );
        }
    }

    fn blend_to(&mut self, id: &str, value: f32, weight: f32) {
        let Some(index) = self.indices.get(id).copied() else {
            return;
        };
        if self.values.is_null() {
            return;
        }

        unsafe {
            let pointer = self.values.add(index);
            let current = *pointer;
            *pointer = self.clamp(index, current + (value - current) * weight);
        }
    }

    fn add(&mut self, id: &str, value: f32, weight: f32) {
        let Some(index) = self.indices.get(id).copied() else {
            return;
        };
        if self.values.is_null() {
            return;
        }

        unsafe {
            let pointer = self.values.add(index);
            *pointer = self.clamp(index, *pointer + value * weight);
        }
    }

    fn multiply(&mut self, id: &str, value: f32, weight: f32) {
        let Some(index) = self.indices.get(id).copied() else {
            return;
        };
        if self.values.is_null() {
            return;
        }

        unsafe {
            let pointer = self.values.add(index);
            let factor = 1.0 + (value - 1.0) * weight;
            *pointer = self.clamp(index, *pointer * factor);
        }
    }

    fn clamp(&self, index: usize, value: f32) -> f32 {
        let minimum = self
            .minimum_values
            .get(index)
            .copied()
            .unwrap_or(f32::NEG_INFINITY);
        let maximum = self
            .maximum_values
            .get(index)
            .copied()
            .unwrap_or(f32::INFINITY);
        clamp_finite_range(value, minimum, maximum)
    }
}

struct ModelParts {
    opacities: *mut f32,
    default_opacities: Vec<f32>,
    indices: HashMap<String, usize>,
}

impl ModelParts {
    fn load(core: &CubismCore, model_pointer: *mut c_void) -> Self {
        let Some(get_part_count) = core.get_part_count else {
            return Self::empty();
        };
        let Some(get_part_ids) = core.get_part_ids else {
            return Self::empty();
        };
        let Some(get_part_opacities) = core.get_part_opacities else {
            return Self::empty();
        };

        let count = unsafe { get_part_count(model_pointer) }.max(0) as usize;
        let opacities = unsafe { get_part_opacities(model_pointer) }
            .cast_mut()
            .cast::<f32>();
        let default_opacities = read_f32_array(opacities.cast::<c_void>(), count, 1.0);
        let indices = read_cstring_array(unsafe { get_part_ids(model_pointer) }, count)
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();

        Self {
            opacities,
            default_opacities,
            indices,
        }
    }

    fn empty() -> Self {
        Self {
            opacities: ptr::null_mut(),
            default_opacities: Vec::new(),
            indices: HashMap::new(),
        }
    }

    fn reset_to_defaults(&mut self) {
        if self.opacities.is_null() || self.default_opacities.is_empty() {
            return;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                self.default_opacities.as_ptr(),
                self.opacities,
                self.default_opacities.len(),
            );
        }
    }

    fn blend_to(&mut self, id: &str, value: f32, weight: f32) {
        let Some(index) = self.indices.get(id).copied() else {
            return;
        };
        if self.opacities.is_null() {
            return;
        }

        unsafe {
            let pointer = self.opacities.add(index);
            let current = *pointer;
            *pointer = finite_clamp(current + (value - current) * weight, 0.0, 1.0);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationTargetKind {
    Parameter,
    PartOpacity,
    Model,
}

#[derive(Debug, Clone)]
struct AnimationTarget {
    kind: AnimationTargetKind,
    id: String,
}

#[derive(Debug, Clone, Copy)]
struct CurvePoint {
    time: f32,
    value: f32,
}

#[derive(Debug, Clone)]
enum CurveSegment {
    Linear {
        start: CurvePoint,
        end: CurvePoint,
    },
    Bezier {
        start: CurvePoint,
        control1: CurvePoint,
        control2: CurvePoint,
        end: CurvePoint,
    },
    Stepped {
        start: CurvePoint,
        end: CurvePoint,
    },
    InverseStepped {
        end: CurvePoint,
    },
}

impl CurveSegment {
    fn end(&self) -> CurvePoint {
        match self {
            Self::Linear { end, .. }
            | Self::Bezier { end, .. }
            | Self::Stepped { end, .. }
            | Self::InverseStepped { end } => *end,
        }
    }

    fn contains_time(&self, time: f32) -> bool {
        time <= self.end().time
    }

    fn evaluate(&self, time: f32) -> f32 {
        match self {
            Self::Linear { start, end } => {
                let amount = normalized_time(time, start.time, end.time);
                start.value + (end.value - start.value) * amount
            }
            Self::Bezier {
                start,
                control1,
                control2,
                end,
            } => evaluate_bezier_value(*start, *control1, *control2, *end, time),
            Self::Stepped { start, .. } => start.value,
            Self::InverseStepped { end } => end.value,
        }
    }
}

#[derive(Debug, Clone)]
struct MotionCurve {
    target: AnimationTarget,
    start: CurvePoint,
    segments: Vec<CurveSegment>,
}

impl MotionCurve {
    fn evaluate(&self, time: f32) -> f32 {
        if time <= self.start.time {
            return self.start.value;
        }

        for segment in &self.segments {
            if segment.contains_time(time) {
                return segment.evaluate(time);
            }
        }

        self.segments
            .last()
            .map(CurveSegment::end)
            .unwrap_or(self.start)
            .value
    }
}

#[derive(Debug, Clone)]
struct Motion {
    duration: f32,
    fade_in: f32,
    curves: Vec<MotionCurve>,
}

impl Motion {
    fn time(&self, elapsed: f32) -> f32 {
        if self.duration > 0.0 {
            elapsed % self.duration
        } else {
            0.0
        }
    }

    fn weight(&self, elapsed: f32) -> f32 {
        let mut weight: f32 = 1.0;
        if self.fade_in > 0.0 {
            weight = weight.min(elapsed / self.fade_in);
        }
        finite_clamp(weight, 0.0, 1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionBlend {
    Add,
    Multiply,
    Overwrite,
}

#[derive(Debug, Clone)]
struct ExpressionParameter {
    id: String,
    value: f32,
    blend: ExpressionBlend,
}

#[derive(Debug, Clone)]
struct Expression {
    name: String,
    fade_in: f32,
    parameters: Vec<ExpressionParameter>,
}

impl Expression {
    fn weight(&self, elapsed: f32) -> f32 {
        if self.fade_in > 0.0 {
            finite_clamp(elapsed / self.fade_in, 0.0, 1.0)
        } else {
            1.0
        }
    }
}

struct CubismModel {
    _moc_memory: AlignedMemory,
    _model_memory: AlignedMemory,
    _moc_pointer: *mut c_void,
    model_pointer: *mut c_void,
    textures: Vec<Image>,
    parameters: ModelParameters,
    parts: ModelParts,
    idle_motions: Vec<Motion>,
    expressions: Vec<Expression>,
    idle_elapsed: f32,
    active_expression: String,
    expression_elapsed: f32,
    drawable_count: usize,
    drawable_constant_flags: Vec<u8>,
    drawable_dynamic_flags: *const u8,
    drawable_texture_indices: Vec<i32>,
    drawable_render_orders: *const i32,
    drawable_render_orders_cache: Vec<i32>,
    drawable_order: Vec<usize>,
    drawable_opacities: *const f32,
    drawable_opacities_cache: Vec<f32>,
    drawable_mask_counts: Vec<i32>,
    drawable_masks: Vec<Vec<i32>>,
    drawable_vertex_counts: Vec<usize>,
    drawable_vertex_position_ptrs: Vec<*const f32>,
    drawable_vertex_positions_cache: Vec<Vec<f32>>,
    drawable_vertex_uvs: Vec<Vec<f32>>,
    drawable_indices: Vec<Vec<u16>>,
    canvas_width: f32,
    canvas_height: f32,
    origin_x: f32,
    origin_y: f32,
    pixels_per_unit: f32,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl CubismModel {
    fn load(core: &CubismCore, source: &Live2DModel) -> Result<Self, String> {
        let moc_path = source
            .moc
            .as_ref()
            .ok_or_else(|| "model3 missing Moc".to_owned())?;
        let moc_bytes = fs::read(moc_path)
            .map_err(|error| format!("读取 moc 失败 {}: {error}", moc_path.display()))?;
        if moc_bytes.is_empty() {
            return Err(format!("moc 文件为空: {}", moc_path.display()));
        }
        if moc_bytes.len() > i32::MAX as usize {
            return Err(format!("moc 文件过大: {}", moc_path.display()));
        }

        let mut moc_memory = AlignedMemory::new(moc_bytes.len());
        moc_memory.copy_from(&moc_bytes);
        let moc_pointer =
            unsafe { (core.revive_moc_in_place)(moc_memory.pointer(), moc_bytes.len() as i32) };
        if moc_pointer.is_null() {
            return Err("csmReviveMocInPlace 返回空指针".to_owned());
        }

        let model_size = unsafe { (core.get_sizeof_model)(moc_pointer) };
        if model_size <= 0 {
            return Err(format!("csmGetSizeofModel 返回 {model_size}"));
        }

        let mut model_memory = AlignedMemory::new(model_size as usize);
        let model_pointer = unsafe {
            (core.initialize_model_in_place)(moc_pointer, model_memory.pointer(), model_size)
        };
        if model_pointer.is_null() {
            return Err("csmInitializeModelInPlace 返回空指针".to_owned());
        }

        let textures = source
            .textures
            .iter()
            .map(|path| load_texture(path))
            .collect::<Result<Vec<_>, _>>()?;
        if textures.is_empty() {
            return Err("模型没有可用贴图".to_owned());
        }

        let parameters = ModelParameters::load(core, model_pointer);
        let parts = ModelParts::load(core, model_pointer);
        let idle_motions = source
            .idle_motions
            .iter()
            .filter_map(|motion| parse_motion(motion).ok())
            .collect::<Vec<_>>();
        let expressions = source
            .expressions
            .iter()
            .filter_map(|expression| parse_expression(expression).ok())
            .collect::<Vec<_>>();

        let drawable_count = unsafe { (core.get_drawable_count)(model_pointer) };
        if drawable_count <= 0 {
            return Err(format!("csmGetDrawableCount 返回 {drawable_count}"));
        }
        let drawable_count = drawable_count as usize;

        let drawable_constant_flags = read_u8_array(
            unsafe { (core.get_drawable_constant_flags)(model_pointer) },
            drawable_count,
            0,
        );
        let drawable_dynamic_flags =
            unsafe { (core.get_drawable_dynamic_flags)(model_pointer) }.cast::<u8>();
        let drawable_texture_indices = read_i32_array(
            unsafe { (core.get_drawable_texture_indices)(model_pointer) },
            drawable_count,
            -1,
        );
        let drawable_render_orders = core.drawable_render_orders(model_pointer).cast::<i32>();
        let drawable_render_orders_cache =
            read_render_orders(drawable_render_orders.cast::<c_void>(), drawable_count);
        let drawable_opacities =
            unsafe { (core.get_drawable_opacities)(model_pointer) }.cast::<f32>();
        let drawable_opacities_cache =
            read_f32_array(drawable_opacities.cast::<c_void>(), drawable_count, 1.0);
        let drawable_mask_counts = read_i32_array(
            unsafe { (core.get_drawable_mask_counts)(model_pointer) },
            drawable_count,
            0,
        );
        let drawable_masks_pointer = core
            .get_drawable_masks
            .map(|function| unsafe { function(model_pointer) })
            .unwrap_or(ptr::null());
        let drawable_masks = read_masks(drawable_masks_pointer, &drawable_mask_counts);
        let drawable_vertex_counts = read_count_array(
            unsafe { (core.get_drawable_vertex_counts)(model_pointer) },
            drawable_count,
        );
        let drawable_vertex_position_ptrs = read_pointer_array::<f32>(
            unsafe { (core.get_drawable_vertex_positions)(model_pointer) },
            drawable_count,
        );
        let drawable_vertex_positions_cache =
            read_drawable_f32_arrays(&drawable_vertex_position_ptrs, &drawable_vertex_counts, 2);
        let drawable_vertex_uv_ptrs = read_pointer_array::<f32>(
            unsafe { (core.get_drawable_vertex_uvs)(model_pointer) },
            drawable_count,
        );
        let drawable_vertex_uvs =
            read_drawable_f32_arrays(&drawable_vertex_uv_ptrs, &drawable_vertex_counts, 2);
        let drawable_index_counts = read_count_array(
            unsafe { (core.get_drawable_index_counts)(model_pointer) },
            drawable_count,
        );
        let drawable_index_ptrs = read_pointer_array::<u16>(
            unsafe { (core.get_drawable_indices)(model_pointer) },
            drawable_count,
        );
        let drawable_indices =
            read_drawable_index_arrays(&drawable_index_ptrs, &drawable_index_counts);

        let bounds = read_bounds(&drawable_vertex_counts, &drawable_vertex_position_ptrs);
        let mut canvas_size = CsmVector2::default();
        let mut canvas_origin = CsmVector2::default();
        let mut pixels_per_unit = 1.0_f32;
        unsafe {
            (core.read_canvas_info)(
                model_pointer,
                &mut canvas_size,
                &mut canvas_origin,
                &mut pixels_per_unit,
            );
        }

        let mut model = Self {
            _moc_memory: moc_memory,
            _model_memory: model_memory,
            _moc_pointer: moc_pointer,
            model_pointer,
            textures,
            parameters,
            parts,
            idle_motions,
            expressions,
            idle_elapsed: 0.0,
            active_expression: NO_EXPRESSION_MODE.to_owned(),
            expression_elapsed: 0.0,
            drawable_count,
            drawable_constant_flags,
            drawable_dynamic_flags,
            drawable_texture_indices,
            drawable_render_orders,
            drawable_render_orders_cache,
            drawable_order: (0..drawable_count).collect(),
            drawable_opacities,
            drawable_opacities_cache,
            drawable_mask_counts,
            drawable_masks,
            drawable_vertex_counts,
            drawable_vertex_position_ptrs,
            drawable_vertex_positions_cache,
            drawable_vertex_uvs,
            drawable_indices,
            canvas_width: positive(canvas_size.x, bounds.width()),
            canvas_height: positive(canvas_size.y, bounds.height()),
            origin_x: canvas_origin.x,
            origin_y: canvas_origin.y,
            pixels_per_unit: positive(pixels_per_unit, 1.0),
            min_x: bounds.min_x,
            min_y: bounds.min_y,
            max_x: bounds.max_x,
            max_y: bounds.max_y,
        };
        model.update_drawable_order();
        Ok(model)
    }

    fn animate(&mut self, delta_seconds: f32, selected_expression: &str) {
        let delta_seconds = if delta_seconds.is_finite() {
            delta_seconds.clamp(0.0, 0.25)
        } else {
            0.0
        };

        self.parameters.reset_to_defaults();
        self.parts.reset_to_defaults();
        self.apply_idle_motion(delta_seconds);
        self.apply_expression(delta_seconds, selected_expression);
    }

    fn apply_idle_motion(&mut self, delta_seconds: f32) {
        let Some(motion) = self.idle_motions.first().cloned() else {
            return;
        };
        self.idle_elapsed += delta_seconds;
        let time = motion.time(self.idle_elapsed);
        let weight = motion.weight(self.idle_elapsed);
        for curve in &motion.curves {
            let value = curve.evaluate(time);
            self.apply_motion_target(&curve.target, value, weight);
        }
    }

    fn apply_motion_target(&mut self, target: &AnimationTarget, value: f32, weight: f32) {
        match target.kind {
            AnimationTargetKind::Parameter => self.parameters.blend_to(&target.id, value, weight),
            AnimationTargetKind::PartOpacity => self.parts.blend_to(&target.id, value, weight),
            AnimationTargetKind::Model => {}
        }
    }

    fn apply_expression(&mut self, delta_seconds: f32, selected_expression: &str) {
        let selected_expression = selected_expression.trim();
        if self.active_expression != selected_expression {
            self.active_expression = selected_expression.to_owned();
            self.expression_elapsed = 0.0;
        }
        self.expression_elapsed += delta_seconds;

        if selected_expression.is_empty() || selected_expression == NO_EXPRESSION_MODE {
            return;
        }

        let Some(expression) = self
            .expressions
            .iter()
            .find(|expression| expression.name == selected_expression)
            .cloned()
        else {
            return;
        };
        let weight = expression.weight(self.expression_elapsed);
        for parameter in &expression.parameters {
            match parameter.blend {
                ExpressionBlend::Add => self.parameters.add(&parameter.id, parameter.value, weight),
                ExpressionBlend::Multiply => {
                    self.parameters
                        .multiply(&parameter.id, parameter.value, weight);
                }
                ExpressionBlend::Overwrite => {
                    self.parameters
                        .blend_to(&parameter.id, parameter.value, weight);
                }
            }
        }
    }

    fn frame_cacheable(&self, selected_expression: &str) -> bool {
        if !self.idle_motions.is_empty() {
            return false;
        }

        let selected_expression = selected_expression.trim();
        if selected_expression.is_empty() || selected_expression == NO_EXPRESSION_MODE {
            return true;
        }
        if self.active_expression != selected_expression {
            return false;
        }

        self.expressions
            .iter()
            .find(|expression| expression.name == selected_expression)
            .map(|expression| self.expression_elapsed >= expression.fade_in)
            .unwrap_or(true)
    }

    fn update(&mut self, core: &CubismCore) -> Result<(), String> {
        unsafe {
            (core.update_model)(self.model_pointer);
        }

        read_i32s_into(
            self.drawable_render_orders,
            &mut self.drawable_render_orders_cache,
        );
        read_f32s_into(self.drawable_opacities, &mut self.drawable_opacities_cache);

        for (drawable, positions) in self.drawable_vertex_positions_cache.iter_mut().enumerate() {
            let pointer = self
                .drawable_vertex_position_ptrs
                .get(drawable)
                .copied()
                .unwrap_or(ptr::null());
            if !pointer.is_null() && !positions.is_empty() {
                unsafe {
                    ptr::copy_nonoverlapping(pointer, positions.as_mut_ptr(), positions.len());
                }
            }
        }

        self.update_drawable_order();
        Ok(())
    }

    fn update_drawable_order(&mut self) {
        let render_orders = &self.drawable_render_orders_cache;
        let mask_counts = &self.drawable_mask_counts;
        let texture_indices = &self.drawable_texture_indices;
        let constant_flags = &self.drawable_constant_flags;
        self.drawable_order.sort_by(|first, second| {
            render_orders
                .get(*first)
                .copied()
                .unwrap_or(*first as i32)
                .cmp(
                    &render_orders
                        .get(*second)
                        .copied()
                        .unwrap_or(*second as i32),
                )
                .then_with(|| {
                    let first_masked = mask_counts.get(*first).copied().unwrap_or_default() > 0;
                    let second_masked = mask_counts.get(*second).copied().unwrap_or_default() > 0;
                    first_masked.cmp(&second_masked)
                })
                .then_with(|| {
                    texture_indices
                        .get(*first)
                        .copied()
                        .unwrap_or(-1)
                        .cmp(&texture_indices.get(*second).copied().unwrap_or(-1))
                })
                .then_with(|| {
                    blend_flags(*constant_flags.get(*first).unwrap_or(&0))
                        .cmp(&blend_flags(*constant_flags.get(*second).unwrap_or(&0)))
                })
                .then_with(|| first.cmp(second))
        });
    }

    fn draw(
        &self,
        canvas: &Canvas,
        config: &OverlayConfig,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) {
        let colors = self.max_vertex_count().max(1);
        let white_colors = vec![SkColor::WHITE; colors];

        for &drawable in &self.drawable_order {
            if !self.visible(drawable) {
                continue;
            }

            let alpha = finite_clamp(config.alpha * self.opacity(drawable), 0.0, 1.0);
            if self.mask_count(drawable) > 0 {
                self.draw_masked_drawable(
                    canvas,
                    config,
                    x,
                    y,
                    width,
                    height,
                    drawable,
                    alpha,
                    &white_colors,
                );
            } else {
                self.draw_textured_drawable(
                    canvas,
                    config,
                    x,
                    y,
                    width,
                    height,
                    drawable,
                    alpha,
                    final_blend_mode(self.constant_flags(drawable)),
                    &white_colors,
                );
            }
        }
    }

    fn draw_masked_drawable(
        &self,
        canvas: &Canvas,
        config: &OverlayConfig,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        drawable: usize,
        alpha: f32,
        white_colors: &[SkColor],
    ) {
        let mut restore_paint = Paint::default();
        restore_paint.set_blend_mode(final_blend_mode(self.constant_flags(drawable)));
        let layer = SaveLayerRec::default().paint(&restore_paint);
        canvas.save_layer(&layer);

        let drawn = self.draw_textured_drawable(
            canvas,
            config,
            x,
            y,
            width,
            height,
            drawable,
            alpha,
            BlendMode::SrcOver,
            white_colors,
        );
        if drawn {
            self.apply_clipping_masks(canvas, config, x, y, width, height, drawable, white_colors);
        }

        canvas.restore();
    }

    fn apply_clipping_masks(
        &self,
        canvas: &Canvas,
        config: &OverlayConfig,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        drawable: usize,
        white_colors: &[SkColor],
    ) -> bool {
        let masks = self.visible_mask_drawables(drawable);
        if masks.is_empty() {
            return false;
        }

        let mask_vertices = masks
            .into_iter()
            .filter_map(|mask_drawable| {
                self.drawable_vertices(
                    mask_drawable,
                    config,
                    x,
                    y,
                    width,
                    height,
                    None,
                    white_colors,
                )
            })
            .collect::<Vec<_>>();
        if mask_vertices.is_empty() {
            return false;
        }

        let mut restore_paint = Paint::default();
        restore_paint.set_blend_mode(if self.inverted_mask(drawable) {
            BlendMode::DstOut
        } else {
            BlendMode::DstIn
        });
        let layer = SaveLayerRec::default().paint(&restore_paint);
        canvas.save_layer(&layer);

        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(SkColor::WHITE);
        paint.set_blend_mode(BlendMode::SrcOver);
        for vertices in &mask_vertices {
            canvas.draw_vertices(vertices, BlendMode::SrcOver, &paint);
        }

        canvas.restore();
        true
    }

    fn visible_mask_drawables(&self, drawable: usize) -> Vec<usize> {
        self.drawable_masks
            .get(drawable)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(|mask_drawable| {
                let mask_drawable = (*mask_drawable >= 0).then_some(*mask_drawable as usize)?;
                (mask_drawable < self.drawable_count && self.visible(mask_drawable))
                    .then_some(mask_drawable)
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_textured_drawable(
        &self,
        canvas: &Canvas,
        config: &OverlayConfig,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        drawable: usize,
        alpha: f32,
        blend_mode: BlendMode,
        white_colors: &[SkColor],
    ) -> bool {
        let texture_index = self.texture_index(drawable);
        let Some(texture) = texture_index.and_then(|index| self.textures.get(index)) else {
            return false;
        };
        let texture_size = (
            texture.width().max(1) as f32,
            texture.height().max(1) as f32,
        );
        let Some(vertices) = self.drawable_vertices(
            drawable,
            config,
            x,
            y,
            width,
            height,
            Some(texture_size),
            white_colors,
        ) else {
            return false;
        };
        let Some(shader) = texture.to_shader(
            (TileMode::Clamp, TileMode::Clamp),
            SamplingOptions::from(FilterMode::Linear),
            None,
        ) else {
            return false;
        };

        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_shader(shader);
        paint.set_alpha_f(alpha);
        paint.set_blend_mode(blend_mode);
        canvas.draw_vertices(&vertices, BlendMode::Modulate, &paint);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn drawable_vertices(
        &self,
        drawable: usize,
        config: &OverlayConfig,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        texture_size: Option<(f32, f32)>,
        white_colors: &[SkColor],
    ) -> Option<Vertices> {
        let vertex_count = self.vertex_count(drawable);
        let positions = self
            .drawable_vertex_positions_cache
            .get(drawable)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let uvs = self
            .drawable_vertex_uvs
            .get(drawable)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let indices = self
            .drawable_indices
            .get(drawable)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if vertex_count == 0
            || positions.len() < vertex_count * 2
            || uvs.len() < vertex_count * 2
            || indices.is_empty()
            || white_colors.len() < vertex_count
        {
            return None;
        }

        let mut sk_positions = Vec::with_capacity(vertex_count);
        let mut sk_texs = Vec::with_capacity(vertex_count);
        for vertex in 0..vertex_count {
            let offset = vertex * 2;
            sk_positions.push(self.screen_point(
                positions[offset],
                positions[offset + 1],
                config,
                x,
                y,
                width,
                height,
            ));
            let tex =
                texture_size.map_or(Point::new(0.0, 0.0), |(texture_width, texture_height)| {
                    let u = uvs[offset];
                    let v = 1.0 - uvs[offset + 1];
                    Point::new(u * texture_width, v * texture_height)
                });
            sk_texs.push(tex);
        }

        let valid_indices = indices
            .iter()
            .copied()
            .filter(|index| (*index as usize) < vertex_count)
            .collect::<Vec<_>>();
        if valid_indices.is_empty() {
            return None;
        }

        Some(Vertices::new_copy(
            VertexMode::Triangles,
            &sk_positions,
            &sk_texs,
            &white_colors[..vertex_count],
            Some(&valid_indices),
        ))
    }

    fn screen_point(
        &self,
        model_x: f32,
        model_y: f32,
        config: &OverlayConfig,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Point {
        let mut pixel_x = self.origin_x + model_x * self.pixels_per_unit;
        let mut pixel_y = self.origin_y - model_y * self.pixels_per_unit;
        if !self.canvas_valid() {
            pixel_x = (model_x - self.min_x) / (self.max_x - self.min_x).max(0.0001)
                * self.canvas_width();
            pixel_y = (self.max_y - model_y) / (self.max_y - self.min_y).max(0.0001)
                * self.canvas_height();
        }

        let screen_x = if config.mirror {
            x + width - pixel_x / self.canvas_width().max(1.0) * width
        } else {
            x + pixel_x / self.canvas_width().max(1.0) * width
        };
        let screen_y = y + pixel_y / self.canvas_height().max(1.0) * height;
        Point::new(screen_x, screen_y)
    }

    fn visible(&self, drawable: usize) -> bool {
        if self.drawable_dynamic_flags.is_null() {
            return true;
        }

        let mut flag = FLAG_IS_VISIBLE;
        if drawable < self.drawable_count {
            unsafe {
                flag = *self.drawable_dynamic_flags.add(drawable);
            }
        }
        (flag & FLAG_IS_VISIBLE) != 0
    }

    fn constant_flags(&self, drawable: usize) -> u8 {
        self.drawable_constant_flags
            .get(drawable)
            .copied()
            .unwrap_or_default()
    }

    fn texture_index(&self, drawable: usize) -> Option<usize> {
        let index = self
            .drawable_texture_indices
            .get(drawable)
            .copied()
            .unwrap_or(-1);
        (index >= 0).then_some(index as usize)
    }

    fn opacity(&self, drawable: usize) -> f32 {
        self.drawable_opacities_cache
            .get(drawable)
            .copied()
            .unwrap_or(1.0)
    }

    fn mask_count(&self, drawable: usize) -> i32 {
        self.drawable_mask_counts
            .get(drawable)
            .copied()
            .unwrap_or_default()
    }

    fn inverted_mask(&self, drawable: usize) -> bool {
        (self.constant_flags(drawable) & FLAG_IS_INVERTED_MASK) != 0
    }

    fn vertex_count(&self, drawable: usize) -> usize {
        self.drawable_vertex_counts
            .get(drawable)
            .copied()
            .unwrap_or_default()
    }

    fn max_vertex_count(&self) -> usize {
        self.drawable_vertex_counts
            .iter()
            .copied()
            .max()
            .unwrap_or_default()
    }

    fn canvas_valid(&self) -> bool {
        self.canvas_width > 0.0 && self.canvas_height > 0.0 && self.pixels_per_unit > 0.0
    }

    fn canvas_width(&self) -> f32 {
        self.canvas_width.max(1.0)
    }

    fn canvas_height(&self) -> f32 {
        self.canvas_height.max(1.0)
    }
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl Bounds {
    fn width(self) -> f32 {
        ((self.max_x - self.min_x) * 256.0).max(1.0)
    }

    fn height(self) -> f32 {
        ((self.max_y - self.min_y) * 256.0).max(1.0)
    }
}

unsafe extern "system" fn overlay_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let app_ptr = unsafe { (*create).lpCreateParams as *mut OverlayApp };
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
        WM_DISPLAYCHANGE => {
            if let Some(app) = unsafe { overlay_app_from_hwnd(hwnd) } {
                app.ensure_screen_rect();
                app.render_frame();
            }
            LRESULT(0)
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
            let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverlayApp };
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

unsafe fn overlay_app_from_hwnd(hwnd: HWND) -> Option<&'static mut OverlayApp> {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverlayApp };
    if raw.is_null() {
        None
    } else {
        Some(unsafe { &mut *raw })
    }
}

fn scan_models(model_dir: &Path) -> io::Result<Vec<Live2DModel>> {
    fs::create_dir_all(model_dir)?;
    let mut folders = Vec::new();
    for entry in fs::read_dir(model_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            folders.push(path);
        }
    }
    folders.sort_by_key(|path| lower_file_name(path));

    Ok(folders
        .into_iter()
        .map(|folder| parse_model_folder(&folder))
        .collect())
}

fn parse_model_folder(folder: &Path) -> Live2DModel {
    let name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("model")
        .to_owned();
    let model_json = find_model_json(folder);
    let Some(model_json_path) = model_json.clone() else {
        return Live2DModel {
            name,
            model_json: None,
            moc: None,
            textures: Vec::new(),
            idle_motions: Vec::new(),
            expressions: Vec::new(),
            error: "No .model3.json found".to_owned(),
        };
    };

    let model_base = model_json_path.parent().unwrap_or(folder).to_path_buf();

    let parse_result = fs::read_to_string(&model_json_path)
        .ok()
        .and_then(|content| serde_json::from_str::<JsonValue>(&content).ok());
    let Some(root) = parse_result else {
        return Live2DModel {
            name,
            model_json: Some(model_json_path),
            moc: None,
            textures: Vec::new(),
            idle_motions: Vec::new(),
            expressions: Vec::new(),
            error: "Failed to parse model3 json".to_owned(),
        };
    };

    let refs = root.get("FileReferences").and_then(JsonValue::as_object);
    let moc = refs.and_then(|refs| resolve_optional(&model_base, refs, "Moc"));
    let textures = refs.map_or_else(Vec::new, |refs| read_textures(&model_base, refs));
    let idle_motions = refs.map_or_else(Vec::new, |refs| read_idle_motions(&model_base, refs));
    let expressions = refs.map_or_else(Vec::new, |refs| read_expressions(&model_base, refs));
    let error = if moc.is_none() {
        "model3 missing Moc or moc file does not exist".to_owned()
    } else if textures.is_empty() {
        "model3 declares no usable textures".to_owned()
    } else {
        String::new()
    };

    Live2DModel {
        name,
        model_json: Some(model_json_path),
        moc,
        textures,
        idle_motions,
        expressions,
        error,
    }
}

fn find_model_json(folder: &Path) -> Option<PathBuf> {
    walk_files(folder, MAX_MODEL_JSON_DEPTH)
        .into_iter()
        .find(|path| has_extension_suffix(path, ".model3.json"))
}

fn read_textures(base: &Path, refs: &JsonMap<String, JsonValue>) -> Vec<PathBuf> {
    refs.get("Textures")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
        .map(|relative| safe_resolve(base, relative))
        .filter(|path| path.exists())
        .collect()
}

fn read_idle_motions(base: &Path, refs: &JsonMap<String, JsonValue>) -> Vec<MotionSource> {
    let Some(motions) = refs.get("Motions").and_then(JsonValue::as_object) else {
        return Vec::new();
    };
    let Some((_, idle_group)) = motions
        .iter()
        .find(|(group, _)| group.eq_ignore_ascii_case("Idle"))
    else {
        return Vec::new();
    };

    idle_group
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| read_motion_source(base, entry))
        .collect()
}

fn read_motion_source(base: &Path, entry: &JsonValue) -> Option<MotionSource> {
    let object = entry.as_object()?;
    let relative = object.get("File").and_then(JsonValue::as_str)?;
    let path = safe_resolve(base, relative);
    if !path.exists() {
        return None;
    }

    Some(MotionSource {
        path,
        fade_in: json_f32(object.get("FadeInTime")),
    })
}

fn read_expressions(base: &Path, refs: &JsonMap<String, JsonValue>) -> Vec<ExpressionSource> {
    refs.get("Expressions")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| read_expression_source(base, entry))
        .collect()
}

fn read_expression_source(base: &Path, entry: &JsonValue) -> Option<ExpressionSource> {
    let object = entry.as_object()?;
    let relative = object.get("File").and_then(JsonValue::as_str)?;
    let path = safe_resolve(base, relative);
    if !path.exists() {
        return None;
    }

    let name = object
        .get("Name")
        .and_then(JsonValue::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| file_stem_name(&path, "expression"));
    Some(ExpressionSource { name, path })
}

fn parse_motion(source: &MotionSource) -> Result<Motion, String> {
    let content = fs::read_to_string(&source.path)
        .map_err(|error| format!("读取 motion 失败 {}: {error}", source.path.display()))?;
    let root = serde_json::from_str::<JsonValue>(&content)
        .map_err(|error| format!("解析 motion 失败 {}: {error}", source.path.display()))?;
    let meta = root.get("Meta").and_then(JsonValue::as_object);
    let duration = meta
        .and_then(|meta| json_f32(meta.get("Duration")))
        .unwrap_or(0.0)
        .max(0.0);
    let fade_in = source
        .fade_in
        .or_else(|| meta.and_then(|meta| json_f32(meta.get("FadeInTime"))))
        .unwrap_or(0.0)
        .max(0.0);
    let curves = root
        .get("Curves")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_motion_curve)
        .collect::<Vec<_>>();

    Ok(Motion {
        duration,
        fade_in,
        curves,
    })
}

fn parse_motion_curve(value: &JsonValue) -> Option<MotionCurve> {
    let object = value.as_object()?;
    let target = parse_animation_target(
        object.get("Target").and_then(JsonValue::as_str)?,
        object.get("Id").and_then(JsonValue::as_str)?,
    )?;
    let segments_json = object.get("Segments").and_then(JsonValue::as_array)?;
    let mut numbers = Vec::with_capacity(segments_json.len());
    for value in segments_json {
        numbers.push(json_f32(Some(value))?);
    }
    if numbers.len() < 2 {
        return None;
    }

    let start = CurvePoint {
        time: numbers[0],
        value: numbers[1],
    };
    let mut current = start;
    let mut index = 2;
    let mut segments = Vec::new();
    while index < numbers.len() {
        let segment_type = numbers[index].round() as i32;
        index += 1;
        match segment_type {
            0 => {
                if index + 1 >= numbers.len() {
                    break;
                }
                let end = CurvePoint {
                    time: numbers[index],
                    value: numbers[index + 1],
                };
                index += 2;
                segments.push(CurveSegment::Linear {
                    start: current,
                    end,
                });
                current = end;
            }
            1 => {
                if index + 5 >= numbers.len() {
                    break;
                }
                let control1 = CurvePoint {
                    time: numbers[index],
                    value: numbers[index + 1],
                };
                let control2 = CurvePoint {
                    time: numbers[index + 2],
                    value: numbers[index + 3],
                };
                let end = CurvePoint {
                    time: numbers[index + 4],
                    value: numbers[index + 5],
                };
                index += 6;
                segments.push(CurveSegment::Bezier {
                    start: current,
                    control1,
                    control2,
                    end,
                });
                current = end;
            }
            2 => {
                if index + 1 >= numbers.len() {
                    break;
                }
                let end = CurvePoint {
                    time: numbers[index],
                    value: numbers[index + 1],
                };
                index += 2;
                segments.push(CurveSegment::Stepped {
                    start: current,
                    end,
                });
                current = end;
            }
            3 => {
                if index + 1 >= numbers.len() {
                    break;
                }
                let end = CurvePoint {
                    time: numbers[index],
                    value: numbers[index + 1],
                };
                index += 2;
                segments.push(CurveSegment::InverseStepped { end });
                current = end;
            }
            _ => break,
        }
    }

    Some(MotionCurve {
        target,
        start,
        segments,
    })
}

fn parse_animation_target(target: &str, id: &str) -> Option<AnimationTarget> {
    let kind = if target.eq_ignore_ascii_case("Parameter") {
        AnimationTargetKind::Parameter
    } else if target.eq_ignore_ascii_case("PartOpacity") {
        AnimationTargetKind::PartOpacity
    } else if target.eq_ignore_ascii_case("Model") {
        AnimationTargetKind::Model
    } else {
        return None;
    };
    Some(AnimationTarget {
        kind,
        id: id.to_owned(),
    })
}

fn parse_expression(source: &ExpressionSource) -> Result<Expression, String> {
    let content = fs::read_to_string(&source.path)
        .map_err(|error| format!("读取 expression 失败 {}: {error}", source.path.display()))?;
    let root = serde_json::from_str::<JsonValue>(&content)
        .map_err(|error| format!("解析 expression 失败 {}: {error}", source.path.display()))?;
    let fade_in = json_f32(root.get("FadeInTime")).unwrap_or(0.0).max(0.0);
    let parameters = root
        .get("Parameters")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_expression_parameter)
        .collect::<Vec<_>>();

    Ok(Expression {
        name: source.name.clone(),
        fade_in,
        parameters,
    })
}

fn parse_expression_parameter(value: &JsonValue) -> Option<ExpressionParameter> {
    let object = value.as_object()?;
    let id = object.get("Id").and_then(JsonValue::as_str)?;
    if id.trim().is_empty() {
        return None;
    }
    let value = json_f32(object.get("Value"))?;
    let blend = object
        .get("Blend")
        .and_then(JsonValue::as_str)
        .map(parse_expression_blend)
        .unwrap_or(ExpressionBlend::Add);

    Some(ExpressionParameter {
        id: id.to_owned(),
        value,
        blend,
    })
}

fn parse_expression_blend(value: &str) -> ExpressionBlend {
    if value.eq_ignore_ascii_case("Multiply") {
        ExpressionBlend::Multiply
    } else if value.eq_ignore_ascii_case("Overwrite") {
        ExpressionBlend::Overwrite
    } else {
        ExpressionBlend::Add
    }
}

fn resolve_optional(base: &Path, refs: &JsonMap<String, JsonValue>, key: &str) -> Option<PathBuf> {
    refs.get(key)
        .and_then(JsonValue::as_str)
        .map(|relative| safe_resolve(base, relative))
        .filter(|path| path.exists())
}

fn walk_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];

    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            } else if path.is_dir() && depth < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }

    files.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    files
}

fn directory_fingerprint(root: &Path) -> DirectoryFingerprint {
    if fs::create_dir_all(root).is_err() {
        return DirectoryFingerprint::default();
    }

    let mut hasher = DefaultHasher::new();
    let mut entries = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            entries.push(path.clone());
            if path.is_dir() && depth < MAX_FINGERPRINT_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }
    entries.sort_by_key(|path| path.to_string_lossy().to_lowercase());

    for path in &entries {
        if let Ok(relative) = path.strip_prefix(root) {
            relative.to_string_lossy().hash(&mut hasher);
        } else {
            path.to_string_lossy().hash(&mut hasher);
        }

        if let Ok(metadata) = fs::metadata(path) {
            metadata.is_dir().hash(&mut hasher);
            metadata.len().hash(&mut hasher);
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default()
                .hash(&mut hasher);
        }
    }

    DirectoryFingerprint {
        hash: hasher.finish(),
        entries: entries.len(),
    }
}

fn directories_fingerprint(roots: &[PathBuf]) -> DirectoryFingerprint {
    let mut hasher = DefaultHasher::new();
    let mut entries = 0_usize;
    for root in roots {
        let fingerprint = directory_fingerprint(root);
        root.to_string_lossy().hash(&mut hasher);
        fingerprint.hash.hash(&mut hasher);
        fingerprint.entries.hash(&mut hasher);
        entries += fingerprint.entries;
    }

    DirectoryFingerprint {
        hash: hasher.finish(),
        entries,
    }
}

fn scan_model_dirs(model_dirs: &[PathBuf]) -> Vec<Live2DModel> {
    model_dirs
        .iter()
        .filter_map(|model_dir| scan_models(model_dir).ok())
        .flatten()
        .collect()
}

fn update_module_model_modes(modules: &SharedModuleHandler, models: &[Live2DModel]) {
    let modes = model_modes(models);
    let Ok(mut modules) = modules.lock() else {
        return;
    };
    let Some(module) = modules.get_mut(MODULE_NAME) else {
        return;
    };
    let Some(mode) = module
        .value_mut(MODEL_VALUE_NAME)
        .and_then(BaseValue::as_mode_mut)
    else {
        return;
    };
    mode.set_modes(modes);
    update_expression_modes_for_module(module, models);
}

fn update_module_expression_modes(modules: &SharedModuleHandler, models: &[Live2DModel]) {
    let Ok(mut modules) = modules.lock() else {
        return;
    };
    let Some(module) = modules.get_mut(MODULE_NAME) else {
        return;
    };
    update_expression_modes_for_module(module, models);
}

fn update_expression_modes_for_module(module: &mut (dyn Module + 'static), models: &[Live2DModel]) {
    let selected_model = module
        .value(MODEL_VALUE_NAME)
        .and_then(BaseValue::as_mode)
        .map(|value| value.current_mode().to_owned())
        .unwrap_or_else(|| NO_MODEL_MODE.to_owned());
    let expression_modes = expression_modes_for_selection(models, &selected_model);
    let Some(expression_value) = module
        .value_mut(EXPRESSION_VALUE_NAME)
        .and_then(BaseValue::as_mode_mut)
    else {
        return;
    };
    if expression_value.modes() != expression_modes.as_slice() {
        expression_value.set_modes(expression_modes);
    }
}

fn model_modes(models: &[Live2DModel]) -> Vec<String> {
    if models.is_empty() {
        vec![NO_MODEL_MODE.to_owned()]
    } else {
        models.iter().map(|model| model.name.clone()).collect()
    }
}

fn expression_modes_for_selection(models: &[Live2DModel], selected_model: &str) -> Vec<String> {
    let mut modes = vec![NO_EXPRESSION_MODE.to_owned()];
    if let Some(model) = models.iter().find(|model| model.name == selected_model) {
        modes.extend(
            model
                .expressions
                .iter()
                .map(|expression| expression.name.clone()),
        );
    }
    modes
}

fn live2d_enabled(modules: &SharedModuleHandler) -> bool {
    modules
        .lock()
        .ok()
        .and_then(|modules| modules.get(MODULE_NAME).map(Module::is_enabled))
        .unwrap_or(false)
}

fn number_value(module: &(dyn Module + 'static), key: &str, fallback: f64) -> f64 {
    module
        .value(key)
        .and_then(BaseValue::as_number)
        .map(|value| value.value())
        .unwrap_or(fallback)
}

fn boolean_value(module: &(dyn Module + 'static), key: &str, fallback: bool) -> bool {
    module
        .value(key)
        .and_then(BaseValue::as_boolean)
        .map(|value| value.value())
        .unwrap_or(fallback)
}

fn safe_resolve(base: &Path, relative: &str) -> PathBuf {
    if relative.trim().is_empty() {
        return base.to_path_buf();
    }
    base.join(relative.replace('\\', "/")).normalize_path()
}

fn has_extension_suffix(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_lowercase().ends_with(suffix))
        .unwrap_or(false)
}

fn lower_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

fn file_stem_name(path: &Path, fallback: &str) -> String {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return fallback.to_owned();
    };
    let lower_name = file_name.to_lowercase();
    for suffix in [".motion3.json", ".exp3.json", ".model3.json", ".json"] {
        if lower_name.ends_with(suffix) && file_name.len() > suffix.len() {
            return file_name[..file_name.len() - suffix.len()].to_owned();
        }
    }

    path.file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| fallback.to_owned())
}

fn json_f32(value: Option<&JsonValue>) -> Option<f32> {
    value
        .and_then(JsonValue::as_f64)
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
}

fn duration_secs(duration: Duration) -> f32 {
    duration.as_secs_f32()
}

fn default_target_fps() -> f32 {
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.0.is_null() {
        return FALLBACK_TARGET_FPS;
    }

    let refresh = unsafe { GetDeviceCaps(Some(screen_dc), VREFRESH) };
    unsafe {
        let _ = ReleaseDC(None, screen_dc);
    }

    let fps = if refresh > 1 {
        refresh as f32
    } else {
        FALLBACK_TARGET_FPS
    };
    finite_clamp(fps, MIN_TARGET_FPS, MAX_TARGET_FPS)
}

fn sleep_until_frame(next_frame_at: Instant, now: Instant) {
    let remaining = next_frame_at.saturating_duration_since(now);
    if remaining > FRAME_SLEEP_GUARD {
        thread::sleep(remaining - FRAME_SLEEP_GUARD);
    } else {
        while Instant::now() < next_frame_at {
            spin_loop();
        }
    }
}

fn model_dirs() -> Vec<PathBuf> {
    vec![model_dir()]
}

fn model_dir() -> PathBuf {
    roaming_app_data_dir().join(".nyx_client").join("models")
}

fn live2d_core_dir() -> PathBuf {
    roaming_app_data_dir()
        .join(".nyx_client")
        .join("live2d")
        .join("core")
}

fn core_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("NYX_LIVE2D_CORE") {
        candidates.push(PathBuf::from(explicit));
    }

    let app_data_core = live2d_core_dir();
    candidates.push(app_data_core.join("Live2DCubismCore.dll"));
    candidates.push(app_data_core.join("Live2DCubismCore64.dll"));

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            candidates.push(exe_dir.join("Live2DCubismCore.dll"));
            candidates.push(
                exe_dir
                    .join("live2d")
                    .join("core")
                    .join("Live2DCubismCore.dll"),
            );
            candidates.push(
                exe_dir
                    .join("assets")
                    .join("live2d")
                    .join("core")
                    .join("Live2DCubismCore.dll"),
            );
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("Live2DCubismCore.dll"));
        candidates.push(
            current_dir
                .join("live2d")
                .join("core")
                .join("Live2DCubismCore.dll"),
        );
    }

    dedup_paths(candidates)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::new();
    for path in paths {
        let normalized = path.normalize_path();
        if !deduped.iter().any(|candidate| candidate == &normalized) {
            deduped.push(normalized);
        }
    }
    deduped
}

fn load_library_path(path: &Path) -> Result<HMODULE, String> {
    let wide = wide_null(path.as_os_str());
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .map_err(|error| format!("LoadLibraryW 失败: {error}"))
}

fn load_library_name(name: &str) -> Result<HMODULE, String> {
    let wide = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .map_err(|error| format!("LoadLibraryW 失败: {error}"))
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn load_texture(path: &Path) -> Result<Image, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("读取贴图失败 {}: {error}", path.display()))?;
    Image::from_encoded(Data::new_copy(&bytes))
        .ok_or_else(|| format!("贴图解码失败: {}", path.display()))
}

fn read_cstring_array(pointer: *const c_void, count: usize) -> Vec<String> {
    read_pointer_array::<i8>(pointer, count)
        .into_iter()
        .map(|pointer| {
            if pointer.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(pointer).to_string_lossy().into_owned() }
            }
        })
        .collect()
}

fn read_u8_array(pointer: *const c_void, count: usize, fallback: u8) -> Vec<u8> {
    let mut values = vec![fallback; count];
    if !pointer.is_null() && count > 0 {
        unsafe {
            ptr::copy_nonoverlapping(pointer.cast::<u8>(), values.as_mut_ptr(), count);
        }
    }
    values
}

fn read_i32_array(pointer: *const c_void, count: usize, fallback: i32) -> Vec<i32> {
    let mut values = vec![fallback; count];
    read_i32s_into(pointer.cast::<i32>(), &mut values);
    values
}

fn read_f32_array(pointer: *const c_void, count: usize, fallback: f32) -> Vec<f32> {
    let mut values = vec![fallback; count];
    read_f32s_into(pointer.cast::<f32>(), &mut values);
    values
}

fn read_count_array(pointer: *const c_void, count: usize) -> Vec<usize> {
    read_i32_array(pointer, count, 0)
        .into_iter()
        .map(|value| value.max(0) as usize)
        .collect()
}

fn read_render_orders(pointer: *const c_void, count: usize) -> Vec<i32> {
    if pointer.is_null() {
        (0..count).map(|index| index as i32).collect()
    } else {
        read_i32_array(pointer, count, 0)
    }
}

fn read_i32s_into(pointer: *const i32, target: &mut [i32]) {
    if !pointer.is_null() && !target.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(pointer, target.as_mut_ptr(), target.len());
        }
    }
}

fn read_f32s_into(pointer: *const f32, target: &mut [f32]) {
    if !pointer.is_null() && !target.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(pointer, target.as_mut_ptr(), target.len());
        }
    }
}

fn read_pointer_array<T>(pointer: *const c_void, count: usize) -> Vec<*const T> {
    if pointer.is_null() || count == 0 {
        return vec![ptr::null(); count];
    }

    let pointers = unsafe { slice::from_raw_parts(pointer.cast::<*const T>(), count) };
    pointers.to_vec()
}

fn read_drawable_f32_arrays(
    pointers: &[*const f32],
    counts: &[usize],
    components: usize,
) -> Vec<Vec<f32>> {
    pointers
        .iter()
        .enumerate()
        .map(|(drawable, pointer)| {
            let len = counts
                .get(drawable)
                .copied()
                .unwrap_or_default()
                .saturating_mul(components.max(1));
            if pointer.is_null() || len == 0 {
                return Vec::new();
            }

            let mut values = vec![0.0_f32; len];
            unsafe {
                ptr::copy_nonoverlapping(*pointer, values.as_mut_ptr(), len);
            }
            values
        })
        .collect()
}

fn read_drawable_index_arrays(pointers: &[*const u16], counts: &[usize]) -> Vec<Vec<u16>> {
    pointers
        .iter()
        .enumerate()
        .map(|(drawable, pointer)| {
            let len = counts.get(drawable).copied().unwrap_or_default();
            if pointer.is_null() || len == 0 {
                return Vec::new();
            }

            let mut values = vec![0_u16; len];
            unsafe {
                ptr::copy_nonoverlapping(*pointer, values.as_mut_ptr(), len);
            }
            values
        })
        .collect()
}

fn read_masks(pointer: *const c_void, mask_counts: &[i32]) -> Vec<Vec<i32>> {
    if pointer.is_null() {
        return vec![Vec::new(); mask_counts.len()];
    }

    let pointers = read_pointer_array::<i32>(pointer, mask_counts.len());
    pointers
        .iter()
        .enumerate()
        .map(|(drawable, pointer)| {
            let len = mask_counts
                .get(drawable)
                .copied()
                .unwrap_or_default()
                .max(0) as usize;
            if pointer.is_null() || len == 0 {
                return Vec::new();
            }

            let mut values = vec![0_i32; len];
            unsafe {
                ptr::copy_nonoverlapping(*pointer, values.as_mut_ptr(), len);
            }
            values
        })
        .collect()
}

fn read_bounds(vertex_counts: &[usize], position_pointers: &[*const f32]) -> Bounds {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for (drawable, pointer) in position_pointers.iter().enumerate() {
        let count = vertex_counts.get(drawable).copied().unwrap_or_default();
        if pointer.is_null() || count == 0 {
            continue;
        }

        let positions = unsafe { slice::from_raw_parts(*pointer, count.saturating_mul(2)) };
        for vertex in positions.chunks_exact(2) {
            min_x = min_x.min(vertex[0]);
            min_y = min_y.min(vertex[1]);
            max_x = max_x.max(vertex[0]);
            max_y = max_y.max(vertex[1]);
        }
    }

    if min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite() {
        Bounds {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    } else {
        Bounds {
            min_x: -1.0,
            min_y: -1.0,
            max_x: 1.0,
            max_y: 1.0,
        }
    }
}

fn blend_flags(flags: u8) -> u8 {
    flags & (FLAG_BLEND_ADDITIVE | FLAG_BLEND_MULTIPLICATIVE)
}

fn final_blend_mode(flags: u8) -> BlendMode {
    if (flags & FLAG_BLEND_ADDITIVE) != 0 {
        BlendMode::Plus
    } else if (flags & FLAG_BLEND_MULTIPLICATIVE) != 0 {
        BlendMode::Multiply
    } else {
        BlendMode::SrcOver
    }
}

fn positive(value: f32, fallback: f32) -> f32 {
    if value > 0.0 && value.is_finite() {
        value
    } else {
        fallback
    }
}

fn normalized_time(time: f32, start: f32, end: f32) -> f32 {
    let duration = end - start;
    if duration.abs() <= f32::EPSILON {
        1.0
    } else {
        finite_clamp((time - start) / duration, 0.0, 1.0)
    }
}

fn evaluate_bezier_value(
    start: CurvePoint,
    control1: CurvePoint,
    control2: CurvePoint,
    end: CurvePoint,
    time: f32,
) -> f32 {
    if end.time <= start.time {
        return end.value;
    }

    let mut low = 0.0_f32;
    let mut high = 1.0_f32;
    for _ in 0..16 {
        let mid = (low + high) * 0.5;
        let x = cubic_bezier(start.time, control1.time, control2.time, end.time, mid);
        if x < time {
            low = mid;
        } else {
            high = mid;
        }
    }
    let amount = (low + high) * 0.5;
    cubic_bezier(
        start.value,
        control1.value,
        control2.value,
        end.value,
        amount,
    )
}

fn cubic_bezier(start: f32, control1: f32, control2: f32, end: f32, amount: f32) -> f32 {
    let inverse = 1.0 - amount;
    inverse.powi(3) * start
        + 3.0 * inverse.powi(2) * amount * control1
        + 3.0 * inverse * amount.powi(2) * control2
        + amount.powi(3) * end
}

fn clamp_finite_range(value: f32, minimum: f32, maximum: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let mut result = value;
    if minimum.is_finite() {
        result = result.max(minimum);
    }
    if maximum.is_finite() {
        result = result.min(maximum);
    }
    result
}

fn roaming_app_data_dir() -> PathBuf {
    if let Some(app_data) = std::env::var_os("APPDATA") {
        return ensure_roaming_dir(PathBuf::from(app_data));
    }

    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(user_profile).join("AppData").join("Roaming");
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn ensure_roaming_dir(path: PathBuf) -> PathBuf {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("Roaming"))
    {
        path
    } else {
        path.join("Roaming")
    }
}

fn virtual_screen_rect() -> ScreenRect {
    let mut x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let mut y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let mut width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let mut height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };

    if width <= 0 || height <= 0 {
        x = 0;
        y = 0;
        width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    }

    ScreenRect {
        x,
        y,
        width: width.max(1),
        height: height.max(1),
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
        "SimHei",
        "DengXian",
        "Segoe UI",
        "Inter",
    ]
    .into_iter()
    .find_map(|family| font_mgr.match_family_style(family, style))
}

fn clip_text(text: &str, font: &Font, paint: &Paint, max_width: f32) -> String {
    if font.measure_str(text, Some(paint)).0 <= max_width {
        return text.to_owned();
    }

    let suffix = "...";
    let mut clipped = text.to_owned();
    while !clipped.is_empty()
        && font
            .measure_str(format!("{clipped}{suffix}"), Some(paint))
            .0
            > max_width
    {
        clipped.pop();
    }
    format!("{clipped}{suffix}")
}

fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> SkColor {
    SkColor::from_argb(alpha, red, green, blue)
}

fn finite_clamp(value: f32, minimum: f32, maximum: f32) -> f32 {
    if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        minimum
    }
}

fn f32_cache_bits(value: f32) -> u32 {
    if value == 0.0 {
        0.0_f32.to_bits()
    } else if value.is_nan() {
        f32::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

trait NormalizePath {
    fn normalize_path(self) -> PathBuf;
}

impl NormalizePath for PathBuf {
    fn normalize_path(self) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in self.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(component.as_os_str()),
            }
        }
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_models_reads_model3_references() {
        let root = unique_temp_dir("nyx_live2d_scan");
        let models_dir = root.join("models");
        let model_dir = models_dir.join("alpha");
        let texture_dir = model_dir.join("textures");
        let motion_dir = model_dir.join("motions");
        let expression_dir = model_dir.join("expressions");
        fs::create_dir_all(&texture_dir).unwrap();
        fs::create_dir_all(&motion_dir).unwrap();
        fs::create_dir_all(&expression_dir).unwrap();
        fs::write(model_dir.join("alpha.moc3"), b"moc").unwrap();
        fs::write(texture_dir.join("texture_00.png"), b"png").unwrap();
        fs::write(
            motion_dir.join("idle.motion3.json"),
            r#"{
                "Meta": { "Duration": 2.0 },
                "Curves": [
                    {
                        "Target": "Parameter",
                        "Id": "ParamAngleX",
                        "Segments": [0.0, 0.0, 0, 1.0, 1.0]
                    }
                ]
            }"#,
        )
        .unwrap();
        fs::write(
            expression_dir.join("smile.exp3.json"),
            r#"{
                "FadeInTime": 0.25,
                "Parameters": [
                    { "Id": "ParamMouthSmile", "Value": 1.0, "Blend": "Overwrite" }
                ]
            }"#,
        )
        .unwrap();
        fs::write(
            model_dir.join("alpha.model3.json"),
            r#"{
                "FileReferences": {
                    "Moc": "alpha.moc3",
                    "Textures": ["textures/texture_00.png"],
                    "Motions": {
                        "Idle": [
                            { "File": "motions/idle.motion3.json", "FadeInTime": 0.1 }
                        ]
                    },
                    "Expressions": [
                        { "Name": "Smile", "File": "expressions/smile.exp3.json" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let models = scan_models(&models_dir).unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "alpha");
        assert!(models[0].valid());
        assert_eq!(models[0].textures.len(), 1);
        assert_eq!(models[0].idle_motions.len(), 1);
        assert_eq!(models[0].expressions.len(), 1);
        assert_eq!(
            expression_modes_for_selection(&models, "alpha"),
            vec![NO_EXPRESSION_MODE.to_owned(), "Smile".to_owned()]
        );

        let motion = parse_motion(&models[0].idle_motions[0]).unwrap();
        assert_eq!(motion.curves.len(), 1);
        assert!((motion.curves[0].evaluate(0.5) - 0.5).abs() < f32::EPSILON);

        let expression = parse_expression(&models[0].expressions[0]).unwrap();
        assert_eq!(expression.name, "Smile");
        assert_eq!(expression.parameters.len(), 1);
        assert_eq!(expression.parameters[0].blend, ExpressionBlend::Overwrite);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn roaming_dir_is_not_duplicated() {
        let app_data = PathBuf::from(r"C:\Users\Test\AppData");
        let roaming = PathBuf::from(r"C:\Users\Test\AppData\Roaming");

        assert_eq!(ensure_roaming_dir(app_data), roaming);
        assert_eq!(ensure_roaming_dir(roaming.clone()), roaming);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{nanos}"))
    }
}

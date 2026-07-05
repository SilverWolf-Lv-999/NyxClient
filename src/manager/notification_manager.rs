use std::{
    cell::RefCell,
    ffi::c_void,
    mem::size_of,
    ptr::{self, null_mut},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use skija::{
    AlphaType, BlendMode, Canvas, Color, Data, Font, FontMgr, FontStyle, Image, ImageInfo, Paint,
    PaintStyle, Point, Rect as SkRect, Typeface, Vertices, font_style, surfaces,
    vertices::VertexMode,
};
use windows::Win32::{
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDeviceCaps, HGDIOBJ, LOGPIXELSX,
        ReleaseDC, SRCCOPY, SelectObject, SetDIBitsToDevice,
    },
    UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    },
};

const TITLE_TEXT_HEIGHT: f32 = 8.0;
const BODY_TEXT_HEIGHT: f32 = 8.0;
const NOTIFICATION_WIDTH: f32 = 230.0;
const NOTIFICATION_HEIGHT: f32 = 28.0;
const RIGHT_PADDING: f32 = 10.0;
const TRIANGLE_WIDTH: f32 = 6.5;
const TRIANGLE_HEIGHT: f32 = 11.7;
const ICON_SCALE: f32 = 0.8;
const TEXT_LEFT_PADDING: f32 = 7.0;
const MAX_NOTIFICATIONS: usize = 15;
const STACK_GAP: f32 = 3.0;
const START_Y_OFFSET: f32 = 46.0;
const ANIMATION_MAX_TICK: i32 = 45;
const ANIMATION_STEP: Duration = Duration::from_millis(16);
const DESKTOP_BASE_SCALE: f32 = 2.0;

const ENABLED_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icon/noti/enabled.png"
));
const DISABLED_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icon/noti/disabled.png"
));
const DEBUG_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icon/noti/debug.png"
));
const INFO_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icon/noti/info.png"
));
const ERROR_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/icon/noti/error.png"
));

static GLOBAL_MANAGER: OnceLock<Mutex<NotificationManager>> = OnceLock::new();
static RENDERING_ENABLED: AtomicBool = AtomicBool::new(true);
static DESKTOP_FRAME_VISIBLE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static DESKTOP_PRESENTER: RefCell<DesktopNotificationPresenter> =
        RefCell::new(DesktopNotificationPresenter::new());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationType {
    Enabled,
    Disabled,
    Debug,
    Info,
    Error,
}

impl NotificationType {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Enabled | Self::Disabled => "Module",
            Self::Debug => "Debug",
            Self::Info => "Info",
            Self::Error => "ERROR",
        }
    }

    pub const fn action_name(self) -> Option<&'static str> {
        match self {
            Self::Enabled => Some("Enabled"),
            Self::Disabled => Some("Disabled"),
            Self::Debug | Self::Info | Self::Error => None,
        }
    }

    pub const fn is_module_state(self) -> bool {
        self.action_name().is_some()
    }

    const fn accent_rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Error => (205, 58, 58),
            Self::Debug | Self::Info => (59, 153, 222),
            Self::Enabled | Self::Disabled => (155, 89, 179),
        }
    }

    const fn status_rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Enabled => (0, 255, 0),
            Self::Disabled => (255, 0, 0),
            Self::Debug | Self::Info | Self::Error => (0, 0, 0),
        }
    }
}

#[derive(Debug)]
pub struct NotificationManager {
    notifications: Vec<NotificationEntry>,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            notifications: Vec::new(),
        }
    }

    pub fn publicity(
        &mut self,
        content: impl Into<String>,
        seconds: u64,
        notification_type: NotificationType,
    ) {
        let display_seconds = if notification_type.is_module_state() {
            2
        } else {
            seconds
        };

        self.notifications.push(NotificationEntry::new(
            content.into(),
            notification_type,
            Duration::from_millis(display_seconds.saturating_mul(1000)),
        ));

        if self.notifications.len() > MAX_NOTIFICATIONS {
            let overflow = self.notifications.len() - MAX_NOTIFICATIONS;
            self.notifications.drain(0..overflow);
        }
    }

    pub fn module_notification(&mut self, module_name: impl Into<String>, enabled: bool) {
        let notification_type = if enabled {
            NotificationType::Enabled
        } else {
            NotificationType::Disabled
        };
        self.publicity(module_name, 4, notification_type);
    }

    pub fn render(
        &mut self,
        canvas: &Canvas,
        renderer: &mut NotificationRenderer,
        screen_width: f32,
        screen_height: f32,
    ) -> bool {
        if !rendering_enabled() || screen_width <= 0.0 || screen_height <= 0.0 {
            return false;
        }

        self.notifications
            .retain(|notification| !notification.should_delete());
        if self.notifications.is_empty() {
            return false;
        }

        let mut drew_anything = false;
        let mut y = screen_height - START_Y_OFFSET;
        for notification in &mut self.notifications {
            drew_anything |= notification.render(canvas, renderer, screen_width, screen_height, y);
            y -= notification.height() + STACK_GAP;
        }
        self.notifications
            .retain(|notification| !notification.should_delete());

        drew_anything
    }

    pub fn clear(&mut self) {
        self.notifications.clear();
    }

    pub fn len(&self) -> usize {
        self.notifications.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

pub struct NotificationRenderer {
    title_typeface: Option<Typeface>,
    body_typeface: Option<Typeface>,
    icons: NotificationIcons,
}

impl NotificationRenderer {
    pub fn new() -> Self {
        Self {
            title_typeface: match_typeface(font_style::Weight::BLACK, &TITLE_FONT_FAMILIES),
            body_typeface: match_typeface(font_style::Weight::MEDIUM, &BODY_FONT_FAMILIES),
            icons: NotificationIcons::new(),
        }
    }

    fn font(&self, size: f32, role: TextRole) -> Font {
        let typeface = match role {
            TextRole::Title => self.title_typeface.as_ref().or(self.body_typeface.as_ref()),
            TextRole::Body => self.body_typeface.as_ref().or(self.title_typeface.as_ref()),
        };

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

    fn font_for_height(&self, pixel_height: f32, role: TextRole) -> Font {
        if pixel_height <= 0.0 {
            return self.font(1.0, role);
        }

        let unit_font = self.font(1.0, role);
        let unit_height = font_line_height(&unit_font).max(0.001);
        self.font(pixel_height / unit_height, role)
    }

    fn text_width(&self, text: &str, font: &Font, paint: &Paint) -> f32 {
        if text.is_empty() {
            0.0
        } else {
            font.measure_str(text, Some(paint)).0
        }
    }

    fn draw_text_top(
        &self,
        canvas: &Canvas,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        font: &Font,
        paint: &Paint,
    ) {
        if text.is_empty() || max_width <= 0.0 {
            return;
        }

        let height = font_line_height(font);
        let (_, metrics) = font.metrics();
        canvas.save();
        canvas.clip_rect(sk_rect(x, y, x + max_width, y + height + 2.0), None, true);
        canvas.draw_str(text, (x, y - metrics.ascent), font, paint);
        canvas.restore();
    }
}

impl Default for NotificationRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct NotificationEntry {
    message: String,
    created_at: Instant,
    notification_type: NotificationType,
    stay_time: Duration,
    pos_y: Option<f32>,
    width: f32,
    animation_x: f32,
    animation_speed: f32,
    animation: BetterAnimation,
    last_animation_step: Instant,
}

impl NotificationEntry {
    fn new(message: String, notification_type: NotificationType, stay_time: Duration) -> Self {
        let width = NOTIFICATION_WIDTH;
        let now = Instant::now();
        Self {
            message,
            created_at: now,
            notification_type,
            stay_time,
            pos_y: None,
            width,
            animation_x: width,
            animation_speed: 0.12,
            animation: BetterAnimation::new(ANIMATION_MAX_TICK),
            last_animation_step: now,
        }
    }

    fn render(
        &mut self,
        canvas: &Canvas,
        renderer: &mut NotificationRenderer,
        screen_width: f32,
        screen_height: f32,
        target_y: f32,
    ) -> bool {
        let current_y = self.pos_y.unwrap_or(screen_height - NOTIFICATION_HEIGHT);
        let pos_y = animate(current_y, target_y, self.animation_speed);
        self.pos_y = Some(pos_y);
        self.update_animation();

        let animation_value = self.animation.animation_value(self.last_animation_step);
        let alpha = animation_value.clamp(0.0, 1.0);
        self.animation_x = self.width * (1.0 - animation_value);
        if alpha <= 0.01 {
            return false;
        }

        let alpha_u8 = alpha_to_u8(255, alpha);
        let accent = color_from_rgb(self.notification_type.accent_rgb(), alpha_u8);
        let title_color = accent;
        let body_color = rgba(0, 0, 0, alpha_u8);
        let background = rgba(255, 255, 255, alpha_u8);

        let x = align_to_half_pixel((screen_width - 10.0) - self.width + self.animation_x);
        let y = align_to_half_pixel(pos_y);

        fill_rect(
            canvas,
            x + NOTIFICATION_HEIGHT,
            y,
            self.width - NOTIFICATION_HEIGHT,
            NOTIFICATION_HEIGHT,
            background,
        );
        self.draw_accent_mark(canvas, renderer, x, y, accent, alpha);
        self.draw_text(canvas, renderer, x, y, title_color, body_color, alpha_u8);

        true
    }

    fn draw_accent_mark(
        &self,
        canvas: &Canvas,
        renderer: &NotificationRenderer,
        x: f32,
        y: f32,
        accent: Color,
        alpha: f32,
    ) {
        fill_rect(
            canvas,
            x,
            y,
            NOTIFICATION_HEIGHT,
            NOTIFICATION_HEIGHT,
            accent,
        );
        self.draw_type_icon(canvas, renderer, x, y, NOTIFICATION_HEIGHT, alpha);

        let triangle_left = x + NOTIFICATION_HEIGHT;
        let center_y = y + NOTIFICATION_HEIGHT * 0.5;
        fill_triangle(
            canvas,
            (triangle_left, center_y - TRIANGLE_HEIGHT * 0.5),
            (triangle_left + TRIANGLE_WIDTH, center_y),
            (triangle_left, center_y + TRIANGLE_HEIGHT * 0.5),
            accent,
        );
    }

    fn draw_type_icon(
        &self,
        canvas: &Canvas,
        renderer: &NotificationRenderer,
        x: f32,
        y: f32,
        accent_size: f32,
        alpha: f32,
    ) {
        let icon_size = (accent_size * ICON_SCALE).round();
        let icon_x = (x + (accent_size - icon_size) * 0.5).round();
        let icon_y = (y + (accent_size - icon_size) * 0.5).round();

        if let Some(icon) = renderer.icons.icon(self.notification_type) {
            let mut paint = Paint::default();
            paint.set_anti_alias(true);
            paint.set_alpha_f(alpha.clamp(0.0, 1.0));
            canvas.draw_image_rect(
                icon,
                None,
                sk_rect(icon_x, icon_y, icon_x + icon_size, icon_y + icon_size),
                &paint,
            );
        } else {
            draw_fallback_icon(
                canvas,
                self.notification_type,
                icon_x,
                icon_y,
                icon_size,
                rgba(255, 255, 255, alpha_to_u8(255, alpha)),
            );
        }
    }

    fn draw_text(
        &self,
        canvas: &Canvas,
        renderer: &NotificationRenderer,
        x: f32,
        y: f32,
        title_color: Color,
        body_color: Color,
        alpha: u8,
    ) {
        let title_font = renderer.font_for_height(TITLE_TEXT_HEIGHT, TextRole::Title);
        let body_font = renderer.font_for_height(BODY_TEXT_HEIGHT, TextRole::Body);
        let title_height = font_line_height(&title_font);
        let body_height = font_line_height(&body_font);
        let text_x = x + NOTIFICATION_HEIGHT + TRIANGLE_WIDTH + TEXT_LEFT_PADDING;
        let half_height = NOTIFICATION_HEIGHT * 0.5;
        let title_y = y + (half_height - title_height) * 0.5;
        let body_y = y + half_height + (half_height - body_height) * 0.5;
        let max_width = self.max_text_width();

        let mut title_paint = Paint::default();
        title_paint.set_anti_alias(true);
        title_paint.set_color(title_color);
        renderer.draw_text_top(
            canvas,
            self.notification_type.title(),
            text_x,
            title_y,
            max_width,
            &title_font,
            &title_paint,
        );

        let mut body_paint = Paint::default();
        body_paint.set_anti_alias(true);
        body_paint.set_color(body_color);

        if let Some(action_name) = self.notification_type.action_name() {
            let status_width =
                renderer.text_width(&format!(" {action_name}"), &body_font, &body_paint);
            let module_name = truncate_to_width(
                renderer,
                &self.message,
                (max_width - status_width).max(0.0),
                &body_font,
                &body_paint,
            );
            renderer.draw_text_top(
                canvas,
                &module_name,
                text_x,
                body_y,
                max_width,
                &body_font,
                &body_paint,
            );

            let status_x =
                text_x + renderer.text_width(&format!("{module_name} "), &body_font, &body_paint);
            let mut status_paint = Paint::default();
            status_paint.set_anti_alias(true);
            status_paint.set_color(color_from_rgb(self.notification_type.status_rgb(), alpha));
            renderer.draw_text_top(
                canvas,
                action_name,
                status_x,
                body_y,
                (max_width - (status_x - text_x)).max(0.0),
                &body_font,
                &status_paint,
            );
        } else {
            let body =
                truncate_to_width(renderer, &self.message, max_width, &body_font, &body_paint);
            renderer.draw_text_top(
                canvas,
                &body,
                text_x,
                body_y,
                max_width,
                &body_font,
                &body_paint,
            );
        }
    }

    fn update_animation(&mut self) {
        if self.animation.popping {
            let steps = animation_steps(self.last_animation_step.elapsed());
            if steps > 0 {
                for _ in 0..steps {
                    self.animation.update(true);
                    if self.animation.tick >= self.animation.max_tick {
                        break;
                    }
                }
                self.last_animation_step = Instant::now();
            }
            if self.animation.tick >= self.animation.max_tick {
                self.animation.popping = false;
            }
        } else if self.is_finished() {
            let steps = animation_steps(self.last_animation_step.elapsed());
            if steps > 0 {
                for _ in 0..steps {
                    self.animation.update(false);
                    if self.animation.tick <= 0 {
                        break;
                    }
                }
                self.last_animation_step = Instant::now();
            }
        }
    }

    fn is_finished(&self) -> bool {
        self.created_at.elapsed() >= self.stay_time
    }

    fn height(&self) -> f32 {
        NOTIFICATION_HEIGHT
    }

    fn should_delete(&self) -> bool {
        self.is_finished() && self.animation_x >= self.width - 5.0
    }

    fn max_text_width(&self) -> f32 {
        self.width - NOTIFICATION_HEIGHT - TRIANGLE_WIDTH - TEXT_LEFT_PADDING - RIGHT_PADDING
    }
}

#[derive(Debug)]
struct BetterAnimation {
    max_tick: i32,
    prev_tick: i32,
    tick: i32,
    popping: bool,
}

impl BetterAnimation {
    fn new(max_tick: i32) -> Self {
        Self {
            max_tick,
            prev_tick: 0,
            tick: 0,
            popping: true,
        }
    }

    fn update(&mut self, forward: bool) {
        self.prev_tick = self.tick;
        self.tick = (self.tick + if forward { 1 } else { -1 }).clamp(0, self.max_tick);
    }

    fn animation_value(&self, last_step: Instant) -> f32 {
        if self.tick <= 0 {
            return 0.0;
        }
        if self.tick >= self.max_tick {
            return 1.0;
        }

        let frame_time =
            (last_step.elapsed().as_secs_f32() / ANIMATION_STEP.as_secs_f32()).clamp(0.0, 1.0);
        let t = (self.prev_tick as f32 + (self.tick - self.prev_tick) as f32 * frame_time)
            / self.max_tick as f32;
        if self.popping {
            1.0 - (1.0 - t).powi(3)
        } else {
            t.powi(3)
        }
    }
}

#[derive(Clone, Copy)]
enum TextRole {
    Title,
    Body,
}

struct NotificationIcons {
    enabled: Option<Image>,
    disabled: Option<Image>,
    debug: Option<Image>,
    info: Option<Image>,
    error: Option<Image>,
}

impl NotificationIcons {
    fn new() -> Self {
        Self {
            enabled: decode_image(ENABLED_ICON),
            disabled: decode_image(DISABLED_ICON),
            debug: decode_image(DEBUG_ICON),
            info: decode_image(INFO_ICON),
            error: decode_image(ERROR_ICON),
        }
    }

    fn icon(&self, notification_type: NotificationType) -> Option<&Image> {
        match notification_type {
            NotificationType::Enabled => self.enabled.as_ref(),
            NotificationType::Disabled => self.disabled.as_ref(),
            NotificationType::Debug => self.debug.as_ref(),
            NotificationType::Info => self.info.as_ref(),
            NotificationType::Error => self.error.as_ref(),
        }
    }
}

pub fn manager() -> &'static Mutex<NotificationManager> {
    GLOBAL_MANAGER.get_or_init(|| Mutex::new(NotificationManager::new()))
}

pub fn publicity(content: impl Into<String>, seconds: u64, notification_type: NotificationType) {
    if let Ok(mut manager) = manager().lock() {
        manager.publicity(content, seconds, notification_type);
    }
}

pub fn module_notification(module_name: impl Into<String>, enabled: bool) {
    if let Ok(mut manager) = manager().lock() {
        manager.module_notification(module_name, enabled);
    }
}

pub fn render(
    canvas: &Canvas,
    renderer: &mut NotificationRenderer,
    screen_width: f32,
    screen_height: f32,
) -> bool {
    manager()
        .lock()
        .is_ok_and(|mut manager| manager.render(canvas, renderer, screen_width, screen_height))
}

pub fn has_visible_notifications() -> bool {
    rendering_enabled() && manager().lock().is_ok_and(|manager| !manager.is_empty())
}

pub fn render_to_windows_desktop(viewport: DesktopViewport) -> bool {
    DESKTOP_PRESENTER.with(|presenter| presenter.borrow_mut().render_frame(viewport))
}

pub fn render_to_current_windows_desktop() -> bool {
    render_to_windows_desktop(DesktopViewport::current_primary_screen())
}

pub fn restore_windows_desktop_notifications() -> bool {
    DESKTOP_PRESENTER.with(|presenter| presenter.borrow_mut().restore_previous())
}

pub fn has_windows_desktop_frame() -> bool {
    DESKTOP_FRAME_VISIBLE.load(Ordering::Acquire)
}

pub fn set_rendering_enabled(enabled: bool) {
    RENDERING_ENABLED.store(enabled, Ordering::Release);
}

pub fn rendering_enabled() -> bool {
    RENDERING_ENABLED.load(Ordering::Acquire)
}

const TITLE_FONT_FAMILIES: [&str; 8] = [
    "JetBrainsMono Nerd Font",
    "JetBrains Mono ExtraBold",
    "JetBrains Mono",
    "CaskaydiaCove Nerd Font",
    "Maple Mono",
    "Microsoft YaHei UI",
    "Segoe UI",
    "Consolas",
];

const BODY_FONT_FAMILIES: [&str; 8] = [
    "Maple Mono",
    "Maple Mono NF",
    "JetBrains Mono",
    "JetBrainsMono Nerd Font",
    "CaskaydiaCove Nerd Font",
    "Microsoft YaHei UI",
    "Segoe UI",
    "Consolas",
];

fn match_typeface(weight: font_style::Weight, families: &[&str]) -> Option<Typeface> {
    let font_mgr = FontMgr::new();
    let style = FontStyle::new(
        weight,
        font_style::Width::NORMAL,
        font_style::Slant::Upright,
    );
    families
        .iter()
        .find_map(|family| font_mgr.match_family_style(family, style))
}

fn decode_image(bytes: &[u8]) -> Option<Image> {
    Image::from_encoded(Data::new_copy(bytes))
}

fn truncate_to_width(
    renderer: &NotificationRenderer,
    text: &str,
    max_width: f32,
    font: &Font,
    paint: &Paint,
) -> String {
    if text.is_empty() || max_width <= 0.0 || renderer.text_width(text, font, paint) <= max_width {
        return text.to_owned();
    }

    let ellipsis = "...";
    let ellipsis_width = renderer.text_width(ellipsis, font, paint);
    if ellipsis_width > max_width {
        return String::new();
    }

    let mut clipped = text.to_owned();
    while !clipped.is_empty()
        && renderer.text_width(&format!("{clipped}{ellipsis}"), font, paint) > max_width
    {
        clipped.pop();
    }
    format!("{clipped}{ellipsis}")
}

fn fill_rect(canvas: &Canvas, x: f32, y: f32, width: f32, height: f32, color: Color) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let mut paint = Paint::default();
    paint.set_anti_alias(false);
    paint.set_style(PaintStyle::Fill);
    paint.set_color(color);
    canvas.draw_rect(sk_rect(x, y, x + width, y + height), &paint);
}

fn fill_triangle(
    canvas: &Canvas,
    first: (f32, f32),
    second: (f32, f32),
    third: (f32, f32),
    color: Color,
) {
    let positions = [
        Point::new(first.0, first.1),
        Point::new(second.0, second.1),
        Point::new(third.0, third.1),
    ];
    let texs = [Point::new(0.0, 0.0); 3];
    let colors = [color; 3];
    let vertices = Vertices::new_copy(VertexMode::Triangles, &positions, &texs, &colors, None);
    let mut paint = Paint::default();
    paint.set_anti_alias(false);
    paint.set_style(PaintStyle::Fill);
    paint.set_color(color);
    canvas.draw_vertices(&vertices, BlendMode::SrcOver, &paint);
}

fn draw_fallback_icon(
    canvas: &Canvas,
    notification_type: NotificationType,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width((size * 0.1).max(1.0));
    paint.set_color(color);

    let cx = x + size * 0.5;
    let cy = y + size * 0.5;
    let radius = size * 0.34;
    match notification_type {
        NotificationType::Enabled => {
            canvas.draw_line(
                (x + size * 0.25, cy),
                (x + size * 0.43, y + size * 0.68),
                &paint,
            );
            canvas.draw_line(
                (x + size * 0.43, y + size * 0.68),
                (x + size * 0.76, y + size * 0.32),
                &paint,
            );
        }
        NotificationType::Disabled | NotificationType::Error => {
            canvas.draw_line(
                (x + size * 0.3, y + size * 0.3),
                (x + size * 0.7, y + size * 0.7),
                &paint,
            );
            canvas.draw_line(
                (x + size * 0.7, y + size * 0.3),
                (x + size * 0.3, y + size * 0.7),
                &paint,
            );
        }
        NotificationType::Debug => {
            canvas.draw_circle((cx, cy), radius, &paint);
            canvas.draw_line((cx, y + size * 0.28), (cx, y + size * 0.5), &paint);
            canvas.draw_line((cx, y + size * 0.62), (cx, y + size * 0.72), &paint);
        }
        NotificationType::Info => {
            canvas.draw_circle((cx, cy), radius, &paint);
            canvas.draw_line((cx, y + size * 0.45), (cx, y + size * 0.7), &paint);
            canvas.draw_circle((cx, y + size * 0.3), size * 0.025, &paint);
        }
    }
}

fn font_line_height(font: &Font) -> f32 {
    let (_, metrics) = font.metrics();
    (metrics.descent - metrics.ascent).max(1.0)
}

fn sk_rect(left: f32, top: f32, right: f32, bottom: f32) -> SkRect {
    SkRect::new(left, top, right, bottom)
}

fn animate(value: f32, target: f32, speed: f32) -> f32 {
    value + (target - value) * speed
}

fn align_to_half_pixel(value: f32) -> f32 {
    value.floor() + 0.5
}

fn color_from_rgb((r, g, b): (u8, u8, u8), alpha: u8) -> Color {
    rgba(r, g, b, alpha)
}

fn alpha_to_u8(base_alpha: u8, alpha: f32) -> u8 {
    ((base_alpha as f32 * alpha.clamp(0.0, 1.0)).round() as i32).clamp(0, 255) as u8
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_argb(a, r, g, b)
}

fn animation_steps(elapsed: Duration) -> u32 {
    (elapsed.as_millis() / ANIMATION_STEP.as_millis())
        .min(8)
        .try_into()
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopViewport {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl DesktopViewport {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn current_virtual_screen() -> Self {
        Self {
            x: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
            y: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
            width: unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1),
            height: unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1),
        }
    }

    pub fn current_primary_screen() -> Self {
        let (width, height) = primary_screen_physical_size();
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    const fn rect(self) -> DesktopRect {
        DesktopRect::new(self.x, self.y, self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DesktopRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl DesktopRect {
    const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    const fn right(self) -> i32 {
        self.x + self.width
    }

    const fn bottom(self) -> i32 {
        self.y + self.height
    }

    const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    fn union(self, other: Self) -> Self {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(left, top, right - left, bottom - top)
    }

    fn intersect(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= left || bottom <= top {
            None
        } else {
            Some(Self::new(left, top, right - left, bottom - top))
        }
    }
}

struct DesktopFrame {
    rect: DesktopRect,
    background_pixels: Vec<u8>,
    dirty_mask: Vec<u8>,
}

struct DesktopNotificationPresenter {
    renderer: NotificationRenderer,
    previous_frame: Option<DesktopFrame>,
}

impl DesktopNotificationPresenter {
    fn new() -> Self {
        Self {
            renderer: NotificationRenderer::new(),
            previous_frame: None,
        }
    }

    fn render_frame(&mut self, viewport: DesktopViewport) -> bool {
        if viewport.is_empty() || !rendering_enabled() {
            return self.restore_previous();
        }

        let scale = desktop_notification_scale();
        debug_desktop_render(viewport, scale);
        let count = manager().lock().map(|manager| manager.len()).unwrap_or(0);
        let Some(current_rect) = notification_desktop_rect(viewport, count, scale) else {
            return self.restore_previous();
        };

        let previous_rect = self.previous_frame.as_ref().map(|frame| frame.rect);
        let mut render_rect = previous_rect
            .map(|previous| previous.union(current_rect))
            .unwrap_or(current_rect);
        let Some(clipped_rect) = render_rect.intersect(viewport.rect()) else {
            return self.restore_previous();
        };
        render_rect = clipped_rect;

        let Some(mut background_pixels) = capture_screen_rect(render_rect) else {
            return false;
        };
        if let Some(previous_frame) = self.previous_frame.as_ref() {
            restore_dirty_pixels(&mut background_pixels, render_rect, previous_frame);
        }

        let mut frame_pixels = background_pixels.clone();
        let row_bytes = render_rect.width as usize * 4;
        let image_info = ImageInfo::new_n32(
            (render_rect.width, render_rect.height),
            AlphaType::Premul,
            None,
        );

        let drew = {
            let Some(mut surface) =
                surfaces::wrap_pixels(&image_info, &mut frame_pixels, Some(row_bytes), None)
            else {
                return false;
            };
            let canvas = surface.canvas();
            canvas.save();
            canvas.translate((
                (viewport.x - render_rect.x) as f32,
                (viewport.y - render_rect.y) as f32,
            ));
            canvas.scale((scale, scale));
            let drew = render(
                canvas,
                &mut self.renderer,
                viewport.width as f32 / scale,
                viewport.height as f32 / scale,
            );
            canvas.restore();
            drew
        };

        if !drew {
            if self.previous_frame.is_some() {
                if blit_pixels_to_screen(render_rect, &background_pixels) {
                    self.previous_frame = None;
                    DESKTOP_FRAME_VISIBLE.store(false, Ordering::Release);
                    return true;
                }
                return false;
            }

            DESKTOP_FRAME_VISIBLE.store(false, Ordering::Release);
            return false;
        }

        let dirty_mask = dirty_pixel_mask(&background_pixels, &frame_pixels);
        if !has_dirty_pixels(&dirty_mask) {
            if self.previous_frame.is_some() {
                if blit_pixels_to_screen(render_rect, &background_pixels) {
                    self.previous_frame = None;
                    DESKTOP_FRAME_VISIBLE.store(false, Ordering::Release);
                    return true;
                }
                return false;
            }

            DESKTOP_FRAME_VISIBLE.store(false, Ordering::Release);
            return false;
        }

        if blit_pixels_to_screen(render_rect, &frame_pixels) {
            self.previous_frame = Some(DesktopFrame {
                rect: render_rect,
                background_pixels,
                dirty_mask,
            });
            DESKTOP_FRAME_VISIBLE.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn restore_previous(&mut self) -> bool {
        let Some(previous_frame) = self.previous_frame.take() else {
            DESKTOP_FRAME_VISIBLE.store(false, Ordering::Release);
            return false;
        };

        let mut restore_pixels = capture_screen_rect(previous_frame.rect)
            .unwrap_or_else(|| previous_frame.background_pixels.clone());
        restore_dirty_pixels(&mut restore_pixels, previous_frame.rect, &previous_frame);

        if blit_pixels_to_screen(previous_frame.rect, &restore_pixels) {
            DESKTOP_FRAME_VISIBLE.store(false, Ordering::Release);
            true
        } else {
            self.previous_frame = Some(previous_frame);
            DESKTOP_FRAME_VISIBLE.store(true, Ordering::Release);
            false
        }
    }
}

fn dirty_pixel_mask(background_pixels: &[u8], frame_pixels: &[u8]) -> Vec<u8> {
    background_pixels
        .chunks_exact(4)
        .zip(frame_pixels.chunks_exact(4))
        .map(|(background, frame)| u8::from(background != frame))
        .collect()
}

fn has_dirty_pixels(mask: &[u8]) -> bool {
    mask.iter().any(|dirty| *dirty != 0)
}

fn restore_dirty_pixels(
    dest_pixels: &mut [u8],
    dest_rect: DesktopRect,
    previous_frame: &DesktopFrame,
) {
    let Some(overlap) = dest_rect.intersect(previous_frame.rect) else {
        return;
    };

    let dest_width = dest_rect.width as usize;
    let previous_width = previous_frame.rect.width as usize;
    let required_dest_len = dest_width
        .saturating_mul(dest_rect.height as usize)
        .saturating_mul(4);
    let required_previous_len = previous_width.saturating_mul(previous_frame.rect.height as usize);
    if dest_pixels.len() < required_dest_len
        || previous_frame.background_pixels.len() < required_previous_len.saturating_mul(4)
        || previous_frame.dirty_mask.len() < required_previous_len
    {
        return;
    }

    let overlap_width = overlap.width as usize;
    let overlap_height = overlap.height as usize;
    let dest_start_x = (overlap.x - dest_rect.x) as usize;
    let dest_start_y = (overlap.y - dest_rect.y) as usize;
    let previous_start_x = (overlap.x - previous_frame.rect.x) as usize;
    let previous_start_y = (overlap.y - previous_frame.rect.y) as usize;

    for row in 0..overlap_height {
        let dest_row_start = (dest_start_y + row) * dest_width + dest_start_x;
        let previous_row_start = (previous_start_y + row) * previous_width + previous_start_x;

        for column in 0..overlap_width {
            let previous_pixel = previous_row_start + column;
            if previous_frame.dirty_mask[previous_pixel] == 0 {
                continue;
            }

            let previous_byte = previous_pixel * 4;
            let dest_byte = (dest_row_start + column) * 4;
            dest_pixels[dest_byte..dest_byte + 4].copy_from_slice(
                &previous_frame.background_pixels[previous_byte..previous_byte + 4],
            );
        }
    }
}

fn notification_desktop_rect(
    viewport: DesktopViewport,
    count: usize,
    scale: f32,
) -> Option<DesktopRect> {
    if count == 0 || viewport.is_empty() {
        return None;
    }

    let scale = scale.max(0.1);
    let visible_count = count.min(MAX_NOTIFICATIONS) as f32;
    let stack_height =
        NOTIFICATION_HEIGHT + (visible_count - 1.0).max(0.0) * (NOTIFICATION_HEIGHT + STACK_GAP);
    let margin = 6.0;
    let right = viewport.x + viewport.width;
    let bottom = viewport.y + viewport.height;
    let physical_width = ((NOTIFICATION_WIDTH + RIGHT_PADDING + margin) * scale).ceil() as i32;
    let physical_height = ((START_Y_OFFSET + stack_height + margin) * scale).ceil() as i32;
    let left = right - physical_width.max(1);
    let top = bottom - physical_height.max(1);

    DesktopRect::new(left, top, right - left, bottom - top)
        .intersect(viewport.rect())
        .filter(|rect| !rect.is_empty())
}

fn capture_screen_rect(rect: DesktopRect) -> Option<Vec<u8>> {
    if rect.is_empty() {
        return None;
    }

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

    let bitmap_info = bitmap_info_for_rect(rect);
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
        .ok()?
    };

    let previous_object = unsafe { SelectObject(memory_dc, HGDIOBJ(bitmap.0)) };
    let capture_result = unsafe {
        BitBlt(
            memory_dc,
            0,
            0,
            rect.width,
            rect.height,
            Some(screen_dc),
            rect.x,
            rect.y,
            SRCCOPY,
        )
    };

    let pixel_len = rect.width as usize * rect.height as usize * 4;
    let mut pixels = vec![0_u8; pixel_len];
    if capture_result.is_ok() && !bits.is_null() {
        unsafe {
            ptr::copy_nonoverlapping(bits.cast::<u8>(), pixels.as_mut_ptr(), pixel_len);
        }
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }

    unsafe {
        let _ = SelectObject(memory_dc, previous_object);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);
        let _ = ReleaseDC(None, screen_dc);
    }

    capture_result.ok().map(|()| pixels)
}

fn blit_pixels_to_screen(rect: DesktopRect, pixels: &[u8]) -> bool {
    if rect.is_empty() || pixels.len() < rect.width as usize * rect.height as usize * 4 {
        return false;
    }

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.0.is_null() {
        return false;
    }

    let bitmap_info = bitmap_info_for_rect(rect);
    let result = unsafe {
        SetDIBitsToDevice(
            screen_dc,
            rect.x,
            rect.y,
            rect.width as u32,
            rect.height as u32,
            0,
            0,
            0,
            rect.height as u32,
            pixels.as_ptr().cast::<c_void>(),
            &bitmap_info,
            DIB_RGB_COLORS,
        )
    };

    unsafe {
        let _ = ReleaseDC(None, screen_dc);
    }

    result != 0
}

fn bitmap_info_for_rect(rect: DesktopRect) -> BITMAPINFO {
    let mut bitmap_info = BITMAPINFO::default();
    bitmap_info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: rect.width,
        biHeight: -rect.height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: (rect.width as usize * rect.height as usize * 4) as u32,
        ..Default::default()
    };
    bitmap_info
}

fn desktop_notification_scale() -> f32 {
    let configured_scale = std::env::var("NYX_NOTIFICATION_SCALE")
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value > 0.0);
    configured_scale.unwrap_or_else(|| (DESKTOP_BASE_SCALE * desktop_dpi_scale()).clamp(1.0, 4.0))
}

fn desktop_dpi_scale() -> f32 {
    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.0.is_null() {
        return 1.0;
    }
    let dpi_x = unsafe { GetDeviceCaps(Some(screen_dc), LOGPIXELSX) };
    unsafe {
        let _ = ReleaseDC(None, screen_dc);
    }
    if dpi_x > 0 { dpi_x as f32 / 96.0 } else { 1.0 }
}

fn primary_screen_physical_size() -> (i32, i32) {
    (
        unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1),
        unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1),
    )
}

fn debug_desktop_render(viewport: DesktopViewport, scale: f32) {
    if std::env::var_os("NYX_NOTIFICATION_DEBUG").is_none() {
        return;
    }

    eprintln!(
        "Notification desktop viewport: x={}, y={}, width={}, height={}, scale={:.2}",
        viewport.x, viewport.y, viewport.width, viewport.height, scale
    );
}

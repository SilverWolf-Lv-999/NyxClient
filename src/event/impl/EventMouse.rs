#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Pressed(MouseButton),
    Released(MouseButton),
    Wheel { delta: i16 },
    HorizontalWheel { delta: i16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMouse {
    pub x: i32,
    pub y: i32,
    pub action: MouseAction,
    pub mouse_data: u32,
    pub flags: u32,
    pub time: u32,
    pub extra_info: usize,
    pub injected: bool,
    pub lower_integrity_injected: bool,
    cancelled: bool,
}

impl EventMouse {
    pub const fn new(
        x: i32,
        y: i32,
        action: MouseAction,
        mouse_data: u32,
        flags: u32,
        time: u32,
        extra_info: usize,
        injected: bool,
        lower_integrity_injected: bool,
    ) -> Self {
        Self {
            x,
            y,
            action,
            mouse_data,
            flags,
            time,
            extra_info,
            injected,
            lower_integrity_injected,
            cancelled: false,
        }
    }

    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.cancelled = cancelled;
    }
}

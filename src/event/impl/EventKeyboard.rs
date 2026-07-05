#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
}

impl KeyState {
    pub const fn is_pressed(self) -> bool {
        matches!(self, Self::Pressed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

impl KeyModifiers {
    pub const fn bitmask(self) -> u8 {
        (self.shift as u8) | ((self.control as u8) << 1) | ((self.alt as u8) << 2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventKeyboard {
    pub vk_code: u32,
    pub scan_code: u32,
    pub state: KeyState,
    pub modifiers: KeyModifiers,
    pub flags: u32,
    pub time: u32,
    pub extra_info: usize,
    pub injected: bool,
    pub lower_integrity_injected: bool,
    cancelled: bool,
}

impl EventKeyboard {
    pub const fn new(
        vk_code: u32,
        scan_code: u32,
        state: KeyState,
        modifiers: KeyModifiers,
        flags: u32,
        time: u32,
        extra_info: usize,
        injected: bool,
        lower_integrity_injected: bool,
    ) -> Self {
        Self {
            vk_code,
            scan_code,
            state,
            modifiers,
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

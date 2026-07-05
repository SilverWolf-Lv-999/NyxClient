use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsSessionAction {
    QueryEndSession,
    EndSession,
    Logoff,
    Shutdown,
    ConsoleClose,
    ConsoleBreak,
    ConsoleCtrlC,
    Suspend,
    Resume,
    PowerStatusChanged,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventWindows {
    pub action: WindowsSessionAction,
    pub raw_message: Option<u32>,
    pub raw_wparam: usize,
    pub raw_lparam: isize,
    pub timestamp: SystemTime,
    cancelled: bool,
}

impl EventWindows {
    pub fn shutdown() -> Self {
        Self::new(WindowsSessionAction::Shutdown, None, 0, 0)
    }

    pub fn query_end_session(raw_lparam: isize) -> Self {
        Self::new(WindowsSessionAction::QueryEndSession, None, 0, raw_lparam)
    }

    pub fn end_session(raw_wparam: usize, raw_lparam: isize) -> Self {
        Self::new(
            WindowsSessionAction::EndSession,
            None,
            raw_wparam,
            raw_lparam,
        )
    }

    pub fn console(action: WindowsSessionAction, raw_code: u32) -> Self {
        Self::new(action, Some(raw_code), raw_code as usize, 0)
    }

    pub fn raw(action: WindowsSessionAction, message: u32, wparam: usize, lparam: isize) -> Self {
        Self::new(action, Some(message), wparam, lparam)
    }

    pub fn new(
        action: WindowsSessionAction,
        raw_message: Option<u32>,
        raw_wparam: usize,
        raw_lparam: isize,
    ) -> Self {
        Self {
            action,
            raw_message,
            raw_wparam,
            raw_lparam,
            timestamp: SystemTime::now(),
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

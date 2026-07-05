use crate::{
    manager::notification_manager,
    modules::{Category, Module, ModuleInfo, ModuleState},
};

const MODULE_NAME: &str = "Notification";

#[derive(Debug)]
pub struct Notification {
    info: ModuleInfo,
    state: ModuleState,
}

impl Notification {
    pub fn new() -> Self {
        let mut module = Self {
            info: ModuleInfo::new(
                MODULE_NAME,
                "Draws the Skia notification stack on an existing render canvas.",
                Category::Visual,
            ),
            state: ModuleState::new(),
        };
        let _ = Module::set_enabled(&mut module, true);
        module
    }
}

impl Default for Notification {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Notification {
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
        notification_manager::set_rendering_enabled(true);
    }

    fn on_disable(&mut self) {
        notification_manager::set_rendering_enabled(false);
    }

    fn should_notify_toggle(&self) -> bool {
        false
    }
}

use crate::modules::{Category, Module, ModuleInfo, ModuleState};

#[derive(Debug)]
pub struct Fun {
    info: ModuleInfo,
    state: ModuleState,
}

impl Fun {
    pub const fn new() -> Self {
        Self {
            info: ModuleInfo::new("Fun", "Base module for other features.", Category::Other),
            state: ModuleState::new(),
        }
    }
}

impl Default for Fun {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Fun {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn state(&self) -> &ModuleState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut ModuleState {
        &mut self.state
    }
}

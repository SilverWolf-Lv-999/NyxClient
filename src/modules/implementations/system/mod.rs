pub mod click_gui;
pub mod stage_manager;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(click_gui::ClickGui::default());
    handler.register(stage_manager::StageManager::default());
}

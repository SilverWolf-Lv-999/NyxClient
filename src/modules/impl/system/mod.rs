#[path = "ClickGui.rs"]
pub mod click_gui;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(click_gui::ClickGui::default());
}

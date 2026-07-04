#[path = "Fun.rs"]
pub mod fun;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(fun::Fun::default());
}

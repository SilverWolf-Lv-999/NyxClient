pub mod combat;
pub mod other;
pub mod player;
pub mod system;
pub mod visual;

use crate::modules::ModuleHandler;

pub fn register_builtin_modules(handler: &mut ModuleHandler) {
    combat::register_modules(handler);
    other::register_modules(handler);
    player::register_modules(handler);
    system::register_modules(handler);
    visual::register_modules(handler);
}

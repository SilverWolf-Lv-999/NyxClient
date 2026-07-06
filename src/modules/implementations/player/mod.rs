pub mod better_touchpad;
mod bilbibili_view;
pub mod fast_close;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(better_touchpad::BetterTouchpad::default());
    handler.register(fast_close::FastClose::default());
}

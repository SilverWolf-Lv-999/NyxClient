mod bilbibili_view;
pub mod fast_close;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(fast_close::FastClose::default());
}

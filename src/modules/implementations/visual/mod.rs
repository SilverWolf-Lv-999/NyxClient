pub mod live2d;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(live2d::Live2D::default());
}

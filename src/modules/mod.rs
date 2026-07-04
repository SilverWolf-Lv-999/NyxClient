#[path = "Module.rs"]
pub mod module;

#[path = "impl/mod.rs"]
pub mod implementations;

pub use implementations::register_builtin_modules;
pub use module::{
    Category, ConfigItem, ConfigType, Module, ModuleHandler, ModuleInfo, ModuleState, ToggleResult,
};

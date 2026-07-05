mod kill_aura;

use crate::modules::ModuleHandler;

pub fn register_modules(handler: &mut ModuleHandler) {
    handler.register(kill_aura::KillAura::default());
}

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Combat,
    Other,
    Player,
    System,
    Visual,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Combat,
        Self::Other,
        Self::Player,
        Self::System,
        Self::Visual,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Combat => "combat",
            Self::Other => "other",
            Self::Player => "player",
            Self::System => "system",
            Self::Visual => "visual",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Combat => "Combat",
            Self::Other => "Other",
            Self::Player => "Player",
            Self::System => "System",
            Self::Visual => "Visual",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub category: Category,
}

impl ModuleInfo {
    pub const fn new(name: &'static str, description: &'static str, category: Category) -> Self {
        Self {
            name,
            description,
            category,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModuleState {
    enabled: bool,
    key_bind: Option<u32>,
    config_saving: bool,
}

impl ModuleState {
    pub const fn new() -> Self {
        Self {
            enabled: false,
            key_bind: None,
            config_saving: true,
        }
    }

    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub const fn key_bind(&self) -> Option<u32> {
        self.key_bind
    }

    pub fn set_key_bind(&mut self, key_bind: Option<u32>) {
        self.key_bind = key_bind;
    }

    pub const fn config_saving(&self) -> bool {
        self.config_saving
    }

    pub fn set_config_saving(&mut self, config_saving: bool) {
        self.config_saving = config_saving;
    }
}

impl Default for ModuleState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigType {
    Boolean,
    Integer,
    Float,
    Text,
    Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigItem {
    pub key: &'static str,
    pub kind: ConfigType,
    pub default_value: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub choices: &'static [&'static str],
}

impl ConfigItem {
    pub const fn new(
        key: &'static str,
        kind: ConfigType,
        default_value: &'static str,
        display_name: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            key,
            kind,
            default_value,
            display_name,
            description,
            choices: &[],
        }
    }

    pub const fn choice(
        key: &'static str,
        default_value: &'static str,
        display_name: &'static str,
        description: &'static str,
        choices: &'static [&'static str],
    ) -> Self {
        Self {
            key,
            kind: ConfigType::Choice,
            default_value,
            display_name,
            description,
            choices,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleResult {
    Changed {
        enabled: bool,
        notify: bool,
        save_config: bool,
    },
    Unchanged,
}

impl ToggleResult {
    pub const fn changed(self) -> bool {
        matches!(self, Self::Changed { .. })
    }
}

pub trait Module: Send {
    fn info(&self) -> &ModuleInfo;
    fn state(&self) -> &ModuleState;
    fn state_mut(&mut self) -> &mut ModuleState;

    fn config_items(&self) -> &[ConfigItem] {
        &[]
    }

    fn config_aliases(&self) -> &[&'static str] {
        &[]
    }

    fn normalize_config_value(&self, _key: &str, value: &str) -> String {
        value.to_owned()
    }

    fn on_enable(&mut self) {}

    fn on_disable(&mut self) {}

    fn should_notify_toggle(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        self.info().name
    }

    fn description(&self) -> &'static str {
        self.info().description
    }

    fn category(&self) -> Category {
        self.info().category
    }

    fn is_enabled(&self) -> bool {
        self.state().is_enabled()
    }

    fn set_enabled(&mut self, enabled: bool) -> ToggleResult {
        if self.is_enabled() == enabled {
            return ToggleResult::Unchanged;
        }

        self.state_mut().enabled = enabled;
        if enabled {
            self.on_enable();
        } else {
            self.on_disable();
        }

        ToggleResult::Changed {
            enabled,
            notify: self.should_notify_toggle(),
            save_config: self.state().config_saving(),
        }
    }

    fn toggle(&mut self) -> ToggleResult {
        self.set_enabled(!self.is_enabled())
    }
}

#[derive(Default)]
pub struct ModuleHandler {
    modules: Vec<Box<dyn Module>>,
}

impl ModuleHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin_modules() -> Self {
        let mut handler = Self::new();
        crate::modules::register_builtin_modules(&mut handler);
        handler
    }

    pub fn register<M>(&mut self, module: M)
    where
        M: Module + 'static,
    {
        self.modules.push(Box::new(module));
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(dyn Module + 'static)> + '_ {
        self.modules.iter().map(Box::as_ref)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut (dyn Module + 'static)> + '_ {
        self.modules.iter_mut().map(Box::as_mut)
    }

    pub fn by_category(
        &self,
        category: Category,
    ) -> impl Iterator<Item = &(dyn Module + 'static)> + '_ {
        self.iter()
            .filter(move |module| module.category() == category)
    }

    pub fn get(&self, name: &str) -> Option<&(dyn Module + 'static)> {
        self.iter()
            .find(|module| module.name().eq_ignore_ascii_case(name))
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut (dyn Module + 'static)> {
        self.iter_mut()
            .find(|module| module.name().eq_ignore_ascii_case(name))
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Option<ToggleResult> {
        self.get_mut(name).map(|module| module.set_enabled(enabled))
    }

    pub fn toggle(&mut self, name: &str) -> Option<ToggleResult> {
        self.get_mut(name).map(|module| module.toggle())
    }
}

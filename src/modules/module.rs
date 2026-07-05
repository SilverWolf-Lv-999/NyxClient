use std::{fmt, fs, io, path::PathBuf};

use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};

use crate::modules::value::{BaseValue, config_key};

pub const DEFAULT_CONFIG_NAME: &str = "default";

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

    fn values(&self) -> &[BaseValue] {
        &[]
    }

    fn values_mut(&mut self) -> &mut [BaseValue] {
        &mut []
    }

    fn main_value(&self) -> Option<&BaseValue> {
        None
    }

    fn value(&self, key: &str) -> Option<&BaseValue> {
        let normalized_key = config_key(key);
        self.values().iter().find(|value| {
            value.config_key().eq_ignore_ascii_case(&normalized_key)
                || value.name().eq_ignore_ascii_case(key)
        })
    }

    fn value_mut(&mut self, key: &str) -> Option<&mut BaseValue> {
        let normalized_key = config_key(key);
        self.values_mut().iter_mut().find(|value| {
            value.config_key().eq_ignore_ascii_case(&normalized_key)
                || value.name().eq_ignore_ascii_case(key)
        })
    }

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

    pub fn config_dir() -> PathBuf {
        roaming_app_data_dir().join(".nyx_client").join("config")
    }

    pub fn config_file(config_name: &str) -> PathBuf {
        let config_name = normalized_config_name(config_name);
        Self::config_dir().join(format!("{config_name}.json"))
    }

    pub fn default_config_file() -> PathBuf {
        Self::config_file(DEFAULT_CONFIG_NAME)
    }

    pub fn config_exists(config_name: &str) -> bool {
        Self::config_file(config_name).exists()
    }

    pub fn default_config_exists() -> bool {
        Self::config_exists(DEFAULT_CONFIG_NAME)
    }

    pub fn list_configs() -> io::Result<Vec<String>> {
        let config_dir = Self::config_dir();
        if !config_dir.exists() {
            return Ok(Vec::new());
        }

        let mut configs = Vec::new();
        for entry in fs::read_dir(config_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                configs.push(stem.to_owned());
            }
        }
        configs.sort();
        Ok(configs)
    }

    pub fn save_default_config(&self) -> io::Result<()> {
        self.save_config(DEFAULT_CONFIG_NAME)
    }

    pub fn load_default_config(&mut self) -> io::Result<()> {
        self.load_config(DEFAULT_CONFIG_NAME)
    }

    pub fn load_default_config_if_exists(&mut self) -> io::Result<bool> {
        self.load_config_if_exists(DEFAULT_CONFIG_NAME)
    }

    pub fn save_config(&self, config_name: &str) -> io::Result<()> {
        let config_file = Self::config_file(config_name);
        if let Some(parent) = config_file.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut modules_json = JsonMap::new();
        for module in self.iter() {
            let mut module_json = JsonMap::new();
            module_json.insert("enabled".to_owned(), JsonValue::Bool(module.is_enabled()));
            module_json.insert(
                "key".to_owned(),
                JsonValue::Number(JsonNumber::from(
                    module.state().key_bind().map(i64::from).unwrap_or(-1),
                )),
            );

            let mut values_json = JsonMap::new();
            for value in module.values() {
                values_json.insert(
                    value.config_key(),
                    JsonValue::String(value.serialize_config_value()),
                );
            }
            if !values_json.is_empty() {
                module_json.insert("values".to_owned(), JsonValue::Object(values_json));
            }

            modules_json.insert(config_key(module.name()), JsonValue::Object(module_json));
        }

        let mut root = JsonMap::new();
        root.insert("modules".to_owned(), JsonValue::Object(modules_json));
        let content = serde_json::to_string_pretty(&JsonValue::Object(root))
            .map_err(|error| io::Error::other(format!("serialize config: {error}")))?;
        fs::write(config_file, content)
    }

    pub fn load_config_if_exists(&mut self, config_name: &str) -> io::Result<bool> {
        if !Self::config_exists(config_name) {
            return Ok(false);
        }
        self.load_config(config_name)?;
        Ok(true)
    }

    pub fn load_config(&mut self, config_name: &str) -> io::Result<()> {
        let config_file = Self::config_file(config_name);
        let content = fs::read_to_string(&config_file)?;
        let root = serde_json::from_str::<JsonValue>(&content).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("parse config {}: {error}", config_file.display()),
            )
        })?;

        let Some(modules_json) = root.get("modules").and_then(JsonValue::as_object) else {
            return Ok(());
        };

        let previous_config_saving = self
            .iter()
            .map(|module| module.state().config_saving())
            .collect::<Vec<_>>();
        for module in self.iter_mut() {
            module.state_mut().set_config_saving(false);
        }

        let result = self.load_modules_from_json(modules_json);

        for (module, config_saving) in self.iter_mut().zip(previous_config_saving) {
            module.state_mut().set_config_saving(config_saving);
        }

        result
    }

    fn load_modules_from_json(
        &mut self,
        modules_json: &JsonMap<String, JsonValue>,
    ) -> io::Result<()> {
        for module in self.iter_mut() {
            let Some(module_json) = find_module_config(modules_json, module) else {
                continue;
            };

            apply_key_bind(module, module_json);
            apply_values(module, module_json);
            apply_enabled(module, module_json);
        }

        Ok(())
    }
}

fn roaming_app_data_dir() -> PathBuf {
    if let Some(app_data) = std::env::var_os("APPDATA") {
        return PathBuf::from(app_data);
    }

    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(user_profile).join("AppData").join("Roaming");
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn normalized_config_name(config_name: &str) -> String {
    let config_name = config_name.trim();
    if config_name.is_empty() {
        DEFAULT_CONFIG_NAME.to_owned()
    } else {
        config_name.trim_end_matches(".json").to_owned()
    }
}

fn find_module_config<'a>(
    modules_json: &'a JsonMap<String, JsonValue>,
    module: &(dyn Module + 'static),
) -> Option<&'a JsonMap<String, JsonValue>> {
    find_json_object(modules_json, &config_key(module.name())).or_else(|| {
        module.config_aliases().iter().find_map(|alias| {
            if alias.is_empty() {
                return None;
            }
            find_json_object(modules_json, alias)
                .or_else(|| find_json_object(modules_json, &config_key(alias)))
        })
    })
}

fn find_json_object<'a>(
    object: &'a JsonMap<String, JsonValue>,
    key: &str,
) -> Option<&'a JsonMap<String, JsonValue>> {
    object.get(key).and_then(JsonValue::as_object).or_else(|| {
        object
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| value.as_object())
    })
}

fn find_json_value<'a>(object: &'a JsonMap<String, JsonValue>, key: &str) -> Option<&'a JsonValue> {
    object.get(key).or_else(|| {
        object
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
            .map(|(_, value)| value)
    })
}

fn apply_key_bind(module: &mut (dyn Module + 'static), module_json: &JsonMap<String, JsonValue>) {
    let Some(key) = module_json.get("key") else {
        module.state_mut().set_key_bind(None);
        return;
    };

    if let Some(key) = key.as_i64() {
        module.state_mut().set_key_bind(u32::try_from(key).ok());
    }
}

fn apply_values(module: &mut (dyn Module + 'static), module_json: &JsonMap<String, JsonValue>) {
    let Some(values_json) = module_json.get("values").and_then(JsonValue::as_object) else {
        return;
    };

    let pending_values = module
        .values()
        .iter()
        .filter_map(|value| {
            let value_key = value.config_key();
            find_json_value(values_json, &value_key)
                .and_then(json_config_value_to_string)
                .map(|raw_value| (value_key, raw_value))
        })
        .collect::<Vec<_>>();

    for (value_key, raw_value) in pending_values {
        let normalized_value = module.normalize_config_value(&value_key, &raw_value);
        if let Some(value) = module.value_mut(&value_key) {
            let _ = value.deserialize_config_value(&normalized_value);
        }
    }
}

fn apply_enabled(module: &mut (dyn Module + 'static), module_json: &JsonMap<String, JsonValue>) {
    if let Some(enabled) = module_json.get("enabled").and_then(JsonValue::as_bool) {
        let _ = module.set_enabled(enabled);
    }
}

fn json_config_value_to_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(value) => Some(value.clone()),
        JsonValue::Bool(value) => Some(value.to_string()),
        JsonValue::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

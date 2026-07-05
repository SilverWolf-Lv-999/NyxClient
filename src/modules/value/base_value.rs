use std::{
    error::Error,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Boolean,
    Number,
    RandomNumber,
    Text,
    Color,
    Mode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BaseValue {
    name: String,
    data: ValueData,
    visibility: Option<ValueVisibility>,
}

impl BaseValue {
    pub fn boolean(value: bool, name: impl Into<String>) -> Self {
        Self::new(name, ValueData::Boolean(BooleanValue::new(value)))
    }

    pub fn number(value: f64, minimum: f64, maximum: f64, name: impl Into<String>) -> Self {
        Self::new(
            name,
            ValueData::Number(NumberValue::new(value, minimum, maximum)),
        )
    }

    pub fn percentage(value: f64, minimum: f64, maximum: f64, name: impl Into<String>) -> Self {
        Self::new(
            name,
            ValueData::Number(NumberValue::percentage(value, minimum, maximum)),
        )
    }

    pub fn random_number(
        current_minimum: f64,
        current_maximum: f64,
        minimum: f64,
        maximum: f64,
        name: impl Into<String>,
    ) -> Self {
        Self::new(
            name,
            ValueData::RandomNumber(RandomNumberValue::new(
                current_minimum,
                current_maximum,
                minimum,
                maximum,
            )),
        )
    }

    pub fn random_integer(
        current_minimum: f64,
        current_maximum: f64,
        minimum: f64,
        maximum: f64,
        name: impl Into<String>,
    ) -> Self {
        Self::new(
            name,
            ValueData::RandomNumber(RandomNumberValue::integer(
                current_minimum,
                current_maximum,
                minimum,
                maximum,
            )),
        )
    }

    pub fn text(value: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(name, ValueData::Text(StringValue::new(value)))
    }

    pub fn color(value: RgbaColor, name: impl Into<String>) -> Self {
        Self::new(name, ValueData::Color(ColorValue::new(value)))
    }

    pub fn mode(
        modes: impl IntoIterator<Item = impl Into<String>>,
        name: impl Into<String>,
        default_value: impl Into<String>,
    ) -> Self {
        Self::new(name, ValueData::Mode(ModeValue::new(modes, default_value)))
    }

    fn new(name: impl Into<String>, data: ValueData) -> Self {
        Self {
            name: name.into(),
            data,
            visibility: None,
        }
    }

    pub fn with_parent(mut self, parent_name: impl Into<String>) -> Self {
        self.visibility = Some(ValueVisibility::new(parent_name));
        self
    }

    pub fn with_mode_parent(
        mut self,
        parent_name: impl Into<String>,
        visible_when_mode: impl Into<String>,
    ) -> Self {
        self.visibility = Some(ValueVisibility::mode(parent_name, visible_when_mode));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn config_key(&self) -> String {
        config_key(&self.name)
    }

    pub fn kind(&self) -> ValueKind {
        self.data.kind()
    }

    pub fn reset(&mut self) {
        self.data.reset();
    }

    pub fn rest(&mut self) {
        self.reset();
    }

    pub fn is_visible(&self) -> bool {
        self.visibility.is_none()
    }

    pub fn is_visible_in(&self, values: &[BaseValue]) -> bool {
        self.is_visible_with_stack(values, &mut Vec::new())
    }

    pub fn display_value(&self) -> String {
        self.data.display_value()
    }

    pub fn serialize_config_value(&self) -> String {
        self.data.serialize_config_value()
    }

    pub fn deserialize_config_value(&mut self, value: &str) -> Result<(), ValueParseError> {
        self.data.deserialize_config_value(value)
    }

    pub fn as_boolean(&self) -> Option<&BooleanValue> {
        match &self.data {
            ValueData::Boolean(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_boolean_mut(&mut self) -> Option<&mut BooleanValue> {
        match &mut self.data {
            ValueData::Boolean(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<&NumberValue> {
        match &self.data {
            ValueData::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number_mut(&mut self) -> Option<&mut NumberValue> {
        match &mut self.data {
            ValueData::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_random_number(&self) -> Option<&RandomNumberValue> {
        match &self.data {
            ValueData::RandomNumber(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_random_number_mut(&mut self) -> Option<&mut RandomNumberValue> {
        match &mut self.data {
            ValueData::RandomNumber(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&StringValue> {
        match &self.data {
            ValueData::Text(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_text_mut(&mut self) -> Option<&mut StringValue> {
        match &mut self.data {
            ValueData::Text(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_color(&self) -> Option<&ColorValue> {
        match &self.data {
            ValueData::Color(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_color_mut(&mut self) -> Option<&mut ColorValue> {
        match &mut self.data {
            ValueData::Color(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_mode(&self) -> Option<&ModeValue> {
        match &self.data {
            ValueData::Mode(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_mode_mut(&mut self) -> Option<&mut ModeValue> {
        match &mut self.data {
            ValueData::Mode(value) => Some(value),
            _ => None,
        }
    }

    fn is_visible_with_stack(&self, values: &[BaseValue], stack: &mut Vec<String>) -> bool {
        let Some(visibility) = &self.visibility else {
            return true;
        };

        if stack.iter().any(|key| key == &visibility.parent_key) {
            return true;
        }

        let Some(parent) = values.iter().find(|value| {
            let key = value.config_key();
            key.eq_ignore_ascii_case(&visibility.parent_key)
                || value.name().eq_ignore_ascii_case(&visibility.parent_key)
        }) else {
            return true;
        };

        stack.push(visibility.parent_key.clone());
        if !parent.is_visible_with_stack(values, stack) {
            stack.pop();
            return false;
        }
        stack.pop();

        match &parent.data {
            ValueData::Boolean(value) => value.value(),
            ValueData::Mode(value) => visibility
                .visible_when_mode
                .as_ref()
                .is_none_or(|mode| value.is(mode)),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueVisibility {
    parent_key: String,
    visible_when_mode: Option<String>,
}

impl ValueVisibility {
    pub fn new(parent_name: impl Into<String>) -> Self {
        Self {
            parent_key: config_key(&parent_name.into()),
            visible_when_mode: None,
        }
    }

    pub fn mode(parent_name: impl Into<String>, visible_when_mode: impl Into<String>) -> Self {
        Self {
            parent_key: config_key(&parent_name.into()),
            visible_when_mode: Some(visible_when_mode.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ValueData {
    Boolean(BooleanValue),
    Number(NumberValue),
    RandomNumber(RandomNumberValue),
    Text(StringValue),
    Color(ColorValue),
    Mode(ModeValue),
}

impl ValueData {
    fn kind(&self) -> ValueKind {
        match self {
            Self::Boolean(_) => ValueKind::Boolean,
            Self::Number(_) => ValueKind::Number,
            Self::RandomNumber(_) => ValueKind::RandomNumber,
            Self::Text(_) => ValueKind::Text,
            Self::Color(_) => ValueKind::Color,
            Self::Mode(_) => ValueKind::Mode,
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Boolean(value) => value.reset(),
            Self::Number(value) => value.reset(),
            Self::RandomNumber(value) => value.reset(),
            Self::Text(value) => value.reset(),
            Self::Color(value) => value.reset(),
            Self::Mode(value) => value.reset(),
        }
    }

    fn display_value(&self) -> String {
        match self {
            Self::Boolean(value) => value.value().to_string(),
            Self::Number(value) => value.display_value(),
            Self::RandomNumber(value) => value.display_value(),
            Self::Text(value) => value.value().to_owned(),
            Self::Color(value) => value.value().to_hex_rgba(),
            Self::Mode(value) => value.current_mode().to_owned(),
        }
    }

    fn serialize_config_value(&self) -> String {
        match self {
            Self::Boolean(value) => value.value().to_string(),
            Self::Number(value) => value.value().to_string(),
            Self::RandomNumber(value) => format!(
                "{},{}",
                value.current_minimum_value(),
                value.current_maximum_value()
            ),
            Self::Text(value) => value.value().to_owned(),
            Self::Color(value) => value.value().to_hex_rgba(),
            Self::Mode(value) => value.current_mode().to_owned(),
        }
    }

    fn deserialize_config_value(&mut self, value: &str) -> Result<(), ValueParseError> {
        match self {
            Self::Boolean(boolean) => {
                boolean.set_value(value.trim().eq_ignore_ascii_case("true"));
                Ok(())
            }
            Self::Number(number) => {
                let value = parse_number(value)?;
                number.set_value(value);
                Ok(())
            }
            Self::RandomNumber(random_number) => {
                let (minimum, maximum) = parse_number_range(value)?;
                random_number.set_current_values(minimum, maximum);
                Ok(())
            }
            Self::Text(text) => {
                text.set_value(value);
                Ok(())
            }
            Self::Color(color) => {
                let value = RgbaColor::parse(value)?;
                color.set_value(value);
                Ok(())
            }
            Self::Mode(mode) => {
                mode.set_current_mode(value);
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BooleanValue {
    default_value: bool,
    value: bool,
}

impl BooleanValue {
    pub const fn new(value: bool) -> Self {
        Self {
            default_value: value,
            value,
        }
    }

    pub const fn default_value(&self) -> bool {
        self.default_value
    }

    pub const fn value(&self) -> bool {
        self.value
    }

    pub fn set_value(&mut self, value: bool) {
        self.value = value;
    }

    pub fn reset(&mut self) {
        self.value = self.default_value;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberValue {
    default_value: f64,
    value: f64,
    minimum: f64,
    maximum: f64,
    percentage: bool,
}

impl NumberValue {
    pub fn new(value: f64, minimum: f64, maximum: f64) -> Self {
        Self::with_percentage(value, minimum, maximum, false)
    }

    pub fn percentage(value: f64, minimum: f64, maximum: f64) -> Self {
        Self::with_percentage(value, minimum, maximum, true)
    }

    pub fn with_percentage(value: f64, minimum: f64, maximum: f64, percentage: bool) -> Self {
        let (minimum, maximum) = ordered_bounds(minimum, maximum);
        let value = clamp(value, minimum, maximum);
        Self {
            default_value: value,
            value,
            minimum,
            maximum,
            percentage,
        }
    }

    pub fn default_value(&self) -> f64 {
        self.default_value
    }

    pub fn value(&self) -> f64 {
        clamp(self.value, self.minimum, self.maximum)
    }

    pub fn int_value(&self) -> i64 {
        self.value().round() as i64
    }

    pub fn float_value(&self) -> f32 {
        self.value() as f32
    }

    pub fn double_value(&self) -> f64 {
        self.value()
    }

    pub fn minimum(&self) -> f64 {
        self.minimum
    }

    pub fn maximum(&self) -> f64 {
        self.maximum
    }

    pub fn is_percentage(&self) -> bool {
        self.percentage
    }

    pub fn set_value(&mut self, value: f64) {
        self.value = clamp(value, self.minimum, self.maximum);
    }

    pub fn reset(&mut self) {
        self.value = self.default_value;
    }

    pub fn display_value(&self) -> String {
        format_number_display(self.value(), self.percentage)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RandomNumberValue {
    default_minimum_value: f64,
    default_maximum_value: f64,
    current_minimum_value: f64,
    current_maximum_value: f64,
    minimum: f64,
    maximum: f64,
    percentage: bool,
    integer: bool,
}

impl RandomNumberValue {
    pub fn new(
        current_minimum_value: f64,
        current_maximum_value: f64,
        minimum: f64,
        maximum: f64,
    ) -> Self {
        Self::with_options(
            current_minimum_value,
            current_maximum_value,
            minimum,
            maximum,
            false,
            false,
        )
    }

    pub fn percentage(
        current_minimum_value: f64,
        current_maximum_value: f64,
        minimum: f64,
        maximum: f64,
    ) -> Self {
        Self::with_options(
            current_minimum_value,
            current_maximum_value,
            minimum,
            maximum,
            true,
            false,
        )
    }

    pub fn integer(
        current_minimum_value: f64,
        current_maximum_value: f64,
        minimum: f64,
        maximum: f64,
    ) -> Self {
        Self::with_options(
            current_minimum_value,
            current_maximum_value,
            minimum,
            maximum,
            false,
            true,
        )
    }

    pub fn with_options(
        current_minimum_value: f64,
        current_maximum_value: f64,
        minimum: f64,
        maximum: f64,
        percentage: bool,
        integer: bool,
    ) -> Self {
        let (minimum, maximum) = ordered_bounds(minimum, maximum);
        let mut value = Self {
            default_minimum_value: normalize_number(current_minimum_value, integer),
            default_maximum_value: normalize_number(current_maximum_value, integer),
            current_minimum_value: minimum,
            current_maximum_value: minimum,
            minimum,
            maximum,
            percentage,
            integer,
        };
        value.set_current_values(current_minimum_value, current_maximum_value);
        value.default_minimum_value = value.current_minimum_value;
        value.default_maximum_value = value.current_maximum_value;
        value
    }

    pub fn default_minimum_value(&self) -> f64 {
        self.default_minimum_value
    }

    pub fn default_maximum_value(&self) -> f64 {
        self.default_maximum_value
    }

    pub fn current_minimum_value(&self) -> f64 {
        self.clamp_to_bounds(self.current_minimum_value)
    }

    pub fn current_maximum_value(&self) -> f64 {
        self.clamp_to_bounds(self.current_maximum_value)
    }

    pub fn value(&self) -> f64 {
        let minimum = self.current_minimum_value();
        let maximum = self.current_maximum_value();

        if maximum <= minimum {
            return normalize_number(minimum, self.integer);
        }

        if self.integer {
            let low = minimum.round() as i64;
            let high = maximum.round() as i64;
            if high <= low {
                return low as f64;
            }
            return random_i64_inclusive(low, high) as f64;
        }

        minimum + (maximum - minimum) * random_unit()
    }

    pub fn minimum(&self) -> f64 {
        self.minimum
    }

    pub fn maximum(&self) -> f64 {
        self.maximum
    }

    pub fn is_percentage(&self) -> bool {
        self.percentage
    }

    pub fn is_integer(&self) -> bool {
        self.integer
    }

    pub fn set_current_minimum_value(&mut self, value: f64) {
        let current_maximum = self.current_maximum_value();
        let next = self.clamp_to_bounds(value).min(current_maximum);
        self.current_minimum_value = normalize_number(next, self.integer);
    }

    pub fn set_current_maximum_value(&mut self, value: f64) {
        let current_minimum = self.current_minimum_value();
        let next = self.clamp_to_bounds(value).max(current_minimum);
        self.current_maximum_value = normalize_number(next, self.integer);
    }

    pub fn set_current_values(&mut self, minimum_value: f64, maximum_value: f64) {
        let mut current_minimum = self.clamp_to_bounds(minimum_value);
        let mut current_maximum = self.clamp_to_bounds(maximum_value);
        if current_minimum > current_maximum {
            std::mem::swap(&mut current_minimum, &mut current_maximum);
        }
        self.current_minimum_value = normalize_number(current_minimum, self.integer);
        self.current_maximum_value = normalize_number(current_maximum, self.integer);
    }

    pub fn set_value(&mut self, value: f64) {
        self.set_current_values(value, value);
    }

    pub fn reset(&mut self) {
        self.set_current_values(self.default_minimum_value, self.default_maximum_value);
    }

    pub fn display_value(&self) -> String {
        format!(
            "{}-{}",
            format_number_display(self.current_minimum_value(), self.percentage),
            format_number_display(self.current_maximum_value(), self.percentage)
        )
    }

    fn clamp_to_bounds(&self, value: f64) -> f64 {
        normalize_number(clamp(value, self.minimum, self.maximum), self.integer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringValue {
    default_value: String,
    value: String,
}

impl StringValue {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            default_value: value.clone(),
            value,
        }
    }

    pub fn default_value(&self) -> &str {
        &self.default_value
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
    }

    pub fn reset(&mut self) {
        self.value.clone_from(&self.default_value);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl RgbaColor {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self::rgba(red, green, blue, 255)
    }

    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    pub fn parse(value: &str) -> Result<Self, ValueParseError> {
        let value = value.trim();
        let Some(hex) = value.strip_prefix('#') else {
            return Err(ValueParseError::new("color must start with #"));
        };

        if hex.len() != 6 && hex.len() != 8 {
            return Err(ValueParseError::new("color must be #RRGGBB or #RRGGBBAA"));
        }

        let red = parse_hex_byte(&hex[0..2])?;
        let green = parse_hex_byte(&hex[2..4])?;
        let blue = parse_hex_byte(&hex[4..6])?;
        let alpha = if hex.len() == 8 {
            parse_hex_byte(&hex[6..8])?
        } else {
            255
        };

        Ok(Self::rgba(red, green, blue, alpha))
    }

    pub fn to_hex_rgba(self) -> String {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            self.red, self.green, self.blue, self.alpha
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorValue {
    default_value: RgbaColor,
    value: RgbaColor,
}

impl ColorValue {
    pub const fn new(value: RgbaColor) -> Self {
        Self {
            default_value: value,
            value,
        }
    }

    pub const fn default_value(&self) -> RgbaColor {
        self.default_value
    }

    pub const fn value(&self) -> RgbaColor {
        self.value
    }

    pub fn set_value(&mut self, value: RgbaColor) {
        self.value = value;
    }

    pub fn reset(&mut self) {
        self.value = self.default_value;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeValue {
    default_value: String,
    value: String,
    modes: Vec<String>,
}

impl ModeValue {
    pub fn new(
        modes: impl IntoIterator<Item = impl Into<String>>,
        default_value: impl Into<String>,
    ) -> Self {
        let modes = modes.into_iter().map(Into::into).collect::<Vec<_>>();
        let requested_default = default_value.into();
        let default_value = default_mode(&modes, &requested_default);
        Self {
            value: default_value.clone(),
            default_value,
            modes,
        }
    }

    pub fn with_default(mut self, mode: impl AsRef<str>) -> Self {
        self.set_current_mode(mode);
        self.default_value.clone_from(&self.value);
        self
    }

    pub fn is(&self, mode: impl AsRef<str>) -> bool {
        self.current_mode() == mode.as_ref()
    }

    pub fn current_mode(&self) -> &str {
        &self.value
    }

    pub fn default_value(&self) -> &str {
        &self.default_value
    }

    pub fn modes(&self) -> &[String] {
        &self.modes
    }

    pub fn set_current_mode(&mut self, mode: impl AsRef<str>) {
        let mode = mode.as_ref();
        if self.modes.iter().any(|candidate| candidate == mode) {
            self.value = mode.to_owned();
        }
    }

    pub fn reset(&mut self) {
        self.value.clone_from(&self.default_value);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueParseError {
    message: String,
}

impl ValueParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ValueParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ValueParseError {}

pub fn config_key(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn default_mode(modes: &[String], requested_default: &str) -> String {
    if modes.is_empty() {
        return String::new();
    }

    modes
        .iter()
        .find(|mode| mode.as_str() == requested_default)
        .cloned()
        .unwrap_or_else(|| modes[0].clone())
}

fn ordered_bounds(minimum: f64, maximum: f64) -> (f64, f64) {
    if minimum <= maximum {
        (minimum, maximum)
    } else {
        (maximum, minimum)
    }
}

fn clamp(value: f64, minimum: f64, maximum: f64) -> f64 {
    value.max(minimum).min(maximum)
}

fn normalize_number(value: f64, integer: bool) -> f64 {
    if integer { value.round() } else { value }
}

fn format_number_display(value: f64, percentage: bool) -> String {
    if percentage {
        format!("{:.2}%", value * 100.0)
    } else {
        format!("{value:.2}")
    }
}

fn parse_number(value: &str) -> Result<f64, ValueParseError> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|error| ValueParseError::new(format!("invalid number: {error}")))
}

fn parse_number_range(value: &str) -> Result<(f64, f64), ValueParseError> {
    let value = value.trim();
    for delimiter in [',', ';', '~', ':'] {
        if let Some((left, right)) = value.split_once(delimiter) {
            return Ok((parse_number(left)?, parse_number(right)?));
        }
    }

    for (index, character) in value.char_indices().skip(1) {
        if character != '-' {
            continue;
        }

        let left = value[..index].trim();
        let right = value[index + character.len_utf8()..].trim();
        if let (Ok(left), Ok(right)) = (left.parse::<f64>(), right.parse::<f64>()) {
            return Ok((left, right));
        }
    }

    let single = parse_number(value)?;
    Ok((single, single))
}

fn parse_hex_byte(value: &str) -> Result<u8, ValueParseError> {
    u8::from_str_radix(value, 16)
        .map_err(|error| ValueParseError::new(format!("invalid color component: {error}")))
}

fn random_unit() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (mixed >> 11) as f64 / (1_u64 << 53) as f64
}

fn random_i64_inclusive(low: i64, high: i64) -> i64 {
    let width = high.saturating_sub(low).saturating_add(1) as u64;
    if width == 0 {
        return low;
    }
    low.saturating_add((random_unit() * width as f64).floor() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parses_java_config_formats() {
        assert_eq!(
            RgbaColor::parse("#AABBCC").unwrap(),
            RgbaColor::rgba(0xAA, 0xBB, 0xCC, 0xFF)
        );
        assert_eq!(
            RgbaColor::parse("#AABBCC80").unwrap(),
            RgbaColor::rgba(0xAA, 0xBB, 0xCC, 0x80)
        );
    }

    #[test]
    fn random_number_range_accepts_negative_dash_ranges() {
        assert_eq!(parse_number_range("-1.5--0.5").unwrap(), (-1.5, -0.5));
        assert_eq!(parse_number_range("1~2").unwrap(), (1.0, 2.0));
    }

    #[test]
    fn visibility_follows_boolean_and_mode_parents() {
        let mut values = vec![
            BaseValue::boolean(false, "Advanced"),
            BaseValue::mode(["Simple", "Expert"], "Mode", "Expert"),
            BaseValue::number(1.0, 0.0, 2.0, "Hidden").with_parent("Advanced"),
            BaseValue::number(1.0, 0.0, 2.0, "Expert Only").with_mode_parent("Mode", "Expert"),
        ];

        assert!(!values[2].is_visible_in(&values));
        assert!(values[3].is_visible_in(&values));

        values[0].as_boolean_mut().unwrap().set_value(true);
        values[1].as_mode_mut().unwrap().set_current_mode("Simple");

        assert!(values[2].is_visible_in(&values));
        assert!(!values[3].is_visible_in(&values));
    }
}

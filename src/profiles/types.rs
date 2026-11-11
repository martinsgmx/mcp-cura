//! Shared profile types: settings, definitions, instances, and materials.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Cura settings are a JSON object of primitive values or arrays/objects.
pub type Settings = Map<String, Value>;

/// The declared type of a Cura setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingType {
    Bool,
    Int,
    Float,
    Str,
    Enum,
    Polygon,
    Polygons,
    Other,
}

impl SettingType {
    /// Parse the `type` field used by Cura definitions.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "bool" => Self::Bool,
            "int" => Self::Int,
            "float" => Self::Float,
            "str" => Self::Str,
            "enum" => Self::Enum,
            "polygon" => Self::Polygon,
            "polygons" => Self::Polygons,
            _ => Self::Other,
        }
    }

    /// Whether a value can be sent to `CuraEngine` as-is.
    #[must_use]
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Bool => value.is_boolean(),
            Self::Int => value.is_i64() || value.is_u64(),
            Self::Float => value.is_number(),
            Self::Str | Self::Enum => value.is_string(),
            Self::Polygon | Self::Polygons | Self::Other => true,
        }
    }
}

/// One setting from a `.def.json` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingDefinition {
    pub key: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub unit: Option<String>,
    pub kind: SettingType,
    /// Literal default used when the setting is not otherwise set.
    pub default_value: Option<Value>,
    /// Cura expression or literal used as a computed fallback.
    pub value_expression: Option<Value>,
    pub options: BTreeMap<String, String>,
    pub minimum: Option<Value>,
    pub maximum: Option<Value>,
    pub minimum_warning: Option<Value>,
    pub maximum_warning: Option<Value>,
    pub enabled: Option<Value>,
    pub settable_per_mesh: bool,
    pub settable_per_extruder: bool,
    pub settable_globally: bool,
}

impl SettingDefinition {
    /// Build a definition from a Cura setting object.
    #[must_use]
    pub fn from_value(key: &str, value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let kind = SettingType::parse(object.get("type").and_then(Value::as_str));
        if kind == SettingType::Other && object.get("default_value").is_none() {
            return None;
        }
        let options = object
            .get("options")
            .and_then(Value::as_object)
            .map(|options| {
                options
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_str().unwrap_or(key).to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            key: key.to_string(),
            label: object
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string),
            description: object
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            unit: object
                .get("unit")
                .and_then(Value::as_str)
                .map(str::to_string),
            kind,
            default_value: object.get("default_value").cloned(),
            value_expression: object.get("value").cloned(),
            options,
            minimum: object.get("minimum_value").cloned(),
            maximum: object.get("maximum_value").cloned(),
            minimum_warning: object.get("minimum_value_warning").cloned(),
            maximum_warning: object.get("maximum_value_warning").cloned(),
            enabled: object.get("enabled").cloned(),
            settable_per_mesh: object
                .get("settable_per_mesh")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            settable_per_extruder: object
                .get("settable_per_extruder")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            settable_globally: object
                .get("settable_globally")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        })
    }
}

/// A resolved `.def.json` (including inherited parents and overrides).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Definition {
    /// Machine/profile name from the file.
    pub name: String,
    /// Path the definition was loaded from.
    pub path: String,
    /// Definition this one inherits from, if any.
    pub inherits: Option<String>,
    pub metadata: Value,
    pub settings: BTreeMap<String, SettingDefinition>,
    /// Definition names in inheritance order (root first).
    pub lineage: Vec<String>,
    /// Non-fatal problems found while loading or inheriting.
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Definition {
    /// Number of flattened settings.
    #[must_use]
    pub fn setting_count(&self) -> usize {
        self.settings.len()
    }

    /// Look up one setting.
    #[must_use]
    pub fn setting(&self, key: &str) -> Option<&SettingDefinition> {
        self.settings.get(key)
    }

    /// Initial literal defaults.
    #[must_use]
    pub fn defaults(&self) -> Settings {
        let mut values = Settings::new();
        for setting in self.settings.values() {
            if let Some(default) = &setting.default_value {
                values.insert(setting.key.clone(), default.clone());
            } else if let Some(value) = &setting.value_expression {
                if !is_expression(value) {
                    values.insert(setting.key.clone(), value.clone());
                }
            }
        }
        values
    }
}

/// The kind of a Cura profile file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Definition,
    Quality,
    QualityChanges,
    Material,
    Variant,
    Intent,
    Extruder,
    User,
    Custom,
    Unknown,
}

impl ProfileKind {
    /// Stable label used in tool output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Quality => "quality",
            Self::QualityChanges => "quality_changes",
            Self::Material => "material",
            Self::Variant => "variant",
            Self::Intent => "intent",
            Self::Extruder => "extruder",
            Self::User => "user",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a `metadata.type` value from a `.inst.cfg`.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "quality" => Self::Quality,
            "quality_changes" | "quality-changes" => Self::QualityChanges,
            "material" => Self::Material,
            "variant" => Self::Variant,
            "intent" => Self::Intent,
            "extruder" => Self::Extruder,
            "user" => Self::User,
            _ => Self::Custom,
        }
    }
}

/// A `.inst.cfg` instance profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceProfile {
    pub path: String,
    pub name: String,
    pub definition: Option<String>,
    pub version: Option<String>,
    pub kind: ProfileKind,
    pub metadata: BTreeMap<String, String>,
    pub values: Settings,
    /// Values that still contain an unevaluated `=...` expression.
    #[serde(default)]
    pub expressions: BTreeMap<String, String>,
}

/// A material profile parsed from `.xml.fdm_material`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialProfile {
    pub path: String,
    pub name: String,
    pub guid: Option<String>,
    pub brand: Option<String>,
    pub material_type: Option<String>,
    pub color: Option<String>,
    pub density: Option<f64>,
    /// Raw `<setting key="...">` values.
    pub settings: BTreeMap<String, String>,
    /// Values mapped to Cura setting names.
    pub mapped: Settings,
}

/// A discovered profile file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub id: String,
    pub kind: ProfileKind,
    pub name: String,
    pub path: String,
    pub definition: Option<String>,
    pub quality_type: Option<String>,
    pub material: Option<String>,
    pub variant: Option<String>,
    pub intent: Option<String>,
    pub setting_version: Option<String>,
    pub value_count: usize,
}

/// Full profile catalog produced by a scan.
#[derive(Debug, Clone, Default)]
pub struct ProfileCatalog {
    pub entries: Vec<ProfileEntry>,
    /// Definition name -> definition path.
    pub definitions: BTreeMap<String, String>,
    pub scanned_dirs: Vec<String>,
    pub warnings: Vec<String>,
}

impl ProfileCatalog {
    /// Find an entry by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ProfileEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Find entries by kind and case-insensitive name/path substring.
    #[must_use]
    pub fn find(&self, kind: Option<ProfileKind>, query: Option<&str>) -> Vec<&ProfileEntry> {
        let query = query.map(str::to_ascii_lowercase);
        self.entries
            .iter()
            .filter(|entry| kind.is_none_or(|kind| entry.kind == kind))
            .filter(|entry| {
                query.as_ref().is_none_or(|query| {
                    entry.name.to_ascii_lowercase().contains(query)
                        || entry.path.to_ascii_lowercase().contains(query)
                        || entry.definition.as_ref().is_some_and(|definition| {
                            definition.to_ascii_lowercase().contains(query)
                        })
                })
            })
            .collect()
    }
}

/// Whether a Cura value is an expression.
#[must_use]
pub fn is_expression(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.trim_start().starts_with('='))
}

/// Coerce an INI/string value to the type declared by a setting.
#[must_use]
pub fn coerce_value(setting: Option<&SettingDefinition>, raw: &str) -> Value {
    let trimmed = raw.trim();
    let kind = setting.map_or(SettingType::Other, |setting| setting.kind);
    match kind {
        SettingType::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Value::Bool(true),
            "false" | "0" | "no" | "off" => Value::Bool(false),
            _ => Value::String(trimmed.to_string()),
        },
        SettingType::Int => trimmed
            .parse::<i64>()
            .map_or_else(|_| Value::String(trimmed.to_string()), Value::from),
        SettingType::Float => trimmed.parse::<f64>().map_or_else(
            |_| Value::String(trimmed.to_string()),
            |value| {
                serde_json::Number::from_f64(value)
                    .map_or_else(|| Value::String(trimmed.to_string()), Value::Number)
            },
        ),
        SettingType::Str | SettingType::Enum => Value::String(unquote(trimmed)),
        SettingType::Polygon | SettingType::Polygons => {
            serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(unquote(trimmed)))
        }
        SettingType::Other => infer_value(trimmed),
    }
}

fn infer_value(raw: &str) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        if value.is_boolean() || value.is_number() || value.is_array() || value.is_object() {
            return value;
        }
    }
    Value::String(unquote(raw))
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].replace("\\n", "\n")
    } else {
        value.to_string()
    }
}

/// Serialize a setting value for a `.inst.cfg` `[values]` section.
#[must_use]
pub fn format_ini_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.replace('\n', "\\n"),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

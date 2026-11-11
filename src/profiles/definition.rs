//! `.def.json` loading, inheritance, and setting flattening.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value};

use crate::{
    error::{Error, Result},
    profiles::types::{Definition, SettingDefinition},
};

const MAX_INHERITANCE_DEPTH: usize = 16;
const KNOWN_TOP_LEVEL: &[&str] = &[
    "version",
    "name",
    "inherits",
    "metadata",
    "overrides",
    "settings",
];

/// Load a definition and all its parents.
pub fn load_definition(path: &Path, search_dirs: &[PathBuf]) -> Result<Definition> {
    let mut lineage = Vec::new();
    load_recursive(path, search_dirs, &mut lineage, 0)
}

fn load_recursive(
    path: &Path,
    search_dirs: &[PathBuf],
    lineage: &mut Vec<String>,
    depth: usize,
) -> Result<Definition> {
    if depth > MAX_INHERITANCE_DEPTH {
        return Err(Error::Profile(
            "definition inheritance is deeper than 16 levels".into(),
        ));
    }

    let text = std::fs::read_to_string(path).map_err(|err| {
        Error::Profile(format!("cannot read definition {}: {err}", path.display()))
    })?;
    let root: Value = serde_json::from_str(&text).map_err(|err| {
        Error::Profile(format!(
            "invalid JSON in definition {}: {err}",
            path.display()
        ))
    })?;
    let object = root
        .as_object()
        .ok_or_else(|| Error::Profile(format!("definition {} is not an object", path.display())))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("definition")
        })
        .to_string();

    if lineage.contains(&name) {
        return Err(Error::Profile(format!(
            "definition inheritance cycle involving '{name}'"
        )));
    }
    lineage.push(name.clone());

    let mut warnings = Vec::new();
    let mut raw_settings = BTreeMap::<String, Value>::new();
    let mut metadata = Value::Object(Map::new());
    let mut inherits = None;

    if let Some(parent_name) = object.get("inherits").and_then(Value::as_str) {
        inherits = Some(parent_name.to_string());
        match find_definition(parent_name, path, search_dirs) {
            Some(parent_path) => {
                let parent = load_recursive(&parent_path, search_dirs, lineage, depth + 1)?;
                raw_settings = parent
                    .settings
                    .iter()
                    .map(|(key, setting)| (key.clone(), setting_to_value(setting)))
                    .collect();
                metadata = parent.metadata;
                warnings = parent.warnings;
            }
            None => warnings.push(format!(
                "inherited definition '{parent_name}' was not found; using only settings in {}",
                path.display()
            )),
        }
    }

    flatten_into(object.get("settings"), &mut raw_settings);
    if let Some(overrides) = object.get("overrides").and_then(Value::as_object) {
        for (key, value) in overrides {
            let entry = raw_settings
                .entry(key.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            merge_object(entry, value);
        }
    }

    if let Some(own_metadata) = object.get("metadata") {
        merge_value(&mut metadata, own_metadata);
    }

    let settings = raw_settings
        .iter()
        .filter_map(|(key, value)| {
            SettingDefinition::from_value(key, value).map(|setting| (key.clone(), setting))
        })
        .collect::<BTreeMap<_, _>>();

    Ok(Definition {
        name,
        path: path.to_string_lossy().to_string(),
        inherits,
        metadata,
        settings,
        lineage: lineage.iter().rev().cloned().collect(),
        warnings,
    })
}

/// Find a definition by name or path.
#[must_use]
pub fn find_definition(name: &str, current: &Path, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    let candidate = PathBuf::from(name);
    if candidate.is_absolute() && candidate.is_file() {
        return Some(candidate);
    }
    if let Some(parent) = current.parent() {
        let local = if name.ends_with(".def.json") {
            parent.join(name)
        } else {
            parent.join(format!("{name}.def.json"))
        };
        if local.is_file() {
            return Some(local);
        }
    }
    for dir in search_dirs {
        for candidate in [
            dir.join(format!("{name}.def.json")),
            dir.join("definitions").join(format!("{name}.def.json")),
            dir.join(name),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Flatten Cura's nested `settings` categories into a key -> raw object map.
#[must_use]
pub fn flatten_settings(root: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_into(root.get("settings"), &mut out);
    out
}

fn flatten_into(settings: Option<&Value>, out: &mut BTreeMap<String, Value>) {
    let Some(settings) = settings.and_then(Value::as_object) else {
        return;
    };
    for (key, value) in settings {
        if KNOWN_TOP_LEVEL.contains(&key.as_str()) {
            continue;
        }
        let Some(object) = value.as_object() else {
            continue;
        };
        let is_category = object.get("type").and_then(Value::as_str) == Some("category");
        if !is_category && object.contains_key("default_value") {
            out.insert(key.clone(), value.clone());
        }
        if let Some(children) = object.get("children") {
            flatten_into(Some(children), out);
        }
    }
}

fn setting_to_value(setting: &SettingDefinition) -> Value {
    let mut object = Map::new();
    object.insert(
        "type".to_string(),
        Value::String(
            match setting.kind {
                crate::profiles::types::SettingType::Bool => "bool",
                crate::profiles::types::SettingType::Int => "int",
                crate::profiles::types::SettingType::Float => "float",
                crate::profiles::types::SettingType::Str
                | crate::profiles::types::SettingType::Other => "str",
                crate::profiles::types::SettingType::Enum => "enum",
                crate::profiles::types::SettingType::Polygon => "polygon",
                crate::profiles::types::SettingType::Polygons => "polygons",
            }
            .to_string(),
        ),
    );
    if let Some(value) = &setting.default_value {
        object.insert("default_value".to_string(), value.clone());
    }
    if let Some(value) = &setting.value_expression {
        object.insert("value".to_string(), value.clone());
    }
    if let Some(value) = &setting.label {
        object.insert("label".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = &setting.description {
        object.insert("description".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = &setting.unit {
        object.insert("unit".to_string(), Value::String(value.clone()));
    }
    if !setting.options.is_empty() {
        object.insert(
            "options".to_string(),
            Value::Object(
                setting
                    .options
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
    }
    for (name, value) in [
        ("minimum_value", &setting.minimum),
        ("maximum_value", &setting.maximum),
        ("minimum_value_warning", &setting.minimum_warning),
        ("maximum_value_warning", &setting.maximum_warning),
        ("enabled", &setting.enabled),
    ] {
        if let Some(value) = value {
            object.insert(name.to_string(), value.clone());
        }
    }
    for (name, value) in [
        ("settable_per_mesh", setting.settable_per_mesh),
        ("settable_per_extruder", setting.settable_per_extruder),
        ("settable_globally", setting.settable_globally),
    ] {
        object.insert(name.to_string(), Value::Bool(value));
    }
    Value::Object(object)
}

fn merge_object(target: &mut Value, source: &Value) {
    let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) else {
        *target = source.clone();
        return;
    };
    for (key, value) in source {
        match target.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merge_object(existing, value);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn merge_value(target: &mut Value, source: &Value) {
    if target.is_object() && source.is_object() {
        merge_object(target, source);
    } else {
        *target = source.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_categories() {
        let value = serde_json::json!({
            "settings": {
                "machine_settings": {
                    "type": "category",
                    "children": {
                        "machine_width": {"type": "float", "default_value": 100},
                        "machine_name": {"type": "str", "default_value": "X"}
                    }
                }
            }
        });
        let flat = flatten_settings(&value);
        assert_eq!(flat.len(), 2);
        assert!(flat.contains_key("machine_width"));
    }

    #[test]
    fn loads_inheritance_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent.def.json");
        std::fs::write(
            &parent,
            serde_json::json!({
                "name": "parent",
                "settings": {
                    "a": {"type": "category", "children": {
                        "machine_width": {"type": "float", "default_value": 100},
                        "layer_height": {"type": "float", "default_value": 0.2}
                    }}
                }
            })
            .to_string(),
        )
        .unwrap();
        let child = dir.path().join("child.def.json");
        std::fs::write(
            &child,
            serde_json::json!({
                "name": "child",
                "inherits": "parent",
                "overrides": {
                    "machine_width": {"default_value": 220},
                    "machine_name": {"type": "str", "default_value": "Child"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let definition = load_definition(&child, &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(definition.name, "child");
        assert_eq!(
            definition.setting("machine_width").unwrap().default_value,
            Some(serde_json::json!(220))
        );
        assert_eq!(
            definition.setting("layer_height").unwrap().default_value,
            Some(serde_json::json!(0.2))
        );
        assert_eq!(definition.lineage, vec!["parent", "child"]);
    }
}

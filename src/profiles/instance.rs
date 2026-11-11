//! `.inst.cfg` parsing for quality, material, variant, intent, and custom
//! profiles.

use std::path::Path;

use ini::Ini;
use serde_json::Value;

use crate::{
    error::{Error, Result},
    profiles::types::{coerce_value, is_expression, InstanceProfile, ProfileKind, Settings},
};

/// Parse a Cura `.inst.cfg` file.
pub fn load_instance(path: &Path) -> Result<InstanceProfile> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| Error::Profile(format!("cannot read {}: {err}", path.display())))?;
    parse_instance(&text, &path.to_string_lossy())
}

/// Parse `.inst.cfg` text. Kept separate for tests and in-memory profiles.
pub fn parse_instance(text: &str, path: &str) -> Result<InstanceProfile> {
    let ini = Ini::load_from_str(text)
        .map_err(|err| Error::Profile(format!("invalid INI profile {path}: {err}")))?;

    let mut metadata = std::collections::BTreeMap::new();
    if let Some(section) = ini.section(Some("metadata")) {
        for (key, value) in section {
            metadata.insert(key.to_string(), value.to_string());
        }
    }

    let mut general = std::collections::BTreeMap::new();
    if let Some(section) = ini.section(Some("general")) {
        for (key, value) in section {
            general.insert(key.to_string(), value.to_string());
        }
    }

    let kind = ProfileKind::parse(metadata.get("type").map(String::as_str));
    let definition = general.get("definition").cloned();
    let version = general.get("version").cloned();
    let name = general
        .get("name")
        .cloned()
        .or_else(|| metadata.get("name").cloned())
        .unwrap_or_else(|| {
            Path::new(path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("profile")
                .to_string()
        });

    let mut values = Settings::new();
    let mut expressions = std::collections::BTreeMap::new();
    if let Some(section) = ini.section(Some("values")) {
        for (key, raw) in section {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let value = Value::String(raw.to_string());
            if is_expression(&value) {
                expressions.insert(key.to_string(), raw.to_string());
                // A placeholder lets resolution know the key was explicitly set.
                values.insert(key.to_string(), value);
            } else {
                values.insert(key.to_string(), coerce_value(None, raw));
            }
        }
    }

    Ok(InstanceProfile {
        path: path.to_string(),
        name,
        definition,
        version,
        kind,
        metadata,
        values,
        expressions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quality_profile() {
        let text = "[general]\nversion = 4\ndefinition = fdmprinter\nname = Fine\n\n\
                    [metadata]\ntype = quality\nquality_type = fine\nsetting_version = 27\n\n\
                    [values]\nlayer_height = 0.12\ninfill_sparse_density = 20\n\
                    support_enable = true\nadhesion_type = brim\n";
        let profile = parse_instance(text, "/tmp/fine.inst.cfg").unwrap();
        assert_eq!(profile.kind, ProfileKind::Quality);
        assert_eq!(profile.name, "Fine");
        assert_eq!(profile.definition.as_deref(), Some("fdmprinter"));
        assert_eq!(
            profile.values.get("layer_height"),
            Some(&serde_json::json!(0.12))
        );
        assert_eq!(
            profile.values.get("support_enable"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            profile.values.get("adhesion_type"),
            Some(&serde_json::json!("brim"))
        );
    }

    #[test]
    fn records_expressions() {
        let text = "[general]\ndefinition = fdmprinter\nname = Custom\n\n\
                    [metadata]\ntype = quality_changes\n\n\
                    [values]\nlayer_height = =layer_height_0 / 2\n";
        let profile = parse_instance(text, "/tmp/custom.inst.cfg").unwrap();
        assert_eq!(profile.kind, ProfileKind::QualityChanges);
        assert_eq!(
            profile.expressions.get("layer_height").map(String::as_str),
            Some("=layer_height_0 / 2")
        );
    }
}

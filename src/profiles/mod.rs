//! Cura profile support.
//!
//! The module understands the three profile formats used by Cura:
//!
//! * `.def.json` machine definitions (with `inherits` and `overrides`),
//! * `.inst.cfg` instance profiles (quality, material, variant, intent,
//!   quality changes, extruder, user/custom),
//! * `.xml.fdm_material` material profiles.
//!
//! Resolution order follows Cura's stack: definition defaults, definition
//! changes, variant, material, quality, intent, quality changes/custom, user
//! overrides, then caller overrides. Expressions are evaluated where possible.

pub mod definition;
pub mod index;
pub mod instance;
pub mod material;
pub mod resolve;
pub mod types;
pub mod verify;

use std::fmt::Write as _;

use crate::{
    error::{Error, Result},
    profiles::types::{format_ini_value, Settings},
};

pub use definition::{find_definition, flatten_settings, load_definition};
pub use index::ProfileIndex;
pub use instance::{load_instance, parse_instance};
pub use material::{load_material, map_settings, parse_material};
pub use resolve::{
    evaluate_expression, extruder_map, merged, resolve, settings_from_value,
    temperature_violations, ProfileLayer, Resolution,
};
pub use types::{
    coerce_value, Definition, InstanceProfile, MaterialProfile, ProfileCatalog, ProfileEntry,
    ProfileKind, SettingDefinition, SettingType,
};
pub use verify::{verify, Issue, Severity, VerificationReport};

/// Build the text of a custom `.inst.cfg` profile.
#[must_use]
pub fn custom_profile_text(
    definition: &str,
    name: &str,
    kind: ProfileKind,
    setting_version: Option<&str>,
    values: &Settings,
) -> String {
    let mut text = String::new();
    text.push_str("[general]\nversion = 4\n");
    writeln!(text, "definition = {definition}").expect("writing to a String cannot fail");
    writeln!(text, "name = {name}\n").expect("writing to a String cannot fail");
    text.push_str("[metadata]\ntype = quality_changes\n");
    if let Some(version) = setting_version {
        writeln!(text, "setting_version = {version}").expect("writing to a String cannot fail");
    }
    writeln!(text, "mcp_cura_kind = {}\n", kind.as_str()).expect("writing to a String cannot fail");
    text.push_str("[values]\n");
    let mut keys = values.keys().collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        writeln!(text, "{key} = {}", format_ini_value(&values[key]))
            .expect("writing to a String cannot fail");
    }
    text
}

/// Validate a custom profile name and return a safe file name.
pub fn custom_profile_file_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 96 {
        return Err(Error::InvalidArguments(
            "profile name must be between 1 and 96 characters".into(),
        ));
    }
    let mut file = String::new();
    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ' ') {
            file.push(character);
        } else if character == '.' || character == '/' || character == '\\' {
            return Err(Error::InvalidArguments(
                "profile name must not contain path separators or dots".into(),
            ));
        } else {
            file.push('_');
        }
    }
    let file = file.trim().replace(' ', "_");
    Ok(format!("{file}.inst.cfg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_custom_profile() {
        let mut values = Settings::new();
        values.insert("layer_height".into(), serde_json::json!(0.16));
        values.insert("adhesion_type".into(), serde_json::json!("brim"));
        let text = custom_profile_text(
            "creality_ender3",
            "My Fine",
            ProfileKind::QualityChanges,
            Some("27"),
            &values,
        );
        assert!(text.contains("[general]"));
        assert!(text.contains("definition = creality_ender3"));
        assert!(text.contains("layer_height = 0.16"));
        assert!(text.contains("adhesion_type = brim"));
    }

    #[test]
    fn rejects_bad_profile_names() {
        assert!(custom_profile_file_name("../x").is_err());
        assert!(custom_profile_file_name("").is_err());
        assert_eq!(
            custom_profile_file_name("My Fine").unwrap(),
            "My_Fine.inst.cfg"
        );
    }
}

//! `.xml.fdm_material` parsing and mapping to Cura setting names.

use std::{collections::BTreeMap, path::Path};

use quick_xml::{events::Event, Reader, XmlVersion};
use serde_json::{Number, Value};

use crate::{
    error::{Error, Result},
    profiles::types::{MaterialProfile, Settings},
};

/// Parse a Cura material XML file.
pub fn load_material(path: &Path) -> Result<MaterialProfile> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| Error::Profile(format!("cannot read {}: {err}", path.display())))?;
    parse_material(&text, &path.to_string_lossy())
}

/// Parse material XML text.
pub fn parse_material(text: &str, path: &str) -> Result<MaterialProfile> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut text_buffers: BTreeMap<String, String> = BTreeMap::new();
    let mut settings = BTreeMap::new();
    let mut current_setting: Option<String> = None;
    let mut current_setting_text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "setting" {
                    current_setting = event
                        .attributes()
                        .flatten()
                        .find(|attribute| local_name(attribute.key.as_ref()) == "key")
                        .and_then(|attribute| {
                            attribute
                                .normalized_value(XmlVersion::Implicit1_0)
                                .ok()
                                .map(|value| value.to_string())
                        });
                    current_setting_text.clear();
                }
                stack.push(name);
            }
            Ok(Event::Text(event)) => {
                let content = event.xml_content(XmlVersion::Implicit1_0);
                let content = quick_xml::escape::unescape(&content)
                    .map_or_else(|_| content.to_string(), |value| value.to_string());
                if current_setting.is_some() {
                    current_setting_text.push_str(&content);
                } else {
                    text_buffers
                        .entry(stack.join("/"))
                        .or_default()
                        .push_str(&content);
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "setting" {
                    if let Some(key) = current_setting.take() {
                        settings.insert(key, current_setting_text.trim().to_string());
                    }
                    current_setting_text.clear();
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(Error::Profile(format!(
                    "invalid material XML {path}: {err}"
                )));
            }
        }
    }

    let name = text_buffers
        .get("fdmmaterial/metadata/name")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let brand = text_buffers
                .get("fdmmaterial/metadata/name/brand")
                .map(|value| value.trim())
                .unwrap_or_default();
            let material = text_buffers
                .get("fdmmaterial/metadata/name/material")
                .map(|value| value.trim())
                .unwrap_or_default();
            let combined = format!("{brand} {material}").trim().to_string();
            (!combined.is_empty()).then_some(combined)
        })
        .unwrap_or_else(|| {
            Path::new(path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("material")
                .to_string()
        });

    let brand = text_buffers
        .get("fdmmaterial/metadata/name/brand")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let material_type = text_buffers
        .get("fdmmaterial/metadata/name/material")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let color = text_buffers
        .get("fdmmaterial/metadata/name/color")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let guid = text_buffers
        .get("fdmmaterial/metadata/GUID")
        .or_else(|| text_buffers.get("fdmmaterial/metadata/guid"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let mapped = map_settings(&settings);
    let density = mapped
        .get("material_density")
        .and_then(Value::as_f64)
        .or_else(|| settings.get("density").and_then(|value| value.parse().ok()));

    Ok(MaterialProfile {
        path: path.to_string(),
        name,
        guid,
        brand,
        material_type,
        color,
        density,
        settings,
        mapped,
    })
}

/// Map common `.fdm_material` keys to Cura settings.
#[must_use]
pub fn map_settings(settings: &BTreeMap<String, String>) -> Settings {
    let mut mapped = Settings::new();
    for (key, raw) in settings {
        let normalized = key.trim().to_ascii_lowercase();
        let target = match normalized.as_str() {
            "print temperature" | "printing temperature" => "material_print_temperature",
            "print temperature layer 0" | "printing temperature layer 0" => {
                "material_print_temperature_layer_0"
            }
            "standby temperature" => "material_standby_temperature",
            "bed temperature" | "build plate temperature" => "material_bed_temperature",
            "bed temperature layer 0" | "build plate temperature layer 0" => {
                "material_bed_temperature_layer_0"
            }
            "diameter" => "material_diameter",
            "density" => "material_density",
            "flow" => "material_flow",
            "adhesion tendency" => "material_adhesion_tendency",
            "surface energy" => "material_surface_energy",
            "shrinkage" => "material_shrinkage_percentage",
            "print cooling" | "part cooling fan speed" => "cool_fan_speed",
            "build volume temperature" => "build_volume_temperature",
            _ => continue,
        };
        if let Some(value) = parse_numeric(raw) {
            mapped.insert(target.to_string(), value);
        }
    }
    mapped
}

fn parse_numeric(raw: &str) -> Option<Value> {
    let value = raw.trim().parse::<f64>().ok()?;
    if !value.is_finite() {
        return None;
    }
    Number::from_f64(value).map(Value::Number)
}

fn local_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATERIAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fdmmaterial version="1.3" xmlns="http://www.ultimaker.com/material">
  <metadata>
    <name><brand>Generic</brand><material>PLA</material><color>Red</color></name>
    <GUID>abc-123</GUID>
  </metadata>
  <settings>
    <setting key="print temperature">210</setting>
    <setting key="bed temperature">60</setting>
    <setting key="diameter">1.75</setting>
    <setting key="density">1.24</setting>
    <setting key="unknown key">ignored</setting>
  </settings>
</fdmmaterial>
"#;

    #[test]
    fn parses_material_and_maps_settings() {
        let material = parse_material(MATERIAL, "/tmp/pla.xml.fdm_material").unwrap();
        assert_eq!(material.name, "Generic PLA");
        assert_eq!(material.brand.as_deref(), Some("Generic"));
        assert_eq!(material.material_type.as_deref(), Some("PLA"));
        assert_eq!(material.guid.as_deref(), Some("abc-123"));
        assert_eq!(
            material.mapped.get("material_print_temperature"),
            Some(&serde_json::json!(210.0))
        );
        assert_eq!(
            material.mapped.get("material_diameter"),
            Some(&serde_json::json!(1.75))
        );
        assert_eq!(material.density, Some(1.24));
        assert!(!material.mapped.contains_key("unknown key"));
    }
}

//! Printer-profile verification.
//!
//! Verification is intentionally conservative: hard errors are things
//! `CuraEngine` or the printer will reject (missing machine dimensions, absurd
//! temperatures), while warnings flag risky but printable configurations.

use serde::Serialize;
use serde_json::Value;

use crate::{
    profiles::{resolve::Resolution, types::Definition},
    safety::policy::PolicyConfig,
};

/// Issue severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// One verification finding.
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub severity: Severity,
    pub setting: Option<String>,
    pub message: String,
}

/// Verification result.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    pub ok: bool,
    pub definition: String,
    pub settings_checked: usize,
    pub errors: usize,
    pub warnings: usize,
    pub issues: Vec<Issue>,
    /// Resolution warnings (unevaluated expressions and similar).
    pub resolution_warnings: Vec<String>,
}

/// Verify a resolved profile against machine and safety rules.
#[must_use]
pub fn verify(
    definition: &Definition,
    resolution: &Resolution,
    policy: &PolicyConfig,
) -> VerificationReport {
    let values = &resolution.values;
    let mut issues = Vec::new();

    for key in [
        "machine_width",
        "machine_depth",
        "machine_height",
        "machine_extruder_count",
        "machine_nozzle_size",
        "layer_height",
        "line_width",
        "speed_print",
    ] {
        if !values.contains_key(key) {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some(key.to_string()),
                message: format!("required setting '{key}' is missing"),
            });
        }
    }

    let width = number(values, "machine_width");
    let depth = number(values, "machine_depth");
    let height = number(values, "machine_height");
    for (key, value) in [
        ("machine_width", width),
        ("machine_depth", depth),
        ("machine_height", height),
    ] {
        match value {
            Some(value) if value > 0.0 => {}
            Some(value) => issues.push(Issue {
                severity: Severity::Error,
                setting: Some(key.to_string()),
                message: format!("{key} must be positive, got {value}"),
            }),
            None => {}
        }
    }
    if let (Some(width), Some(depth), Some(height)) = (width, depth, height) {
        if width * depth * height > 2_000_000_000.0 {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: None,
                message: format!(
                    "build volume is very large ({width:.0} × {depth:.0} × {height:.0} mm); \
                     check the machine definition"
                ),
            });
        }
    }

    let extruders = number(values, "machine_extruder_count");
    if let Some(extruders) = extruders {
        if !(1.0..=16.0).contains(&extruders) {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("machine_extruder_count".into()),
                message: format!(
                    "machine_extruder_count must be between 1 and 16, got {extruders}"
                ),
            });
        }
    }

    let nozzle = number(values, "machine_nozzle_size");
    if let Some(nozzle) = nozzle {
        if !(0.05..=2.5).contains(&nozzle) {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("machine_nozzle_size".into()),
                message: format!(
                    "machine_nozzle_size must be between 0.05 and 2.5 mm, got {nozzle}"
                ),
            });
        }
    }

    let layer_height = number(values, "layer_height");
    if let (Some(layer), Some(nozzle)) = (layer_height, nozzle) {
        if layer <= 0.0 {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("layer_height".into()),
                message: "layer_height must be positive".into(),
            });
        } else if layer > nozzle {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("layer_height".into()),
                message: format!(
                    "layer_height {layer} mm is larger than the nozzle diameter {nozzle} mm"
                ),
            });
        } else if layer > nozzle * 0.8 {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: Some("layer_height".into()),
                message: format!(
                    "layer_height {layer} mm is above 80% of the nozzle diameter {nozzle} mm; \
                     extrusion may be unreliable"
                ),
            });
        }
    }

    let line_width = number(values, "line_width");
    if let (Some(line), Some(nozzle)) = (line_width, nozzle) {
        if line < 0.05 || line > nozzle * 3.0 {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("line_width".into()),
                message: format!("line_width {line} mm is not plausible for a {nozzle} mm nozzle"),
            });
        } else if line < nozzle * 0.5 || line > nozzle * 2.0 {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: Some("line_width".into()),
                message: format!(
                    "line_width {line} mm is outside the usual 0.5–2× nozzle range for a \
                     {nozzle} mm nozzle"
                ),
            });
        }
    }

    if let Some(speed) = number(values, "speed_print") {
        if !(1.0..=1000.0).contains(&speed) {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("speed_print".into()),
                message: format!("speed_print must be between 1 and 1000 mm/s, got {speed}"),
            });
        }
    }

    if let Some(temperature) = number(values, "material_print_temperature") {
        if temperature > policy.max_hotend_temp {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("material_print_temperature".into()),
                message: format!(
                    "hotend temperature {temperature} °C exceeds the policy limit of {} °C",
                    policy.max_hotend_temp
                ),
            });
        } else if temperature < 120.0 {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: Some("material_print_temperature".into()),
                message: format!("hotend temperature {temperature} °C is unusually low"),
            });
        }
    }

    if let Some(temperature) = number(values, "material_bed_temperature") {
        if temperature > policy.max_bed_temp {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("material_bed_temperature".into()),
                message: format!(
                    "bed temperature {temperature} °C exceeds the policy limit of {} °C",
                    policy.max_bed_temp
                ),
            });
        } else if temperature < 0.0 {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("material_bed_temperature".into()),
                message: "bed temperature must not be negative".into(),
            });
        }
        if temperature > 0.0 && bool_value(values, "machine_heated_bed") == Some(false) {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: Some("machine_heated_bed".into()),
                message:
                    "a bed temperature is set but the profile says the machine has no heated bed"
                        .into(),
            });
        }
    }

    if let Some(diameter) = number(values, "material_diameter") {
        if !(1.0..=4.0).contains(&diameter) {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: Some("material_diameter".into()),
                message: format!("filament diameter {diameter} mm is outside the usual 1.0–4.0 mm"),
            });
        }
    }

    if let Some(adhesion) = values.get("adhesion_type").and_then(Value::as_str) {
        if !matches!(adhesion, "skirt" | "brim" | "raft" | "none") {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("adhesion_type".into()),
                message: format!("unknown adhesion_type '{adhesion}'"),
            });
        }
    }

    if let Some(density) = number(values, "infill_sparse_density") {
        if !(0.0..=100.0).contains(&density) {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("infill_sparse_density".into()),
                message: format!("infill_sparse_density must be between 0 and 100, got {density}"),
            });
        }
    }

    if let Some(walls) = number(values, "wall_line_count") {
        if walls < 0.0 {
            issues.push(Issue {
                severity: Severity::Error,
                setting: Some("wall_line_count".into()),
                message: "wall_line_count must not be negative".into(),
            });
        }
    }

    for key in ["machine_start_gcode", "machine_end_gcode"] {
        if let Some(value) = values.get(key).and_then(Value::as_str) {
            if value.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Warning,
                    setting: Some(key.to_string()),
                    message: format!("{key} is empty; the print may not start or finish cleanly"),
                });
            }
        }
    }

    if let Some(flavor) = values.get("machine_gcode_flavor").and_then(Value::as_str) {
        let known = [
            "RepRap (Marlin/Sprinter)",
            "RepRap (Volumetric)",
            "RepRap (RepRap)",
            "UltiGCode",
            "Griffin",
            "Griffin+Cheetah",
            "Makerbot",
            "BFB",
            "Mach3",
            "Repetier",
            "BambuLab",
        ];
        if !known.contains(&flavor) {
            issues.push(Issue {
                severity: Severity::Warning,
                setting: Some("machine_gcode_flavor".into()),
                message: format!("unrecognized machine_gcode_flavor '{flavor}'"),
            });
        }
    }

    for warning in &resolution.warnings {
        issues.push(Issue {
            severity: Severity::Warning,
            setting: None,
            message: warning.clone(),
        });
    }

    let errors = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .count();
    let warnings = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Warning)
        .count();

    VerificationReport {
        ok: errors == 0,
        definition: definition.name.clone(),
        settings_checked: values.len(),
        errors,
        warnings,
        issues,
        resolution_warnings: resolution.warnings.clone(),
    }
}

fn number(values: &crate::profiles::types::Settings, key: &str) -> Option<f64> {
    values.get(key).and_then(|value| match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        Value::Bool(value) => Some(f64::from(u8::from(*value))),
        _ => None,
    })
}

fn bool_value(values: &crate::profiles::types::Settings, key: &str) -> Option<bool> {
    values.get(key).and_then(|value| match value {
        Value::Bool(value) => Some(*value),
        Value::String(text) => match text.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{
        resolve::ProfileLayer,
        types::{SettingDefinition, SettingType},
    };
    use std::collections::BTreeMap;

    fn definition() -> Definition {
        let mut settings = BTreeMap::new();
        for (key, default) in [
            ("machine_width", serde_json::json!(220)),
            ("machine_depth", serde_json::json!(220)),
            ("machine_height", serde_json::json!(250)),
            ("machine_extruder_count", serde_json::json!(1)),
            ("machine_nozzle_size", serde_json::json!(0.4)),
            ("layer_height", serde_json::json!(0.2)),
            ("line_width", serde_json::json!(0.4)),
            ("speed_print", serde_json::json!(50)),
            ("material_print_temperature", serde_json::json!(210)),
            ("material_bed_temperature", serde_json::json!(60)),
        ] {
            settings.insert(
                key.to_string(),
                SettingDefinition {
                    key: key.to_string(),
                    label: None,
                    description: None,
                    unit: None,
                    kind: SettingType::Float,
                    default_value: Some(default),
                    value_expression: None,
                    options: BTreeMap::new(),
                    minimum: None,
                    maximum: None,
                    minimum_warning: None,
                    maximum_warning: None,
                    enabled: None,
                    settable_per_mesh: false,
                    settable_per_extruder: false,
                    settable_globally: true,
                },
            );
        }
        Definition {
            name: "test".into(),
            path: "test.def.json".into(),
            inherits: None,
            metadata: Value::Null,
            settings,
            lineage: vec!["test".into()],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn valid_profile_passes() {
        let definition = definition();
        let resolution = crate::profiles::resolve::resolve(
            &definition,
            &[ProfileLayer::from_values(
                "quality",
                crate::profiles::types::Settings::new(),
            )],
            &crate::profiles::types::Settings::new(),
        );
        let report = verify(&definition, &resolution, &PolicyConfig::default());
        assert!(report.ok);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn excessive_temperature_fails() {
        let definition = definition();
        let mut values = crate::profiles::types::Settings::new();
        values.insert("material_print_temperature".into(), serde_json::json!(400));
        let resolution = crate::profiles::resolve::resolve(
            &definition,
            &[ProfileLayer::from_values("quality", values)],
            &crate::profiles::types::Settings::new(),
        );
        let report = verify(&definition, &resolution, &PolicyConfig::default());
        assert!(!report.ok);
        assert!(report.errors >= 1);
    }
}

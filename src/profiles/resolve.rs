//! Profile stack resolution and Cura expression evaluation.
//!
//! Cura values may be literals (`0.2`) or expressions (`=layer_height * 4`).
//! Expressions are evaluated with `meval` against the settings resolved so far.
//! Cura's own helper functions (`extruderValues`, `resolveOrValue`, ...) are not
//! supported; when one cannot be evaluated the definition default is kept and a
//! warning is recorded, rather than sending an expression string to `CuraEngine`.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use serde_json::{Map, Number, Value};

use crate::{
    profiles::types::{coerce_value, is_expression, Definition, Settings},
    safety::policy::PolicyConfig,
};

/// One layer of a profile stack.
#[derive(Debug, Clone)]
pub struct ProfileLayer {
    pub source: String,
    pub values: Settings,
}

impl ProfileLayer {
    /// Build a layer from an instance profile's values.
    #[must_use]
    pub fn from_values(source: impl Into<String>, values: Settings) -> Self {
        Self {
            source: source.into(),
            values,
        }
    }
}

/// Result of resolving a profile stack.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub values: Settings,
    pub warnings: Vec<String>,
    pub explicit: BTreeSet<String>,
    pub layers: Vec<String>,
}

impl Resolution {
    /// Values that can be sent to `CuraEngine`, with unresolved expressions
    /// removed.
    #[must_use]
    pub fn engine_values(&self) -> Settings {
        self.values
            .iter()
            .filter(|(_, value)| !is_expression(value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// Resolve definition defaults, profile layers, and caller overrides.
#[must_use]
pub fn resolve(
    definition: &Definition,
    layers: &[ProfileLayer],
    overrides: &Settings,
) -> Resolution {
    let mut values = definition.defaults();
    let mut explicit = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut layer_names = Vec::new();

    computed_pass(definition, &mut values, &explicit, 3);

    for layer in layers {
        layer_names.push(layer.source.clone());
        apply_values(
            definition,
            &mut values,
            &mut explicit,
            &mut warnings,
            &layer.source,
            &layer.values,
        );
    }

    computed_pass(definition, &mut values, &explicit, 3);

    apply_values(
        definition,
        &mut values,
        &mut explicit,
        &mut warnings,
        "caller overrides",
        overrides,
    );

    values.retain(|_, value| !is_expression(value));
    if warnings.len() > 200 {
        warnings.truncate(200);
        warnings.push("additional resolution warnings were suppressed".to_string());
    }

    Resolution {
        values,
        warnings,
        explicit,
        layers: layer_names,
    }
}

fn apply_values(
    definition: &Definition,
    values: &mut Settings,
    explicit: &mut BTreeSet<String>,
    warnings: &mut Vec<String>,
    source: &str,
    incoming: &Settings,
) {
    for (key, value) in incoming {
        let setting = definition.setting(key);
        let resolved = if is_expression(value) {
            let expression = value.as_str().unwrap_or_default();
            if let Some(value) = evaluate_expression(expression, values) {
                value
            } else {
                warnings.push(format!(
                    "{source}: expression for '{key}' could not be evaluated: {expression}"
                ));
                values.get(key).cloned().unwrap_or(Value::Null)
            }
        } else if let Some(text) = value.as_str() {
            coerce_value(setting, text)
        } else {
            value.clone()
        };
        if !resolved.is_null() {
            values.insert(key.clone(), resolved);
        }
        explicit.insert(key.clone());
    }
}

fn computed_pass(
    definition: &Definition,
    values: &mut Settings,
    explicit: &BTreeSet<String>,
    passes: usize,
) {
    for _ in 0..passes {
        let mut changed = false;
        for (key, setting) in &definition.settings {
            if explicit.contains(key) {
                continue;
            }
            let Some(expression) = &setting.value_expression else {
                continue;
            };
            if !is_expression(expression) {
                continue;
            }
            let text = expression.as_str().unwrap_or_default();
            if let Some(resolved) = evaluate_expression(text, values) {
                if values.get(key) != Some(&resolved) {
                    values.insert(key.clone(), resolved);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Evaluate a Cura `=...` expression against numeric settings.
#[must_use]
pub fn evaluate_expression(expression: &str, values: &Settings) -> Option<Value> {
    let expression = expression.trim().trim_start_matches('=');
    if expression.is_empty() {
        return None;
    }
    let parsed = meval::Expr::from_str(expression).ok()?;
    let mut context = meval::Context::new();
    for (key, value) in values {
        if let Some(number) = value.as_f64() {
            context.var(key, number);
        } else if let Some(boolean) = value.as_bool() {
            context.var(key, f64::from(u8::from(boolean)));
        }
    }
    let result = parsed.eval_with_context(&context).ok()?;
    if !result.is_finite() {
        return None;
    }
    Number::from_f64(result).map(Value::Number)
}

/// Return temperature-related settings that exceed the policy limits.
#[must_use]
pub fn temperature_violations(values: &Settings, policy: &PolicyConfig) -> Vec<String> {
    let mut violations = Vec::new();
    for (key, value) in values {
        let Some(number) = value.as_f64() else {
            continue;
        };
        let lower = key.to_ascii_lowercase();
        if lower.contains("temperature") && lower.contains("bed") && number > policy.max_bed_temp {
            violations.push(format!(
                "{key} = {number} °C exceeds the bed limit of {} °C",
                policy.max_bed_temp
            ));
        } else if (lower.contains("print_temperature")
            || lower.contains("standby_temperature")
            || lower.contains("hotend"))
            && number > policy.max_hotend_temp
        {
            violations.push(format!(
                "{key} = {number} °C exceeds the hotend limit of {} °C",
                policy.max_hotend_temp
            ));
        }
    }
    violations
}

/// Convert a JSON object into a settings map, rejecting non-objects.
pub fn settings_from_value(value: Value) -> crate::error::Result<Settings> {
    match value {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        _ => Err(crate::error::Error::InvalidArguments(
            "settings must be a JSON object".into(),
        )),
    }
}

/// Merge a map of settings into another, returning the merged map.
#[must_use]
pub fn merged(base: &Settings, extra: &Settings) -> Settings {
    let mut out = base.clone();
    for (key, value) in extra {
        out.insert(key.clone(), value.clone());
    }
    out
}

/// Group settings by Cura's `extruder.N` convention.
#[must_use]
pub fn extruder_map(
    overrides: &BTreeMap<u32, Settings>,
    base: &Settings,
) -> BTreeMap<String, Settings> {
    overrides
        .iter()
        .map(|(index, settings)| (format!("extruder.{index}"), merged(base, settings)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::types::SettingDefinition;

    fn definition() -> Definition {
        let mut settings = BTreeMap::new();
        for (key, default) in [
            ("layer_height", serde_json::json!(0.2)),
            ("machine_nozzle_size", serde_json::json!(0.4)),
            ("line_width", serde_json::json!(0.4)),
            ("speed_print", serde_json::json!(50)),
        ] {
            settings.insert(
                key.to_string(),
                SettingDefinition {
                    key: key.to_string(),
                    label: None,
                    description: None,
                    unit: None,
                    kind: crate::profiles::types::SettingType::Float,
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
    fn layers_override_defaults() {
        let definition = definition();
        let mut values = Settings::new();
        values.insert("layer_height".into(), serde_json::json!(0.12));
        values.insert("speed_print".into(), serde_json::json!("80"));
        let resolution = resolve(
            &definition,
            &[ProfileLayer::from_values("quality", values)],
            &Settings::new(),
        );
        assert_eq!(
            resolution.values.get("layer_height"),
            Some(&serde_json::json!(0.12))
        );
        assert_eq!(
            resolution.values.get("speed_print"),
            Some(&serde_json::json!(80.0))
        );
        assert!(resolution.explicit.contains("speed_print"));
    }

    #[test]
    fn evaluates_simple_expressions() {
        let mut values = Settings::new();
        values.insert("layer_height".into(), serde_json::json!(0.2));
        let value = evaluate_expression("=layer_height * 4", &values).unwrap();
        assert_eq!(value, serde_json::json!(0.8));
    }

    #[test]
    fn unsupported_expression_keeps_default_and_warns() {
        let definition = definition();
        let mut values = Settings::new();
        values.insert(
            "line_width".into(),
            Value::String("=resolveOrValue('line_width')".into()),
        );
        let resolution = resolve(
            &definition,
            &[ProfileLayer::from_values("custom", values)],
            &Settings::new(),
        );
        assert_eq!(
            resolution.values.get("line_width"),
            Some(&serde_json::json!(0.4))
        );
        assert!(resolution
            .warnings
            .iter()
            .any(|warning| warning.contains("line_width")));
    }

    #[test]
    fn temperature_limits_are_checked() {
        let mut values = Settings::new();
        values.insert("material_print_temperature".into(), serde_json::json!(400));
        values.insert("material_bed_temperature".into(), serde_json::json!(150));
        let violations = temperature_violations(&values, &PolicyConfig::default());
        assert_eq!(violations.len(), 2);
    }
}

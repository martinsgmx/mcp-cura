//! Argument parsing helpers for MCP tools.
//!
//! Schemas are JSON Schema, but decoding goes through this small typed helper so
//! unknown properties fail loudly and every error names the offending field.

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// A validated tool argument object.
#[derive(Debug, Clone)]
pub struct Args {
    map: Map<String, Value>,
}

impl Args {
    /// Wrap a JSON value, requiring an object and rejecting unknown keys.
    pub fn parse(value: Option<Value>, allowed: &[&str]) -> Result<Self> {
        let value = value.unwrap_or_else(|| Value::Object(Map::new()));
        let map = match value {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            _ => {
                return Err(Error::InvalidArguments(
                    "arguments must be a JSON object".to_string(),
                ));
            }
        };

        if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
            let mut accepted = allowed.to_vec();
            accepted.sort_unstable();
            return Err(Error::InvalidArguments(format!(
                "unknown argument '{key}'; accepted arguments: {}",
                if accepted.is_empty() {
                    "none".to_string()
                } else {
                    accepted.join(", ")
                }
            )));
        }
        Ok(Self { map })
    }

    /// The canonical JSON value, used for approval fingerprints and audit.
    #[must_use]
    pub fn into_value(self) -> Value {
        Value::Object(self.map)
    }

    /// Borrow the underlying value.
    #[must_use]
    pub fn as_value(&self) -> Value {
        Value::Object(self.map.clone())
    }

    /// Whether the field was supplied.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Raw value, if present and non-null.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.map.get(key).filter(|value| !value.is_null())
    }

    /// Optional string, trimmed. Empty strings are rejected.
    pub fn optional_string(&self, key: &str) -> Result<Option<String>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(Error::InvalidArguments(format!(
                        "'{key}' must not be empty"
                    )));
                }
                if trimmed.contains('\0') {
                    return Err(Error::InvalidArguments(format!(
                        "'{key}' must not contain NUL bytes"
                    )));
                }
                Ok(Some(trimmed.to_string()))
            }
            Some(_) => Err(Error::InvalidArguments(format!("'{key}' must be a string"))),
        }
    }

    /// Required string.
    pub fn required_string(&self, key: &str) -> Result<String> {
        self.optional_string(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional float, rejecting NaN and infinity.
    pub fn optional_f64(&self, key: &str) -> Result<Option<f64>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => match value.as_f64() {
                Some(number) if number.is_finite() => Ok(Some(number)),
                _ => Err(Error::InvalidArguments(format!(
                    "'{key}' must be a finite number"
                ))),
            },
        }
    }

    /// Required float.
    pub fn required_f64(&self, key: &str) -> Result<f64> {
        self.optional_f64(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional boolean.
    pub fn optional_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(Error::InvalidArguments(format!(
                "'{key}' must be a boolean"
            ))),
        }
    }

    /// Required boolean.
    pub fn required_bool(&self, key: &str) -> Result<bool> {
        self.optional_bool(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional unsigned integer.
    pub fn optional_u64(&self, key: &str) -> Result<Option<u64>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                Error::InvalidArguments(format!("'{key}' must be a non-negative integer"))
            }),
        }
    }

    /// Required unsigned integer.
    pub fn required_u64(&self, key: &str) -> Result<u64> {
        self.optional_u64(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional unsigned 32-bit integer.
    pub fn optional_u32(&self, key: &str) -> Result<Option<u32>> {
        self.optional_u64(key)?.map_or(Ok(None), |value| {
            u32::try_from(value)
                .map(Some)
                .map_err(|_| Error::InvalidArguments(format!("'{key}' is too large")))
        })
    }

    /// Optional array of non-empty strings.
    pub fn optional_string_list(&self, key: &str) -> Result<Option<Vec<String>>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => {
                if items.is_empty() {
                    return Err(Error::InvalidArguments(format!(
                        "'{key}' must contain at least one item"
                    )));
                }
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let text = item.as_str().ok_or_else(|| {
                        Error::InvalidArguments(format!("'{key}' must contain only strings"))
                    })?;
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        return Err(Error::InvalidArguments(format!(
                            "'{key}' must not contain empty strings"
                        )));
                    }
                    if trimmed.contains('\0') {
                        return Err(Error::InvalidArguments(format!(
                            "'{key}' must not contain NUL bytes"
                        )));
                    }
                    out.push(trimmed.to_string());
                }
                Ok(Some(out))
            }
            Some(_) => Err(Error::InvalidArguments(format!(
                "'{key}' must be an array of strings"
            ))),
        }
    }

    /// Required string list.
    pub fn required_string_list(&self, key: &str) -> Result<Vec<String>> {
        self.optional_string_list(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional JSON object, for Cura settings or per-extruder maps.
    pub fn optional_object(&self, key: &str) -> Result<Option<Map<String, Value>>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(map)) => Ok(Some(map.clone())),
            Some(_) => Err(Error::InvalidArguments(format!(
                "'{key}' must be a JSON object"
            ))),
        }
    }

    /// Required JSON object.
    pub fn required_object(&self, key: &str) -> Result<Map<String, Value>> {
        self.optional_object(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional per-extruder settings: `{ "0": {...}, "1": {...} }`.
    pub fn optional_extruder_settings(
        &self,
        key: &str,
    ) -> Result<Option<std::collections::BTreeMap<u32, Map<String, Value>>>> {
        let Some(object) = self.optional_object(key)? else {
            return Ok(None);
        };
        let mut out = std::collections::BTreeMap::new();
        for (name, value) in object {
            let index = name.parse::<u32>().map_err(|_| {
                Error::InvalidArguments(format!(
                    "'{key}' keys must be extruder indexes, got '{name}'"
                ))
            })?;
            let settings = value.as_object().ok_or_else(|| {
                Error::InvalidArguments(format!("'{key}.{name}' must be a JSON object"))
            })?;
            out.insert(index, settings.clone());
        }
        Ok(Some(out))
    }
}

/// Validate a path for import: absolute, under one of `roots`, no symlink escape.
pub fn validate_import_path(path: &str, roots: &[String]) -> Result<std::path::PathBuf> {
    let expanded = expand_tilde(path);
    let candidate = std::path::PathBuf::from(&expanded);
    if !candidate.is_absolute() {
        return Err(Error::InvalidArguments(
            "import path must be absolute".to_string(),
        ));
    }
    let canonical = candidate.canonicalize().map_err(|err| {
        Error::InvalidArguments(format!(
            "cannot read import path '{}': {err}",
            candidate.display()
        ))
    })?;
    if !canonical.is_file() {
        return Err(Error::InvalidArguments(format!(
            "import path '{}' is not a file",
            canonical.display()
        )));
    }
    let allowed = roots.iter().any(|root| {
        let root = std::path::PathBuf::from(expand_tilde(root));
        root.canonicalize()
            .is_ok_and(|root| canonical.starts_with(root))
    });
    if !allowed {
        return Err(Error::Policy(format!(
            "import path '{}' is outside the configured import roots",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Validate an export destination under one of `roots`. The destination need
/// not exist yet, so its parent is canonicalised instead.
pub fn validate_export_path(path: &str, roots: &[String]) -> Result<std::path::PathBuf> {
    let expanded = expand_tilde(path);
    let candidate = std::path::PathBuf::from(&expanded);
    if !candidate.is_absolute() {
        return Err(Error::InvalidArguments(
            "export path must be absolute".to_string(),
        ));
    }
    let parent = candidate.parent().ok_or_else(|| {
        Error::InvalidArguments("export path must have a parent directory".to_string())
    })?;
    std::fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize()?;
    let file_name = candidate.file_name().ok_or_else(|| {
        Error::InvalidArguments("export path must include a file name".to_string())
    })?;
    let canonical = canonical_parent.join(file_name);
    let allowed = roots.iter().any(|root| {
        let root = std::path::PathBuf::from(expand_tilde(root));
        root.canonicalize()
            .is_ok_and(|root| canonical.starts_with(root))
    });
    if !allowed {
        return Err(Error::Policy(format!(
            "export path '{}' is outside the configured export roots",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Expand a leading `~` using the current user's home directory.
#[must_use]
pub fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir().map_or_else(
            || path.to_string(),
            |home| home.to_string_lossy().to_string(),
        );
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

/// Validate a relative artifact/job file name.
pub fn validate_name(name: &str, what: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 160 {
        return Err(Error::InvalidArguments(format!(
            "{what} must be between 1 and 160 characters"
        )));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(Error::InvalidArguments(format!(
            "{what} must not contain path separators"
        )));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_unknown_keys() {
        let err = Args::parse(Some(json!({"tmeperature": 200})), &["temperature"]).unwrap_err();
        assert!(err.to_string().contains("unknown argument 'tmeperature'"));
    }

    #[test]
    fn accepts_null_arguments() {
        let args = Args::parse(Some(Value::Null), &["x"]).unwrap();
        assert!(!args.contains("x"));
    }

    #[test]
    fn validates_numbers() {
        let args = Args::parse(Some(json!({"t": 200})), &["t"]).unwrap();
        assert!((args.required_f64("t").unwrap() - 200.0).abs() < f64::EPSILON);
        let args = Args::parse(Some(json!({"t": "hot"})), &["t"]).unwrap();
        assert!(args
            .required_f64("t")
            .unwrap_err()
            .to_string()
            .contains("finite number"));
    }

    #[test]
    fn parses_extruder_settings() {
        let args = Args::parse(
            Some(json!({"extruders": {"0": {"speed_print": 40}, "1": {"speed_print": 30}}})),
            &["extruders"],
        )
        .unwrap();
        let extruders = args
            .optional_extruder_settings("extruders")
            .unwrap()
            .unwrap();
        assert_eq!(extruders.len(), 2);
        assert_eq!(extruders[&1]["speed_print"], json!(30));
    }
}

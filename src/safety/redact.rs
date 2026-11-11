//! Secret redaction for logs, audit records, and tool output.
//!
//! The server has few secrets, but paths and user-provided profile values can
//! still contain credentials. Redaction is deliberately conservative: keys that
//! look like secrets are replaced recursively, and URL userinfo is stripped.

use serde_json::Value;

const SECRET_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "credential",
    "private_key",
];

/// Return a redacted copy of a JSON value.
#[must_use]
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if is_secret_key(key) {
                        (key.clone(), Value::String("[REDACTED]".to_string()))
                    } else {
                        (key.clone(), redact_value(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

/// Strip userinfo from a URL so it can be logged.
#[must_use]
pub fn redact_url_userinfo(url: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(url) {
        if !parsed.username().is_empty() || parsed.password().is_some() {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
        }
        return parsed.to_string();
    }
    url.to_string()
}

/// Truncate a string for audit output, appending a marker when truncated.
#[must_use]
pub fn truncate_for_audit(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut end = max_len;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}…[truncated {} bytes]", &text[..end], text.len() - end)
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_nested_secrets() {
        let value = json!({
            "api_key": "hunter2",
            "nested": {"token": "abc", "filename": "part.stl"},
            "items": [{"password": "x"}]
        });
        let redacted = redact_value(&value);
        let text = redacted.to_string();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("\"abc\""));
        assert!(text.contains("part.stl"));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn strips_url_userinfo() {
        assert_eq!(
            redact_url_userinfo("http://user:pass@example.com:8080/x"),
            "http://example.com:8080/x"
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        let text = "aébc";
        let truncated = truncate_for_audit(text, 2);
        assert!(truncated.contains("truncated"));
        assert!(truncated.is_char_boundary(0));
    }
}

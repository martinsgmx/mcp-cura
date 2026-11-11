//! G-code parsing, verification, and preparation.
//!
//! `CuraEngine` emits Marlin-flavoured G-code. This module deliberately parses
//! line-oriented commands rather than interpreting motion: the goal is to give
//! an agent a reliable summary (time, filament, layers, temperatures), catch
//! dangerous or nonsensical commands, and add a safe shutdown sequence to a
//! file that is missing one.
//!
//! It never executes G-code. Everything is a text transformation over bytes that
//! already live in the artifact store.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    safety::policy::PolicyConfig,
};

/// One dangerous command found in a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DangerousCommand {
    /// 1-based line number.
    pub line: usize,
    pub command: String,
    pub reason: String,
    /// The original line, truncated for display.
    pub text: String,
}

/// Command and motion statistics for a G-code file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcodeStats {
    pub lines: usize,
    pub command_lines: usize,
    pub comment_lines: usize,
    pub blank_lines: usize,
    pub command_counts: BTreeMap<String, usize>,
    pub layers: usize,
    pub declared_layer_count: Option<usize>,
    pub min_z: Option<f64>,
    pub max_z: Option<f64>,
    pub estimated_time_secs: Option<u64>,
    pub filament_mm: Option<f64>,
    pub filament_mm3: Option<f64>,
    pub filament_grams: Option<f64>,
    pub max_hotend_temp: Option<f64>,
    pub max_bed_temp: Option<f64>,
    pub has_end_temperature_off: bool,
    pub has_disable_steppers: bool,
    pub dangerous_commands: Vec<DangerousCommand>,
}

impl GcodeStats {
    /// Whether any command in the file is considered dangerous.
    #[must_use]
    pub fn is_dangerous(&self) -> bool {
        !self.dangerous_commands.is_empty()
    }
}

/// Parse and summarise G-code text.
#[must_use]
pub fn analyze(text: &str) -> GcodeStats {
    let mut command_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines = 0usize;
    let mut command_lines = 0usize;
    let mut comment_lines = 0usize;
    let mut blank_lines = 0usize;
    let mut layers = 0usize;
    let mut declared_layer_count = None;
    let mut min_z: Option<f64> = None;
    let mut max_z: Option<f64> = None;
    let mut estimated_time_secs = None;
    let mut filament_mm = None;
    let mut filament_mm3 = None;
    let mut filament_grams = None;
    let mut max_hotend_temp: Option<f64> = None;
    let mut max_bed_temp: Option<f64> = None;
    let mut dangerous_commands = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        lines += 1;
        let line = raw.trim();
        if line.is_empty() {
            blank_lines += 1;
            continue;
        }
        if let Some(comment) = line.strip_prefix(';') {
            comment_lines += 1;
            parse_comment(
                comment,
                &mut layers,
                &mut declared_layer_count,
                &mut estimated_time_secs,
                &mut filament_mm,
                &mut filament_mm3,
                &mut filament_grams,
            );
            continue;
        }

        let command_part = line.split(';').next().unwrap_or(line).trim();
        if command_part.is_empty() {
            comment_lines += 1;
            continue;
        }
        command_lines += 1;

        let mut tokens = command_part.split_whitespace();
        let Some(word) = tokens.next() else {
            continue;
        };
        let command = normalize_command(word);
        if command.is_empty() {
            continue;
        }
        *command_counts.entry(command.clone()).or_insert(0) += 1;

        let parameters: BTreeMap<char, f64> = tokens
            .filter_map(|token| {
                let mut chars = token.chars();
                let letter = chars.next()?.to_ascii_uppercase();
                let number = chars.as_str().parse::<f64>().ok()?;
                number.is_finite().then_some((letter, number))
            })
            .collect();

        if let Some(reason) = dangerous_reason(&command) {
            dangerous_commands.push(DangerousCommand {
                line: index + 1,
                command: command.clone(),
                reason: reason.to_string(),
                text: truncate_line(line, 160),
            });
        }

        match command.as_str() {
            "M104" | "M109" => {
                if let Some(temp) = parameters.get(&'S').or_else(|| parameters.get(&'R')) {
                    if *temp > 0.0 {
                        max_hotend_temp = Some(max_hotend_temp.map_or(*temp, |old| old.max(*temp)));
                    }
                }
            }
            "M140" | "M190" => {
                if let Some(temp) = parameters.get(&'S').or_else(|| parameters.get(&'R')) {
                    if *temp > 0.0 {
                        max_bed_temp = Some(max_bed_temp.map_or(*temp, |old| old.max(*temp)));
                    }
                }
            }
            "G0" | "G1" => {
                if let Some(z) = parameters.get(&'Z') {
                    min_z = Some(min_z.map_or(*z, |old| old.min(*z)));
                    max_z = Some(max_z.map_or(*z, |old| old.max(*z)));
                }
            }
            _ => {}
        }
    }

    // Cura's end sequence turns both heaters off and disables steppers. Checking
    // the individual commands is more reliable than looking for a comment.
    let has_end_temperature_off = has_shutdown_temperature(text);
    let has_disable_steppers =
        command_counts.contains_key("M84") || command_counts.contains_key("M18");
    GcodeStats {
        lines,
        command_lines,
        comment_lines,
        blank_lines,
        command_counts,
        layers,
        declared_layer_count,
        min_z,
        max_z,
        estimated_time_secs,
        filament_mm,
        filament_mm3,
        filament_grams,
        max_hotend_temp,
        max_bed_temp,
        has_end_temperature_off,
        has_disable_steppers,
        dangerous_commands,
    }
}

fn parse_comment(
    comment: &str,
    layers: &mut usize,
    declared_layer_count: &mut Option<usize>,
    estimated_time_secs: &mut Option<u64>,
    filament_mm: &mut Option<f64>,
    filament_mm3: &mut Option<f64>,
    filament_grams: &mut Option<f64>,
) {
    let trimmed = comment.trim();
    if let Some(value) = trimmed.strip_prefix("LAYER:") {
        if value.trim().parse::<usize>().is_ok() {
            *layers += 1;
        }
    } else if let Some(value) = trimmed.strip_prefix("LAYER_COUNT:") {
        *declared_layer_count = value.trim().parse::<usize>().ok();
    } else if let Some(value) = trimmed.strip_prefix("TIME:") {
        *estimated_time_secs = value.trim().parse::<u64>().ok();
    } else if let Some(value) = trimmed.strip_prefix("Filament used:") {
        parse_filament(value.trim(), filament_mm, filament_mm3, filament_grams);
    } else if let Some(value) = trimmed.strip_prefix("Filament volume:") {
        // Older Cura versions report volume directly in mm^3.
        *filament_mm3 = value
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<f64>().ok());
    }
}

fn parse_filament(
    value: &str,
    filament_mm: &mut Option<f64>,
    filament_mm3: &mut Option<f64>,
    filament_grams: &mut Option<f64>,
) {
    // Examples:
    //   ";Filament used: 1.23456m"
    //   ";Filament used: 1.23456m, 3.7cm3"
    //   ";Filament used: 3.7g"
    for part in value.split(',') {
        let part = part.trim();
        if let Some(number) = part.strip_suffix("mm3") {
            *filament_mm3 = number.trim().parse::<f64>().ok().or(*filament_mm3);
        } else if let Some(number) = part.strip_suffix("cm3") {
            *filament_mm3 = number
                .trim()
                .parse::<f64>()
                .ok()
                .map(|value| value * 1000.0)
                .or(*filament_mm3);
        } else if let Some(number) = part.strip_suffix('m') {
            *filament_mm = number
                .trim()
                .parse::<f64>()
                .ok()
                .map(|value| value * 1000.0)
                .or(*filament_mm);
        } else if let Some(number) = part.strip_suffix("mm") {
            *filament_mm = number.trim().parse::<f64>().ok().or(*filament_mm);
        } else if let Some(number) = part.strip_suffix('g') {
            *filament_grams = number.trim().parse::<f64>().ok().or(*filament_grams);
        }
    }
}

fn has_shutdown_temperature(text: &str) -> bool {
    let mut hotend_off = false;
    let mut bed_off = false;
    for line in text.lines().rev().take(200) {
        let command = line
            .split(';')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let parameter = line
            .split(';')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .find_map(|token| {
                let mut chars = token.chars();
                let letter = chars.next()?.to_ascii_uppercase();
                (letter == 'S').then(|| chars.as_str().parse::<f64>().ok())?
            });
        match command.as_str() {
            "M104" | "M109" if parameter == Some(0.0) => hotend_off = true,
            "M140" | "M190" if parameter == Some(0.0) => bed_off = true,
            _ => {}
        }
        if hotend_off && bed_off {
            return true;
        }
    }
    false
}

fn normalize_command(word: &str) -> String {
    let upper = word.to_ascii_uppercase();
    if upper.starts_with('N') && upper[1..].chars().all(|ch| ch.is_ascii_digit()) {
        // Line-numbered G-code: the command follows the line number.
        return String::new();
    }
    if upper.starts_with('G') || upper.starts_with('M') || upper.starts_with('T') {
        upper
    } else {
        String::new()
    }
}

fn dangerous_reason(command: &str) -> Option<&'static str> {
    match command {
        "M112" => Some("emergency stop"),
        "M502" => Some("restores firmware defaults"),
        "M500" => Some("writes settings to EEPROM"),
        "M501" => Some("reloads settings from EEPROM"),
        "M851" => Some("changes the probe Z offset"),
        "M92" => Some("changes axis steps per millimetre"),
        "M203" | "M201" | "M204" | "M205" | "M220" | "M221" => {
            Some("changes motion or extrusion limits")
        }
        "M301" | "M304" | "M303" => Some("changes PID or hotend control parameters"),
        "M600" => Some("filament change macro"),
        "M0" | "M1" => Some("unconditional pause"),
        "M26" => Some("manually sets an SD card position"),
        "M23" | "M24" | "M25" | "M32" => Some("controls an SD card print"),
        "M997" | "M999" => Some("firmware update or reset"),
        _ => None,
    }
}

/// A verification finding.
#[derive(Debug, Clone, Serialize)]
pub struct GcodeIssue {
    pub severity: Severity,
    pub line: Option<usize>,
    pub command: Option<String>,
    pub message: String,
}

/// Severity used by G-code verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// Verification report for a G-code artifact.
#[derive(Debug, Clone, Serialize)]
pub struct GcodeReport {
    pub ok: bool,
    pub errors: usize,
    pub warnings: usize,
    pub issues: Vec<GcodeIssue>,
    pub stats: GcodeStats,
}

/// Verify a G-code file against policy limits and basic sanity rules.
#[must_use]
pub fn verify(text: &str, policy: &PolicyConfig) -> GcodeReport {
    let stats = analyze(text);
    let mut issues = Vec::new();

    if stats.lines == 0 {
        issues.push(GcodeIssue {
            severity: Severity::Error,
            line: None,
            command: None,
            message: "the file is empty".to_string(),
        });
    }
    if stats.command_lines == 0 {
        issues.push(GcodeIssue {
            severity: Severity::Error,
            line: None,
            command: None,
            message: "the file contains no commands".to_string(),
        });
    }
    if !stats.dangerous_commands.is_empty() {
        for command in &stats.dangerous_commands {
            issues.push(GcodeIssue {
                severity: Severity::Error,
                line: Some(command.line),
                command: Some(command.command.clone()),
                message: format!(
                    "dangerous command {} on line {}: {}",
                    command.command, command.line, command.reason
                ),
            });
        }
    }
    if let Some(temp) = stats.max_hotend_temp {
        if temp > policy.max_hotend_temp {
            issues.push(GcodeIssue {
                severity: Severity::Error,
                line: None,
                command: None,
                message: format!(
                    "hotend temperature {temp} °C exceeds the policy limit of {} °C",
                    policy.max_hotend_temp
                ),
            });
        }
    }
    if let Some(temp) = stats.max_bed_temp {
        if temp > policy.max_bed_temp {
            issues.push(GcodeIssue {
                severity: Severity::Error,
                line: None,
                command: None,
                message: format!(
                    "bed temperature {temp} °C exceeds the policy limit of {} °C",
                    policy.max_bed_temp
                ),
            });
        }
    }
    if !stats.has_end_temperature_off {
        issues.push(GcodeIssue {
            severity: Severity::Error,
            line: None,
            command: None,
            message: "heater shutdown commands (M104/M140 S0) were not found".to_string(),
        });
    }
    if !stats.has_disable_steppers {
        issues.push(GcodeIssue {
            severity: Severity::Warning,
            line: None,
            command: None,
            message: "stepper disable (M84) was not found".to_string(),
        });
    }
    if stats.estimated_time_secs.is_none() {
        issues.push(GcodeIssue {
            severity: Severity::Info,
            line: None,
            command: None,
            message: "no ;TIME estimate was found".to_string(),
        });
    }
    if stats.filament_mm.is_none() && stats.filament_mm3.is_none() {
        issues.push(GcodeIssue {
            severity: Severity::Info,
            line: None,
            command: None,
            message: "no filament usage comment was found".to_string(),
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

    GcodeReport {
        ok: errors == 0,
        errors,
        warnings,
        issues,
        stats,
    }
}

/// Options for preparing a file for a printer.
#[derive(Debug, Clone, Copy)]
pub struct PrepareOptions {
    /// Add `M104 S0`/`M140 S0` when the file does not shut heaters down.
    pub add_end_temperature: bool,
    /// Add a full safe end sequence when the file does not disable steppers.
    pub add_end_sequence: bool,
    /// Ensure the file ends with a newline.
    pub ensure_final_newline: bool,
}

impl Default for PrepareOptions {
    fn default() -> Self {
        Self {
            add_end_temperature: true,
            add_end_sequence: false,
            ensure_final_newline: true,
        }
    }
}

/// Result of preparing a G-code file.
#[derive(Debug, Clone, Serialize)]
pub struct PrepareResult {
    pub text: String,
    pub changes: Vec<String>,
    pub stats_before: GcodeStats,
    pub stats_after: GcodeStats,
}

/// Add safe shutdown commands and normalise the file ending.
pub fn prepare(text: &str, options: PrepareOptions) -> Result<PrepareResult> {
    if text.contains('\0') {
        return Err(Error::Gcode("G-code contains NUL bytes".to_string()));
    }
    let before = analyze(text);
    if before.command_lines == 0 {
        return Err(Error::Gcode(
            "cannot prepare a file without commands".to_string(),
        ));
    }

    let mut output = text.to_string();
    let mut changes = Vec::new();

    if options.add_end_temperature && !before.has_end_temperature_off {
        output.push_str("\n; mcp-cura: heater shutdown\nM104 S0\nM140 S0\n");
        changes.push("added M104/M140 S0 heater shutdown".to_string());
    }
    if options.add_end_sequence && !before.has_disable_steppers {
        output.push_str(
            "; mcp-cura: safe end sequence\n\
             M107\n\
             G91\n\
             G1 Z10 F300\n\
             G90\n\
             G28 X0 Y0\n\
             M84\n",
        );
        changes.push("added a safe end sequence with stepper disable".to_string());
    }
    if options.ensure_final_newline && !output.ends_with('\n') {
        output.push('\n');
        changes.push("added a final newline".to_string());
    }

    // A prepared file must not contain a dangerous command; refuse to launder
    // one by merely appending a shutdown sequence.
    let after = analyze(&output);
    if after.is_dangerous() {
        return Err(Error::Gcode(
            "refusing to prepare a file that contains dangerous commands; inspect it first"
                .to_string(),
        ));
    }

    Ok(PrepareResult {
        text: output,
        changes,
        stats_before: before,
        stats_after: after,
    })
}

fn truncate_line(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let truncated = text.chars().take(max).collect::<String>();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
;FLAVOR:Marlin
;TIME:1234
;Filament used: 2.5m
;LAYER_COUNT:2
M140 S60
M104 S200
G28
;LAYER:0
G1 Z0.2 F300
G1 X10 Y10 E1
;LAYER:1
G1 Z0.4
M104 S0
M140 S0
M84
";

    #[test]
    fn extracts_metadata() {
        let stats = analyze(SAMPLE);
        assert_eq!(stats.estimated_time_secs, Some(1234));
        assert_eq!(stats.filament_mm, Some(2500.0));
        assert_eq!(stats.layers, 2);
        assert_eq!(stats.declared_layer_count, Some(2));
        assert_eq!(stats.max_hotend_temp, Some(200.0));
        assert_eq!(stats.max_bed_temp, Some(60.0));
        assert!(stats.has_end_temperature_off);
        assert!(stats.has_disable_steppers);
        assert!(stats.max_z.unwrap() > 0.39);
        assert!(!stats.is_dangerous());
    }

    #[test]
    fn detects_dangerous_commands() {
        let stats = analyze("G28\nM502\nM500\n");
        assert_eq!(stats.dangerous_commands.len(), 2);
        assert_eq!(stats.dangerous_commands[0].command, "M502");
    }

    #[test]
    fn verify_flags_temperature_and_missing_shutdown() {
        let policy = PolicyConfig::default();
        let report = verify("M104 S400\nG28\n", &policy);
        assert!(!report.ok);
        assert!(report.errors >= 2);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.message.contains("hotend temperature")));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.message.contains("heater shutdown")));
    }

    #[test]
    fn prepare_adds_shutdown_once() {
        let result = prepare("G28\nG1 Z1\n", PrepareOptions::default()).unwrap();
        assert!(result.text.contains("M104 S0"));
        assert!(result.text.contains("M140 S0"));
        let again = prepare(&result.text, PrepareOptions::default()).unwrap();
        assert!(again.changes.is_empty());
    }

    #[test]
    fn prepare_refuses_dangerous_files() {
        assert!(prepare("M502\n", PrepareOptions::default()).is_err());
    }
}

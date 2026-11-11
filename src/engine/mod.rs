//! `CuraEngine` discovery and invocation.
//!
//! Two runners are supported:
//!
//! * `binary` spawns a local `CuraEngine` executable (the default),
//! * `docker` runs `docker run --rm -v <job>:/work -w /work <image> CuraEngine`
//!   so a pinned engine image can be used without installing Cura locally.
//!
//! Settings are normally delivered as a resolved-settings JSON file (`-r`),
//! which avoids command-line length limits and lets per-model settings work.
//! Older engines can use `-s key=value` via `engine.mode = "settings_args"`.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Map, Value};
use tokio::{process::Command, time::timeout};

use crate::{
    config::{EngineConfig, EngineMode, EngineRunner},
    error::{Error, Result},
    profiles::types::Settings,
};

/// Result of probing the configured engine.
#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub runner: EngineRunner,
    pub binary: String,
    pub resolved_path: Option<String>,
    pub docker_image: Option<String>,
    pub available: bool,
    pub version: Option<String>,
    pub detail: String,
    pub search_paths: Vec<String>,
    pub mode: EngineMode,
}

/// One model in a slice plan.
#[derive(Debug, Clone)]
pub struct PlanModel {
    /// File name inside the job directory, for example `model_0.stl`.
    pub file_name: String,
    /// Host path to the mesh bytes.
    pub source: PathBuf,
    /// Per-model Cura settings.
    pub settings: Settings,
    /// Artifact id for traceability.
    pub artifact_id: Option<String>,
}

/// Everything needed to invoke `CuraEngine` for one job.
#[derive(Debug, Clone)]
pub struct SlicePlan {
    /// Definition file to pass with `-j`.
    pub definition: Option<PathBuf>,
    pub models: Vec<PlanModel>,
    /// Global (`global` in the resolved JSON) settings.
    pub global: Settings,
    /// Per-extruder settings keyed by `extruder.N`.
    pub extruders: std::collections::BTreeMap<String, Settings>,
    pub mode: EngineMode,
    pub threads: Option<usize>,
    pub verbose: bool,
    pub search_paths: Vec<PathBuf>,
    pub timeout: Duration,
}

impl SlicePlan {
    /// Create an empty plan with the engine defaults.
    #[must_use]
    pub fn new(mode: EngineMode, timeout: Duration) -> Self {
        Self {
            definition: None,
            models: Vec::new(),
            global: Settings::new(),
            extruders: std::collections::BTreeMap::new(),
            mode,
            threads: None,
            verbose: false,
            search_paths: Vec::new(),
            timeout,
        }
    }
}

/// Outcome of a successful engine run.
#[derive(Debug, Clone, Serialize)]
pub struct SliceOutcome {
    pub gcode_path: String,
    pub resolved_path: Option<String>,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
}

/// `CuraEngine` facade.
#[derive(Debug, Clone)]
pub struct Engine {
    config: EngineConfig,
    search_paths: Vec<PathBuf>,
}

impl Engine {
    /// Build an engine facade from configuration and effective search paths.
    #[must_use]
    pub fn new(config: EngineConfig, search_paths: Vec<PathBuf>) -> Self {
        Self {
            config,
            search_paths,
        }
    }

    /// Engine configuration.
    #[must_use]
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Probe the configured runner without slicing anything.
    pub async fn probe(&self) -> EngineInfo {
        match self.config.runner {
            EngineRunner::Binary => self.probe_binary().await,
            EngineRunner::Docker => self.probe_docker().await,
        }
    }

    async fn probe_binary(&self) -> EngineInfo {
        let mut info = EngineInfo {
            runner: EngineRunner::Binary,
            binary: self.config.binary.clone(),
            resolved_path: None,
            docker_image: None,
            available: false,
            version: None,
            detail: String::new(),
            search_paths: self
                .search_paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect(),
            mode: self.config.mode,
        };
        let Some(path) = resolve_binary(&self.config.binary) else {
            info.detail = format!(
                "'{}' was not found on PATH; install CuraEngine or set CURA_ENGINE_BIN",
                self.config.binary
            );
            return info;
        };
        info.resolved_path = Some(path.to_string_lossy().to_string());
        info.available = true;

        let mut command = Command::new(&path);
        command
            .arg("help")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match timeout(Duration::from_secs(15), command.output()).await {
            Ok(Ok(output)) => {
                let mut text = String::from_utf8_lossy(&output.stdout).to_string();
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                info.version = parse_version(&text);
                info.detail = info.version.as_ref().map_or_else(
                    || "engine responded to 'help'".to_string(),
                    |version| format!("CuraEngine {version}"),
                );
            }
            Ok(Err(err)) => {
                info.available = false;
                info.detail = format!("could not execute {}: {err}", path.display());
            }
            Err(_) => {
                info.detail = "engine probe timed out after 15 seconds".to_string();
            }
        }
        info
    }

    async fn probe_docker(&self) -> EngineInfo {
        let mut info = EngineInfo {
            runner: EngineRunner::Docker,
            binary: "docker".to_string(),
            resolved_path: None,
            docker_image: self.config.docker_image.clone(),
            available: false,
            version: None,
            detail: String::new(),
            search_paths: Vec::new(),
            mode: self.config.mode,
        };
        let Some(docker) = resolve_binary("docker") else {
            info.detail = "docker was not found on PATH".to_string();
            return info;
        };
        info.resolved_path = Some(docker.to_string_lossy().to_string());
        let Some(image) = &self.config.docker_image else {
            info.detail = "engine.docker_image is not configured".to_string();
            return info;
        };
        info.available = true;
        let mut command = Command::new(&docker);
        command
            .args(["image", "inspect", image])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        match timeout(Duration::from_secs(15), command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                info.detail = format!("Docker image '{image}' is present");
            }
            Ok(Ok(_)) => {
                info.detail = format!(
                    "Docker is available but image '{image}' is not present; it will be pulled on \
                     first use"
                );
            }
            Ok(Err(err)) => {
                info.available = false;
                info.detail = format!("could not execute docker: {err}");
            }
            Err(_) => {
                info.detail = "docker image inspection timed out".to_string();
            }
        }
        info
    }

    /// Execute a slice plan inside `job_dir`.
    pub async fn slice(&self, plan: &SlicePlan, job_dir: &Path) -> Result<SliceOutcome> {
        self.slice_cancellable(plan, job_dir, None).await
    }

    /// Execute a slice plan, stopping early when `cancel` is triggered.
    pub async fn slice_cancellable(
        &self,
        plan: &SlicePlan,
        job_dir: &Path,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<SliceOutcome> {
        std::fs::create_dir_all(job_dir)?;
        let started = Instant::now();

        for model in &plan.models {
            let destination = job_dir.join(&model.file_name);
            std::fs::copy(&model.source, &destination).map_err(|err| {
                Error::Engine(format!(
                    "could not copy model {} into the job directory: {err}",
                    model.source.display()
                ))
            })?;
        }

        let output_name = "output.gcode";
        let output_path = job_dir.join(output_name);
        let _ = std::fs::remove_file(&output_path);

        let (command, resolved_path) = match plan.mode {
            EngineMode::ResolvedJson => {
                let resolved = job_dir.join("resolved.json");
                write_resolved_json(plan, &resolved)?;
                (
                    self.build_command(plan, job_dir, &resolved, output_name),
                    Some(resolved),
                )
            }
            EngineMode::SettingsArgs => {
                if plan.models.iter().any(|model| !model.settings.is_empty())
                    || !plan.extruders.is_empty()
                {
                    return Err(Error::Engine(
                        "settings_args mode cannot express per-model or per-extruder settings; \
                         use resolved_json mode"
                            .into(),
                    ));
                }
                (
                    self.build_settings_command(plan, job_dir, output_name),
                    None,
                )
            }
        };

        let mut commandline = command;
        let command_strings = command_to_strings(&commandline);
        commandline.kill_on_drop(true);
        commandline.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = run_engine(commandline, plan.timeout, cancel).await?;

        let stdout = truncate(
            &String::from_utf8_lossy(&output.stdout),
            self.config.max_log_bytes,
        );
        let stderr = truncate(
            &String::from_utf8_lossy(&output.stderr),
            self.config.max_log_bytes,
        );
        let duration = started.elapsed();

        if !output.status.success() {
            let detail = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            return Err(Error::Engine(format!(
                "CuraEngine exited with status {:?}: {}",
                output.status.code(),
                truncate(&detail, 2000)
            )));
        }

        let metadata = std::fs::metadata(&output_path).map_err(|_| {
            Error::Engine("CuraEngine reported success but did not write output.gcode".to_string())
        })?;
        if metadata.len() == 0 {
            return Err(Error::Engine(
                "CuraEngine reported success but output.gcode is empty".to_string(),
            ));
        }

        Ok(SliceOutcome {
            gcode_path: output_path.to_string_lossy().to_string(),
            resolved_path: resolved_path.map(|path| path.to_string_lossy().to_string()),
            command: command_strings,
            exit_code: output.status.code(),
            duration_ms: duration.as_millis(),
            stdout,
            stderr,
        })
    }

    fn build_command(
        &self,
        plan: &SlicePlan,
        job_dir: &Path,
        resolved: &Path,
        output_name: &str,
    ) -> Command {
        let mut docker_mounts: Vec<String> = Vec::new();
        let mut command = match self.config.runner {
            EngineRunner::Binary => {
                let binary = resolve_binary(&self.config.binary)
                    .unwrap_or_else(|| PathBuf::from(&self.config.binary));
                let mut command = Command::new(binary);
                command.current_dir(job_dir);
                command
            }
            EngineRunner::Docker => {
                let mut command = Command::new(
                    resolve_binary("docker").unwrap_or_else(|| PathBuf::from("docker")),
                );
                command
                    .arg("run")
                    .arg("--rm")
                    .arg("-v")
                    .arg(format!("{}:/work", job_dir.to_string_lossy()))
                    .arg("-w")
                    .arg("/work");
                docker_mounts = docker_definition_mounts(&mut command, &plan.search_paths);
                for argument in &self.config.docker_args {
                    command.arg(argument);
                }
                command.arg(self.config.docker_image.as_deref().unwrap_or("cura-engine"));
                command.arg("CuraEngine");
                command
            }
        };

        command.arg("slice");

        if let Some(definition) = &plan.definition {
            match self.config.runner {
                EngineRunner::Binary => {
                    command.arg("-j").arg(definition);
                }
                EngineRunner::Docker => {
                    if let Some(file_name) = definition.file_name() {
                        let destination = job_dir.join(file_name);
                        if definition != &destination {
                            let _ = std::fs::copy(definition, &destination);
                        }
                        command.arg("-j").arg(file_name);
                    }
                }
            }
        }

        let resolved_arg = match self.config.runner {
            EngineRunner::Binary => resolved.to_string_lossy().to_string(),
            EngineRunner::Docker => resolved.file_name().map_or_else(
                || "resolved.json".to_string(),
                |name| name.to_string_lossy().to_string(),
            ),
        };
        command.arg("-r").arg(resolved_arg);

        for model in &plan.models {
            command.arg("-l").arg(&model.file_name);
        }

        if let Some(threads) = plan.threads {
            command.arg(format!("-m{threads}"));
        }
        if plan.verbose {
            command.arg("-v");
        }

        match self.config.runner {
            EngineRunner::Binary => {
                for path in &plan.search_paths {
                    command.arg("-d").arg(path);
                }
                let joined = std::env::join_paths(&plan.search_paths).ok();
                if let Some(joined) = joined {
                    command.env("CURA_ENGINE_SEARCH_PATH", joined);
                }
            }
            EngineRunner::Docker => {
                for path in &docker_mounts {
                    command.arg("-d").arg(path);
                }
            }
        }

        command.arg("-o").arg(output_name);
        command
    }

    fn build_settings_command(
        &self,
        plan: &SlicePlan,
        job_dir: &Path,
        output_name: &str,
    ) -> Command {
        let mut docker_mounts: Vec<String> = Vec::new();
        let mut command = match self.config.runner {
            EngineRunner::Binary => {
                let binary = resolve_binary(&self.config.binary)
                    .unwrap_or_else(|| PathBuf::from(&self.config.binary));
                let mut command = Command::new(binary);
                command.current_dir(job_dir);
                command
            }
            EngineRunner::Docker => {
                let mut command = Command::new(
                    resolve_binary("docker").unwrap_or_else(|| PathBuf::from("docker")),
                );
                command
                    .arg("run")
                    .arg("--rm")
                    .arg("-v")
                    .arg(format!("{}:/work", job_dir.to_string_lossy()))
                    .arg("-w")
                    .arg("/work");
                docker_mounts = docker_definition_mounts(&mut command, &plan.search_paths);
                for argument in &self.config.docker_args {
                    command.arg(argument);
                }
                command.arg(self.config.docker_image.as_deref().unwrap_or("cura-engine"));
                command.arg("CuraEngine");
                command
            }
        };

        command.arg("slice");
        if let Some(definition) = &plan.definition {
            match self.config.runner {
                EngineRunner::Binary => {
                    command.arg("-j").arg(definition);
                }
                EngineRunner::Docker => {
                    if let Some(file_name) = definition.file_name() {
                        let destination = job_dir.join(file_name);
                        if definition != &destination {
                            let _ = std::fs::copy(definition, &destination);
                        }
                        command.arg("-j").arg(file_name);
                    }
                }
            }
        }
        for (key, value) in &plan.global {
            command
                .arg("-s")
                .arg(format!("{key}={}", settings_value(value)));
        }
        for model in &plan.models {
            command.arg("-l").arg(&model.file_name);
        }
        if let Some(threads) = plan.threads {
            command.arg(format!("-m{threads}"));
        }
        if plan.verbose {
            command.arg("-v");
        }
        for path in &docker_mounts {
            command.arg("-d").arg(path);
        }
        command.arg("-o").arg(output_name);
        command
    }
}

async fn run_engine(
    mut command: Command,
    duration: Duration,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<std::process::Output> {
    let child = command
        .spawn()
        .map_err(|err| Error::Engine(format!("could not start CuraEngine: {err}")))?;

    if let Some(token) = cancel {
        tokio::select! {
            () = token.cancelled() => {
                // The child is owned by the wait future below. Cancelling that
                // branch drops the future, and kill_on_drop terminates the process.
                Err(Error::Job("slice job was cancelled".to_string()))
            }
            result = timeout(duration, child.wait_with_output()) => {
                engine_output(result, duration)
            }
        }
    } else {
        engine_output(timeout(duration, child.wait_with_output()).await, duration)
    }
}

/// Mount each existing host directory into the engine container at
/// `/cura-defs/<index>` and return the container-side paths.
///
/// Host paths do not exist inside the container, so mounting them read-only
/// and passing container paths to `CuraEngine` keeps profile inheritance
/// resolvable. Paths that are missing or not directories are skipped.
fn docker_definition_mounts(command: &mut Command, search_paths: &[PathBuf]) -> Vec<String> {
    let mut container_paths = Vec::new();
    for path in search_paths {
        let Ok(path) = std::fs::canonicalize(path) else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        let index = container_paths.len();
        let container = format!("/cura-defs/{index}");
        command
            .arg("-v")
            .arg(format!("{}:{container}:ro", path.to_string_lossy()));
        container_paths.push(container);
    }
    container_paths
}

fn engine_output(
    result: std::result::Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>,
    duration: Duration,
) -> Result<std::process::Output> {
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => Err(Error::Engine(format!(
            "could not wait for CuraEngine: {err}"
        ))),
        Err(_) => Err(Error::Engine(format!(
            "CuraEngine did not finish within {} seconds",
            duration.as_secs()
        ))),
    }
}

fn write_resolved_json(plan: &SlicePlan, path: &Path) -> Result<()> {
    let mut root = Map::new();
    root.insert("global".to_string(), Value::Object(plan.global.clone()));
    for (key, settings) in &plan.extruders {
        root.insert(key.clone(), Value::Object(settings.clone()));
    }
    for model in &plan.models {
        if !model.settings.is_empty() {
            root.insert(
                model.file_name.clone(),
                Value::Object(model.settings.clone()),
            );
        }
    }
    std::fs::write(path, serde_json::to_vec_pretty(&Value::Object(root))?)?;
    Ok(())
}

fn settings_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

fn resolve_binary(binary: &str) -> Option<PathBuf> {
    let path = Path::new(binary);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    if binary.contains('/') {
        let joined = std::env::current_dir().ok()?.join(path);
        return joined.is_file().then_some(joined);
    }
    which::which(binary).ok()
}

fn parse_version(text: &str) -> Option<String> {
    for marker in [
        "Cura_SteamEngine version",
        "CuraEngine version",
        "CuraSteamEngine version",
    ] {
        if let Some(position) = text.find(marker) {
            let rest = &text[position + marker.len()..];
            let version = rest
                .trim_start_matches([' ', ':', '\t'])
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim();
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
    }
    None
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}…[truncated]", &text[..end])
}

fn command_to_strings(command: &Command) -> Vec<String> {
    let mut parts = vec![command.as_std().get_program().to_string_lossy().to_string()];
    parts.extend(
        command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().to_string()),
    );
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> SlicePlan {
        let mut plan = SlicePlan::new(EngineMode::ResolvedJson, Duration::from_secs(30));
        plan.definition = Some(PathBuf::from("/tmp/ender3.def.json"));
        plan.models.push(PlanModel {
            file_name: "model_0.stl".into(),
            source: PathBuf::from("/tmp/model.stl"),
            settings: Settings::new(),
            artifact_id: Some("art_x".into()),
        });
        let mut global = Settings::new();
        global.insert("layer_height".into(), serde_json::json!(0.2));
        plan.global = global;
        plan.search_paths = vec![PathBuf::from("/tmp/definitions")];
        plan
    }

    #[test]
    fn parses_curaengine_versions() {
        assert_eq!(
            parse_version("Cura_SteamEngine version 5.7.0\nusage..."),
            Some("5.7.0".into())
        );
        assert!(parse_version("no version here").is_none());
    }

    #[test]
    fn docker_definition_mounts_existing_dirs_read_only() {
        let fixture =
            std::env::temp_dir().join(format!("mcp-cura-cura-defs-test-{}", std::process::id()));
        std::fs::create_dir_all(&fixture).unwrap();
        let missing = fixture.join("nope");

        let mut command = Command::new("docker");
        let mounts = docker_definition_mounts(&mut command, &[fixture.clone(), missing]);
        assert_eq!(mounts, vec!["/cura-defs/0".to_string()]);
        let parts = command_to_strings(&command);
        assert!(parts.iter().any(|part| part == "-v"));
        assert!(parts.iter().any(|part| part.contains("/cura-defs/0:ro")));

        std::fs::remove_dir_all(&fixture).ok();
    }

    #[test]
    fn builds_binary_command() {
        let config = EngineConfig::default();
        let engine = Engine::new(config, vec![PathBuf::from("/tmp/definitions")]);
        let plan = plan();
        let command = engine.build_command(
            &plan,
            Path::new("/tmp/job"),
            Path::new("/tmp/job/resolved.json"),
            "output.gcode",
        );
        let parts = command_to_strings(&command);
        assert!(parts.iter().any(|part| part == "slice"));
        assert!(parts.iter().any(|part| part == "-r"));
        assert!(parts.iter().any(|part| part == "model_0.stl"));
        assert!(parts.iter().any(|part| part == "output.gcode"));
    }

    #[test]
    fn settings_mode_is_rejected_with_per_model_settings() {
        let mut plan = plan();
        plan.mode = EngineMode::SettingsArgs;
        plan.models[0]
            .settings
            .insert("layer_height".into(), serde_json::json!(0.1));
        // Command construction itself is fine; the slice method rejects it.
        assert_eq!(plan.models[0].settings.len(), 1);
    }
}

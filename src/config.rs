//! Configuration loading, normalization, and validation.
//!
//! Precedence, lowest to highest:
//!
//! 1. built-in safe defaults
//! 2. an optional TOML file (`--config` / `MCP_CURA_CONFIG`)
//! 3. environment variables
//! 4. command-line flags (applied by `main.rs`)
//!
//! Secrets are held in [`SecretString`] and never serialized by
//! [`Config::public_view`]. This server has no printer credentials; the only
//! secret is the optional HTTP bearer token.

use std::{
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// MCP transport to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Newline-delimited JSON-RPC over stdin/stdout. Best for local agents.
    #[default]
    Stdio,
    /// Streamable HTTP (with optional SSE compatibility).
    Http,
}

impl FromStr for TransportKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stdio" | "stdin" | "stdout" => Ok(Self::Stdio),
            "http" | "streamable-http" | "streamable_http" => Ok(Self::Http),
            other => Err(format!(
                "unknown transport '{other}' (expected 'stdio' or 'http')"
            )),
        }
    }
}

/// How destructive actions are approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// The prepare call returns a single-use phrase; the agent supplies it to
    /// the confirm tool. Convenient, but an autonomous agent can self-approve.
    #[default]
    InBand,
    /// A human must run `mcp-cura approve <id>` on the server host before the
    /// agent can confirm. Recommended for shared deployments.
    OutOfBand,
    /// Both an in-band phrase and a human approval are required.
    Both,
}

impl FromStr for ApprovalMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "in_band" | "in-band" | "inband" => Ok(Self::InBand),
            "out_of_band" | "out-of-band" | "outofband" => Ok(Self::OutOfBand),
            "both" => Ok(Self::Both),
            other => Err(format!(
                "unknown approval mode '{other}' (expected 'in_band', 'out_of_band' or 'both')"
            )),
        }
    }
}

/// How the engine receives resolved settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EngineMode {
    /// Write a resolved-settings JSON file and pass it with `-r`. This is the
    /// most robust mode for Cura 5.x and avoids command-line length limits.
    #[default]
    ResolvedJson,
    /// Pass every changed setting with `-s`. Useful for older `CuraEngine`
    /// builds that do not implement `-r`.
    SettingsArgs,
}

impl FromStr for EngineMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "resolved_json" | "resolved-json" | "json" | "r" => Ok(Self::ResolvedJson),
            "settings_args" | "settings-args" | "args" | "s" => Ok(Self::SettingsArgs),
            other => Err(format!(
                "unknown engine mode '{other}' (expected 'resolved_json' or 'settings_args')"
            )),
        }
    }
}

/// HTTP transport settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    /// Bind host. Defaults to loopback.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// Static bearer token. Required when binding to a non-loopback address.
    #[serde(skip_serializing)]
    pub token: Option<SecretString>,
    /// Extra `Host` header values accepted by DNS-rebinding protection.
    pub allowed_hosts: Vec<String>,
    /// Extra browser origins allowed by CORS.
    pub allowed_origins: Vec<String>,
    /// Explicitly allow unauthenticated non-loopback HTTP. Never use this on a
    /// network you do not fully control.
    pub allow_unauthenticated: bool,
    /// Streamable HTTP path.
    pub path: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8765,
            token: None,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            allow_unauthenticated: false,
            path: "/mcp".to_string(),
        }
    }
}

/// Server/transport settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub transport: TransportKind,
    pub http: HttpConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transport: TransportKind::Stdio,
            http: HttpConfig::default(),
        }
    }
}

/// Managed workspace and artifact store settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Root for artifacts, jobs, custom profiles, approvals, and temporary data.
    pub root: String,
    /// Filesystem roots from which `cura_artifact_import` may read. An empty
    /// list disables path-based imports; base64 imports still work.
    pub import_roots: Vec<String>,
    /// Filesystem roots into which `cura_artifact_export` may write.
    pub export_roots: Vec<String>,
    /// Maximum size accepted by a single import.
    pub max_import_bytes: usize,
    /// Maximum size of any artifact in the store.
    pub max_artifact_bytes: usize,
    /// Maximum number of artifacts retained.
    pub max_artifacts: usize,
    /// Number of job records to keep on disk.
    pub keep_jobs: usize,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: default_workspace_root(),
            import_roots: vec![
                "~/Downloads".to_string(),
                "~/Documents".to_string(),
                "~/Desktop".to_string(),
                "~/models".to_string(),
            ],
            export_roots: vec![
                "~/Downloads".to_string(),
                "~/Documents".to_string(),
                "~/Desktop".to_string(),
            ],
            max_import_bytes: 512 * 1024 * 1024,
            max_artifact_bytes: 1024 * 1024 * 1024,
            max_artifacts: 10_000,
            keep_jobs: 100,
        }
    }
}

/// How `CuraEngine` is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EngineRunner {
    /// Spawn a local `CuraEngine` executable.
    #[default]
    Binary,
    /// Run `CuraEngine` inside a Docker image. The job directory is mounted at
    /// `/work`.
    Docker,
}

impl FromStr for EngineRunner {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "binary" | "local" | "exec" => Ok(Self::Binary),
            "docker" | "container" => Ok(Self::Docker),
            other => Err(format!(
                "unknown engine runner '{other}' (expected 'binary' or 'docker')"
            )),
        }
    }
}

/// `CuraEngine` discovery and execution settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    /// Executable name or absolute path. Resolved with `PATH` when relative.
    pub binary: String,
    /// Run the engine locally or through Docker.
    pub runner: EngineRunner,
    /// Docker image containing `CuraEngine` (runner = docker).
    pub docker_image: Option<String>,
    /// Additional arguments passed to `docker run` before the image name.
    pub docker_args: Vec<String>,
    /// Extra definition search paths passed with `-d` and
    /// `CURA_ENGINE_SEARCH_PATH`.
    pub search_paths: Vec<String>,
    /// Optional definitions directory. Added to `search_paths`.
    pub definitions: Option<String>,
    /// Optional `CuraEngine` thread count (`-m<threads>`).
    pub threads: Option<usize>,
    /// Hard wall-clock timeout for a slice.
    pub timeout_secs: u64,
    /// Maximum captured engine log bytes per stream.
    pub max_log_bytes: usize,
    /// How resolved settings are delivered to the engine.
    pub mode: EngineMode,
    /// Add `-v` to engine invocations.
    pub verbose: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            binary: "CuraEngine".to_string(),
            runner: EngineRunner::Binary,
            docker_image: None,
            docker_args: Vec::new(),
            search_paths: Vec::new(),
            definitions: None,
            threads: None,
            timeout_secs: 900,
            max_log_bytes: 1024 * 1024,
            mode: EngineMode::ResolvedJson,
            verbose: false,
        }
    }
}

/// Cura profile discovery settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilesConfig {
    /// Directories recursively scanned for `.def.json`, `.inst.cfg`, and
    /// `.xml.fdm_material` files.
    pub dirs: Vec<String>,
    /// Discover common Cura installation locations when `dirs` is empty.
    pub auto_discover: bool,
    /// Maximum number of indexed profiles.
    pub max_profiles: usize,
}

impl Default for ProfilesConfig {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            auto_discover: true,
            max_profiles: 20_000,
        }
    }
}

/// Local safety policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    /// Deny every non-read tool, including slicing and workspace writes.
    pub read_only: bool,
    /// Allow tools that create artifacts or write custom profiles.
    pub allow_workspace_writes: bool,
    /// Allow spawning `CuraEngine`.
    pub allow_slicing: bool,
    /// Allow copying artifacts outside the workspace.
    pub allow_export: bool,
    /// Allow deleting artifacts (still requires approval).
    pub allow_delete: bool,
    /// Hard limit for hotend temperatures found in profiles and G-code.
    pub max_hotend_temp: f64,
    /// Hard limit for bed temperatures found in profiles and G-code.
    pub max_bed_temp: f64,
    /// Maximum input mesh triangle count for geometry operations.
    pub max_triangles: usize,
    /// Maximum estimated voxel cells for voxel operations.
    pub max_voxel_cells: usize,
    /// Maximum simultaneously running slice jobs.
    pub max_concurrent_jobs: usize,
    /// Hard wall-clock timeout for a slice job.
    pub max_slice_secs: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            read_only: false,
            allow_workspace_writes: true,
            allow_slicing: true,
            allow_export: true,
            allow_delete: true,
            max_hotend_temp: 320.0,
            max_bed_temp: 130.0,
            max_triangles: 5_000_000,
            max_voxel_cells: 64_000_000,
            max_concurrent_jobs: 2,
            max_slice_secs: 900,
        }
    }
}

/// Double-confirmation settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalConfig {
    pub mode: ApprovalMode,
    /// How long a prepared action remains valid.
    pub ttl_secs: u64,
    /// Directory for approval state. Empty means `<workspace.root>/state`.
    pub state_dir: String,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            mode: ApprovalMode::InBand,
            ttl_secs: 120,
            state_dir: String::new(),
        }
    }
}

/// Logging and audit settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// `tracing` filter, e.g. `info` or `mcp_cura=debug`.
    pub level: String,
    /// Optional JSON-lines audit log. When unset, audit events go to stderr.
    pub audit_log: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            audit_log: None,
        }
    }
}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub workspace: WorkspaceConfig,
    pub engine: EngineConfig,
    pub profiles: ProfilesConfig,
    pub policy: PolicyConfig,
    pub approval: ApprovalConfig,
    pub logging: LoggingConfig,
}

impl Config {
    /// Load defaults, then an optional TOML file, then environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut config = Self::default();

        let config_path = path
            .map(Path::to_path_buf)
            .or_else(|| env_str("MCP_CURA_CONFIG").map(PathBuf::from));

        if let Some(config_path) = config_path {
            if config_path.exists() {
                let text = std::fs::read_to_string(&config_path).map_err(|err| {
                    Error::Config(format!(
                        "cannot read config file {}: {err}",
                        config_path.display()
                    ))
                })?;
                config = toml::from_str(&text)?;
            } else if path.is_some() {
                return Err(Error::Config(format!(
                    "config file not found: {}",
                    config_path.display()
                )));
            }
        }

        config.apply_env()?;
        config.normalize()?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self) -> Result<()> {
        if let Some(value) = env_str("MCP_CURA_TRANSPORT") {
            self.server.transport = value.parse().map_err(Error::Config)?;
        }
        if let Some(value) = env_str("MCP_CURA_HTTP_HOST") {
            self.server.http.host = value;
        }
        if let Some(value) = env_u64("MCP_CURA_HTTP_PORT")? {
            self.server.http.port = u16::try_from(value)
                .map_err(|_| Error::Config("HTTP port must be between 1 and 65535".into()))?;
        }
        if let Some(value) = env_str("MCP_CURA_HTTP_TOKEN") {
            self.server.http.token = Some(SecretString::from(value));
        }
        if let Some(value) = env_list("MCP_CURA_HTTP_ALLOWED_HOSTS") {
            self.server.http.allowed_hosts = value;
        }
        if let Some(value) = env_list("MCP_CURA_HTTP_ALLOWED_ORIGINS") {
            self.server.http.allowed_origins = value;
        }
        if let Some(value) = env_bool("MCP_CURA_HTTP_ALLOW_UNAUTHENTICATED")? {
            self.server.http.allow_unauthenticated = value;
        }
        if let Some(value) = env_str("MCP_CURA_HTTP_PATH") {
            self.server.http.path = value;
        }

        if let Some(value) = env_str("MCP_CURA_WORKSPACE") {
            self.workspace.root = value;
        }
        if let Some(value) = env_list("MCP_CURA_IMPORT_ROOTS") {
            self.workspace.import_roots = value;
        }
        if let Some(value) = env_list("MCP_CURA_EXPORT_ROOTS") {
            self.workspace.export_roots = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_IMPORT_BYTES")? {
            self.workspace.max_import_bytes = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_ARTIFACT_BYTES")? {
            self.workspace.max_artifact_bytes = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_ARTIFACTS")? {
            self.workspace.max_artifacts = value;
        }
        if let Some(value) = env_usize("MCP_CURA_KEEP_JOBS")? {
            self.workspace.keep_jobs = value;
        }

        if let Some(value) = env_str("MCP_CURA_ENGINE_BINARY") {
            self.engine.binary = value;
        }
        if let Some(value) = env_str("CURA_ENGINE_BIN") {
            self.engine.binary = value;
        }
        if let Some(value) = env_str("MCP_CURA_ENGINE_RUNNER") {
            self.engine.runner = value.parse().map_err(Error::Config)?;
        }
        if let Some(value) = env_str("MCP_CURA_DOCKER_IMAGE") {
            self.engine.docker_image = Some(value);
        }
        if let Some(value) = env_list("MCP_CURA_DOCKER_ARGS") {
            self.engine.docker_args = value;
        }
        if let Ok(value) = env::var("CURA_ENGINE_SEARCH_PATH") {
            self.engine.search_paths.extend(
                env::split_paths(&value)
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.to_string_lossy().to_string()),
            );
        }
        if let Some(value) = env_str("MCP_CURA_ENGINE_DEFINITIONS") {
            self.engine.definitions = Some(value);
        }
        if let Some(value) = env_usize("CURA_ENGINE_THREADS")? {
            self.engine.threads = Some(value);
        }
        if let Some(value) = env_u64("MCP_CURA_ENGINE_TIMEOUT_SECS")? {
            self.engine.timeout_secs = value;
        }
        if let Some(value) = env_usize("MCP_CURA_ENGINE_MAX_LOG_BYTES")? {
            self.engine.max_log_bytes = value;
        }
        if let Some(value) = env_str("MCP_CURA_ENGINE_MODE") {
            self.engine.mode = value.parse().map_err(Error::Config)?;
        }
        if let Some(value) = env_bool("MCP_CURA_ENGINE_VERBOSE")? {
            self.engine.verbose = value;
        }

        if let Some(value) = env_list("MCP_CURA_PROFILE_DIRS") {
            self.profiles.dirs = value;
        }
        if let Some(value) = env_bool("MCP_CURA_PROFILE_AUTO_DISCOVER")? {
            self.profiles.auto_discover = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_PROFILES")? {
            self.profiles.max_profiles = value;
        }

        if let Some(value) = env_bool("MCP_CURA_READ_ONLY")? {
            self.policy.read_only = value;
        }
        if let Some(value) = env_bool("MCP_CURA_ALLOW_WORKSPACE_WRITES")? {
            self.policy.allow_workspace_writes = value;
        }
        if let Some(value) = env_bool("MCP_CURA_ALLOW_SLICING")? {
            self.policy.allow_slicing = value;
        }
        if let Some(value) = env_bool("MCP_CURA_ALLOW_EXPORT")? {
            self.policy.allow_export = value;
        }
        if let Some(value) = env_bool("MCP_CURA_ALLOW_DELETE")? {
            self.policy.allow_delete = value;
        }
        if let Some(value) = env_f64("MCP_CURA_MAX_HOTEND_TEMP")? {
            self.policy.max_hotend_temp = value;
        }
        if let Some(value) = env_f64("MCP_CURA_MAX_BED_TEMP")? {
            self.policy.max_bed_temp = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_TRIANGLES")? {
            self.policy.max_triangles = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_VOXEL_CELLS")? {
            self.policy.max_voxel_cells = value;
        }
        if let Some(value) = env_usize("MCP_CURA_MAX_CONCURRENT_JOBS")? {
            self.policy.max_concurrent_jobs = value;
        }
        if let Some(value) = env_u64("MCP_CURA_MAX_SLICE_SECS")? {
            self.policy.max_slice_secs = value;
        }

        if let Some(value) = env_str("MCP_CURA_APPROVAL_MODE") {
            self.approval.mode = value.parse().map_err(Error::Config)?;
        }
        if let Some(value) = env_u64("MCP_CURA_APPROVAL_TTL_SECS")? {
            self.approval.ttl_secs = value;
        }
        if let Some(value) = env_str("MCP_CURA_STATE_DIR") {
            self.approval.state_dir = value;
        }

        if let Some(value) = env_str("MCP_CURA_LOG_LEVEL") {
            self.logging.level = value;
        }
        if let Some(value) = env_str("MCP_CURA_AUDIT_LOG") {
            self.logging.audit_log = Some(value);
        }

        Ok(())
    }

    fn normalize(&mut self) -> Result<()> {
        self.workspace.root = absolute_path(&expand_tilde(&self.workspace.root))?;
        self.workspace.import_roots = normalize_paths(&mut self.workspace.import_roots);
        self.workspace.export_roots = normalize_paths(&mut self.workspace.export_roots);

        self.engine.search_paths = normalize_paths(&mut self.engine.search_paths);
        if let Some(definitions) = self.engine.definitions.as_mut() {
            *definitions = expand_tilde(definitions);
        }

        self.profiles.dirs = normalize_paths(&mut self.profiles.dirs);
        if self.profiles.dirs.is_empty() && self.profiles.auto_discover {
            self.profiles.dirs = discover_profile_dirs();
        }

        if self.approval.state_dir.trim().is_empty() {
            self.approval.state_dir = PathBuf::from(&self.workspace.root)
                .join("state")
                .to_string_lossy()
                .to_string();
        } else {
            self.approval.state_dir = absolute_path(&expand_tilde(&self.approval.state_dir))?;
        }
        if let Some(path) = self.logging.audit_log.as_mut() {
            *path = absolute_path(&expand_tilde(path))?;
        }

        self.server.http.path = normalize_path(&self.server.http.path)?;
        self.server.http.allowed_hosts = normalize_list(&mut self.server.http.allowed_hosts);
        self.server.http.allowed_origins = normalize_list(&mut self.server.http.allowed_origins);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.workspace.root.trim().is_empty() {
            return Err(Error::Config("workspace.root must not be empty".into()));
        }
        if !(1024..=8 * 1024 * 1024 * 1024).contains(&self.workspace.max_import_bytes) {
            return Err(Error::Config(
                "workspace.max_import_bytes must be between 1 KiB and 8 GiB".into(),
            ));
        }
        if self.workspace.max_artifact_bytes < self.workspace.max_import_bytes {
            return Err(Error::Config(
                "workspace.max_artifact_bytes must be at least max_import_bytes".into(),
            ));
        }
        if self.workspace.max_artifacts == 0 {
            return Err(Error::Config(
                "workspace.max_artifacts must be positive".into(),
            ));
        }
        if self.workspace.keep_jobs == 0 {
            return Err(Error::Config("workspace.keep_jobs must be positive".into()));
        }

        if self.engine.binary.trim().is_empty() {
            return Err(Error::Config("engine.binary must not be empty".into()));
        }
        if self.engine.runner == EngineRunner::Docker
            && self
                .engine
                .docker_image
                .as_deref()
                .is_none_or(|image| image.trim().is_empty())
        {
            return Err(Error::Config(
                "engine.docker_image must be set when engine.runner = docker".into(),
            ));
        }
        if self.engine.timeout_secs == 0 || self.engine.timeout_secs > 86_400 {
            return Err(Error::Config(
                "engine.timeout_secs must be between 1 and 86400".into(),
            ));
        }
        if self.engine.max_log_bytes < 4096 || self.engine.max_log_bytes > 64 * 1024 * 1024 {
            return Err(Error::Config(
                "engine.max_log_bytes must be between 4 KiB and 64 MiB".into(),
            ));
        }
        if let Some(threads) = self.engine.threads {
            if threads == 0 || threads > 256 {
                return Err(Error::Config(
                    "engine.threads must be between 1 and 256".into(),
                ));
            }
        }

        if self.profiles.max_profiles == 0 {
            return Err(Error::Config(
                "profiles.max_profiles must be positive".into(),
            ));
        }

        if self.approval.ttl_secs < 5 || self.approval.ttl_secs > 3600 {
            return Err(Error::Config(
                "approval.ttl_secs must be between 5 and 3600".into(),
            ));
        }
        if self.server.http.port == 0 {
            return Err(Error::Config("HTTP port must not be 0".into()));
        }
        if !(0.0..=500.0).contains(&self.policy.max_hotend_temp) {
            return Err(Error::Config(
                "policy.max_hotend_temp must be between 0 and 500 °C".into(),
            ));
        }
        if !(0.0..=200.0).contains(&self.policy.max_bed_temp) {
            return Err(Error::Config(
                "policy.max_bed_temp must be between 0 and 200 °C".into(),
            ));
        }
        if self.policy.max_triangles == 0 || self.policy.max_triangles > 50_000_000 {
            return Err(Error::Config(
                "policy.max_triangles must be between 1 and 50,000,000".into(),
            ));
        }
        if self.policy.max_voxel_cells < 1_000_000 || self.policy.max_voxel_cells > 500_000_000 {
            return Err(Error::Config(
                "policy.max_voxel_cells must be between 1,000,000 and 500,000,000".into(),
            ));
        }
        if self.policy.max_concurrent_jobs == 0 || self.policy.max_concurrent_jobs > 32 {
            return Err(Error::Config(
                "policy.max_concurrent_jobs must be between 1 and 32".into(),
            ));
        }
        if self.policy.max_slice_secs == 0 || self.policy.max_slice_secs > 86_400 {
            return Err(Error::Config(
                "policy.max_slice_secs must be between 1 and 86400".into(),
            ));
        }

        if self.server.transport == TransportKind::Http {
            let host = self.server.http.host.trim();
            let is_loopback = host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback());
            if !is_loopback
                && self.server.http.token.is_none()
                && !self.server.http.allow_unauthenticated
            {
                return Err(Error::Config(
                    "refusing to bind HTTP to a non-loopback address without \
                     MCP_CURA_HTTP_TOKEN; set a bearer token or explicitly set \
                     MCP_CURA_HTTP_ALLOW_UNAUTHENTICATED=true"
                        .into(),
                ));
            }
        }

        Ok(())
    }

    /// Re-run normalization and validation after programmatic overrides.
    pub fn revalidate(&mut self) -> Result<()> {
        self.normalize()?;
        self.validate()
    }

    /// Engine timeout as a `Duration`.
    #[must_use]
    pub fn engine_timeout(&self) -> Duration {
        Duration::from_secs(self.engine.timeout_secs)
    }

    /// Approval TTL as a `Duration`.
    #[must_use]
    pub fn approval_ttl(&self) -> Duration {
        Duration::from_secs(self.approval.ttl_secs)
    }

    /// Workspace root as a `PathBuf`.
    #[must_use]
    pub fn workspace_root(&self) -> PathBuf {
        PathBuf::from(&self.workspace.root)
    }

    /// State directory as a `PathBuf`.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        PathBuf::from(&self.approval.state_dir)
    }

    /// Audit log path, if configured.
    #[must_use]
    pub fn audit_log_path(&self) -> Option<PathBuf> {
        self.logging.audit_log.as_ref().map(PathBuf::from)
    }

    /// Effective engine search paths, including the definitions directory.
    #[must_use]
    pub fn engine_search_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .engine
            .search_paths
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if let Some(definitions) = &self.engine.definitions {
            paths.push(PathBuf::from(definitions));
        }
        paths.extend(self.profiles.dirs.iter().map(PathBuf::from));
        dedupe_paths(paths)
    }

    /// Secret-free view for `doctor`, logs, and docs.
    #[must_use]
    pub fn public_view(&self) -> serde_json::Value {
        serde_json::json!({
            "server": {
                "transport": self.server.transport,
                "http": {
                    "host": self.server.http.host,
                    "port": self.server.http.port,
                    "path": self.server.http.path,
                    "token_configured": self.server.http.token.is_some(),
                    "allowed_hosts": self.server.http.allowed_hosts,
                    "allowed_origins": self.server.http.allowed_origins,
                    "allow_unauthenticated": self.server.http.allow_unauthenticated,
                }
            },
            "workspace": {
                "root": self.workspace.root,
                "import_roots": self.workspace.import_roots,
                "export_roots": self.workspace.export_roots,
                "max_import_bytes": self.workspace.max_import_bytes,
                "max_artifact_bytes": self.workspace.max_artifact_bytes,
                "max_artifacts": self.workspace.max_artifacts,
                "keep_jobs": self.workspace.keep_jobs,
            },
            "engine": {
                "binary": self.engine.binary,
                "runner": self.engine.runner,
                "docker_image": self.engine.docker_image,
                "docker_args": self.engine.docker_args,
                "search_paths": self.engine.search_paths,
                "definitions": self.engine.definitions,
                "threads": self.engine.threads,
                "timeout_secs": self.engine.timeout_secs,
                "max_log_bytes": self.engine.max_log_bytes,
                "mode": self.engine.mode,
                "verbose": self.engine.verbose,
            },
            "profiles": self.profiles,
            "policy": self.policy,
            "approval": {
                "mode": self.approval.mode,
                "ttl_secs": self.approval.ttl_secs,
                "state_dir": self.approval.state_dir,
            },
            "logging": self.logging,
        })
    }
}

fn normalize_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::Config("HTTP path must not be empty".into()));
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    if with_slash.len() > 128 {
        return Err(Error::Config("HTTP path is too long".into()));
    }
    Ok(with_slash)
}

fn normalize_list(values: &mut Vec<String>) -> Vec<String> {
    values
        .drain(..)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_paths(values: &mut Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values.drain(..) {
        let expanded = expand_tilde(value.trim());
        if expanded.is_empty() {
            continue;
        }
        if let Ok(path) = absolute_path(&expanded) {
            out.push(path);
        } else {
            out.push(expanded);
        }
    }
    dedupe_strings(out)
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

fn dedupe_paths(values: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

fn expand_tilde(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "~" {
        return dirs::home_dir().map_or_else(
            || trimmed.to_string(),
            |home| home.to_string_lossy().to_string(),
        );
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    trimmed.to_string()
}

fn absolute_path(path: &str) -> Result<String> {
    let path = PathBuf::from(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir().map_err(Error::Io)?.join(path)
    };
    Ok(absolute.to_string_lossy().to_string())
}

fn default_workspace_root() -> String {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .map_or_else(|| PathBuf::from(".mcp-cura"), |dir| dir.join("mcp-cura"))
        .to_string_lossy()
        .to_string()
}

fn discover_profile_dirs() -> Vec<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(search) = env::var("CURA_ENGINE_SEARCH_PATH") {
        candidates.extend(env::split_paths(&search));
    }

    for base in [
        "/usr/share/cura/resources",
        "/usr/local/share/cura/resources",
        "/opt/cura/share/cura/resources",
        "/opt/cura/resources",
        "/opt/cura-engine/share/cura/resources",
    ] {
        candidates.push(PathBuf::from(base));
    }

    if let Some(data) = dirs::data_dir() {
        candidates.push(data.join("cura"));
    }
    if let Some(config) = dirs::config_dir() {
        candidates.push(config.join("cura"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/share/cura"));
        candidates.push(home.join("Cura/resources"));
    }

    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("resources"));
        candidates.push(cwd.join("profiles"));
        candidates.push(cwd.join(".cura-engine/share/cura/resources"));
    }

    let mut found = Vec::new();
    for candidate in candidates {
        if candidate.is_dir() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn env_list(key: &str) -> Option<Vec<String>> {
    env_str(key).map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect()
    })
}

fn env_bool(key: &str) -> Result<Option<bool>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(Error::Config(format!(
            "{key} must be a boolean (true/false), got '{value}'"
        ))),
    }
}

fn env_u64(key: &str) -> Result<Option<u64>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| Error::Config(format!("{key} must be an unsigned integer, got '{value}'")))
}

fn env_usize(key: &str) -> Result<Option<usize>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| Error::Config(format!("{key} must be an unsigned integer, got '{value}'")))
}

fn env_f64(key: &str) -> Result<Option<f64>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| Error::Config(format!("{key} must be a number, got '{value}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe() {
        let config = Config::default();
        assert_eq!(config.server.transport, TransportKind::Stdio);
        assert!(!config.policy.read_only);
        assert!(config.policy.allow_slicing);
        assert!(config.policy.allow_workspace_writes);
        assert_eq!(config.approval.mode, ApprovalMode::InBand);
        assert!(config.server.http.token.is_none());
        assert!(!config.public_view().to_string().contains("hunter2"));
    }

    #[test]
    fn rejects_non_loopback_http_without_token() {
        let mut config = Config::default();
        config.server.transport = TransportKind::Http;
        config.server.http.host = "0.0.0.0".into();
        assert!(config.validate().is_err());
        config.server.http.token = Some(SecretString::from("secret"));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn normalizes_paths_and_lists() {
        let mut config = Config::default();
        config.server.http.path = "mcp".into();
        config.server.http.allowed_hosts = vec![" a ".into(), String::new()];
        config.workspace.import_roots = vec![" ~/Downloads ".into()];
        config.normalize().unwrap();
        assert_eq!(config.server.http.path, "/mcp");
        assert_eq!(config.server.http.allowed_hosts, vec!["a"]);
        assert!(config.workspace.import_roots[0].contains("Downloads"));
    }

    #[test]
    fn parses_approval_and_engine_modes() {
        assert_eq!(
            "in_band".parse::<ApprovalMode>().unwrap(),
            ApprovalMode::InBand
        );
        assert_eq!(
            "out-of-band".parse::<ApprovalMode>().unwrap(),
            ApprovalMode::OutOfBand
        );
        assert_eq!("both".parse::<ApprovalMode>().unwrap(), ApprovalMode::Both);
        assert!("nope".parse::<ApprovalMode>().is_err());
        assert_eq!(
            "resolved-json".parse::<EngineMode>().unwrap(),
            EngineMode::ResolvedJson
        );
        assert_eq!(
            "settings-args".parse::<EngineMode>().unwrap(),
            EngineMode::SettingsArgs
        );
    }
}

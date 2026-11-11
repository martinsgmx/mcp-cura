//! Command-line interface for mcp-cura.
//!
//! Running with no subcommand starts the stdio server, so an MCP client can be
//! configured with just the executable path. Operational subcommands cover
//! engine/profile diagnostics and out-of-band approval decisions.
//!
//! stdout is reserved for MCP messages in server mode and for command output in
//! every other mode. Diagnostics and tracing always go to stderr.

use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Args, Parser, Subcommand};
use mcp_cura::{
    config::{Config, EngineRunner, TransportKind},
    error::{Error, Result},
    safety::{approval::ApprovalStore, Decision, Policy},
    server::{server_details, CuraServerHandler},
    tools::{Gate, ToolContext, ToolRegistry},
    transport,
};

/// Starter configuration written by `mcp-cura init`. It contains no secrets.
const STARTER_CONFIG: &str = r#"# mcp-cura configuration
#
# Precedence: built-in defaults < this file < environment variables < CLI flags.
# Keep secrets out of this file. Use MCP_CURA_HTTP_TOKEN for the HTTP transport.

[server]
# "stdio" is the zero-friction default for local agents.
transport = "stdio"

[server.http]
host = "127.0.0.1"
port = 8765
path = "/mcp"
allow_unauthenticated = false

[workspace]
# Artifacts, jobs, custom profiles, approvals, and temporary files live here.
root = "~/.local/share/mcp-cura"
max_import_bytes = 536870912
max_artifact_bytes = 1073741824
max_artifacts = 10000
keep_jobs = 100

[engine]
# Runner: "binary" (local CuraEngine) or "docker" (pinned image).
runner = "binary"
binary = "CuraEngine"
# docker_image = "cura-engine:5.14.0"
timeout_secs = 900
mode = "resolved_json"

[profiles]
# Empty dirs plus auto_discover finds common Cura installations.
dirs = []
auto_discover = true
max_profiles = 20000

[policy]
read_only = false
allow_workspace_writes = true
allow_slicing = true
allow_export = true
allow_delete = true
max_hotend_temp = 320.0
max_bed_temp = 130.0
max_triangles = 5000000
max_voxel_cells = 64000000
max_concurrent_jobs = 2
max_slice_secs = 900

[approval]
# in_band: the agent receives a single-use phrase.
# out_of_band: a human must run `mcp-cura approve <id>`.
# both: phrase plus human approval (recommended for shared hosts).
mode = "in_band"
ttl_secs = 120

[logging]
level = "info"
"#;

#[derive(Debug, Parser)]
#[command(
    name = "mcp-cura",
    version,
    about = "Structured CuraEngine slicing MCP server",
    long_about = "A Model Context Protocol server for CuraEngine. It manages mesh preparation, Cura \
                  profile resolution and verification, background slice jobs, and G-code review. \
                  Mesh and G-code data stay in a content-addressed workspace; no printer is \
                  contacted and no G-code is executed.\n\nRunning without a subcommand starts the \
                  stdio server, which is what most MCP clients need.",
    propagate_version = true
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, global = true, env = "MCP_CURA_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the MCP server (default when no subcommand is given).
    Serve(ServeArgs),
    /// Check configuration, workspace, engine, and profiles.
    Doctor(DoctorArgs),
    /// Mark a high-risk action as human-approved (out-of-band approval).
    Approve {
        /// Approval id from the prepare response, for example `apr_0123...`.
        approval_id: String,
    },
    /// Cancel a high-risk action so it can never run.
    Deny {
        /// Approval id from the prepare response.
        approval_id: String,
    },
    /// List prepared high-risk actions.
    Pending(PendingArgs),
    /// Print the fully resolved, secret-free configuration.
    PrintConfig(PrintConfigArgs),
    /// List MCP tools, their risk, and their authorization path.
    Tools(ToolsArgs),
    /// Write a starter configuration file.
    Init(InitArgs),
}

#[derive(Debug, Args, Default)]
struct ServeArgs {
    /// Transport: stdio (default) or http.
    #[arg(long, value_name = "TRANSPORT")]
    transport: Option<String>,

    /// HTTP bind host (only used with --transport http).
    #[arg(long, value_name = "HOST")]
    http_host: Option<String>,

    /// HTTP bind port (only used with --transport http).
    #[arg(long, value_name = "PORT")]
    http_port: Option<u16>,

    /// `CuraEngine` executable name or absolute path.
    #[arg(long, value_name = "BIN")]
    engine_bin: Option<String>,

    /// Engine runner: binary or docker.
    #[arg(long, value_name = "RUNNER")]
    engine_runner: Option<String>,

    /// Docker image containing `CuraEngine` (with --engine-runner docker).
    #[arg(long, value_name = "IMAGE")]
    docker_image: Option<String>,

    /// Workspace root.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,

    /// Deny every non-read tool.
    #[arg(long)]
    read_only: bool,

    /// Enable workspace writes (imports and new artifacts).
    #[arg(long)]
    allow_workspace_writes: bool,

    /// Enable slicing and mesh processing.
    #[arg(long)]
    allow_slicing: bool,

    /// Enable artifact export.
    #[arg(long)]
    allow_export: bool,

    /// Enable artifact deletion (still approval-gated).
    #[arg(long)]
    allow_delete: bool,

    /// Approval model: `in_band`, `out_of_band`, or both.
    #[arg(long, value_name = "MODE")]
    approval_mode: Option<String>,

    /// Directory for approval state.
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    /// Tracing filter, for example `info` or `mcp_cura=debug`.
    #[arg(long, value_name = "FILTER")]
    log_level: Option<String>,
}

impl ServeArgs {
    fn apply(&self, config: &mut Config) -> Result<()> {
        if let Some(transport) = &self.transport {
            config.server.transport = transport.parse().map_err(Error::Config)?;
        }
        if let Some(host) = &self.http_host {
            config.server.http.host.clone_from(host);
        }
        if let Some(port) = self.http_port {
            config.server.http.port = port;
        }
        if let Some(binary) = &self.engine_bin {
            config.engine.binary.clone_from(binary);
        }
        if let Some(runner) = &self.engine_runner {
            config.engine.runner = runner.parse::<EngineRunner>().map_err(Error::Config)?;
        }
        if let Some(image) = &self.docker_image {
            config.engine.docker_image = Some(image.clone());
        }
        if let Some(workspace) = &self.workspace {
            config.workspace.root = workspace.to_string_lossy().to_string();
        }
        if self.read_only {
            config.policy.read_only = true;
        }
        if self.allow_workspace_writes {
            config.policy.allow_workspace_writes = true;
        }
        if self.allow_slicing {
            config.policy.allow_slicing = true;
        }
        if self.allow_export {
            config.policy.allow_export = true;
        }
        if self.allow_delete {
            config.policy.allow_delete = true;
        }
        if let Some(mode) = &self.approval_mode {
            config.approval.mode = mode.parse().map_err(Error::Config)?;
        }
        if let Some(dir) = &self.state_dir {
            config.approval.state_dir = dir.to_string_lossy().to_string();
        }
        if let Some(level) = &self.log_level {
            config.logging.level.clone_from(level);
        }
        config.revalidate()
    }
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Emit a machine-readable JSON report.
    #[arg(long)]
    json: bool,

    /// Exit non-zero when there are warnings, not only failures.
    #[arg(long)]
    strict: bool,
}

#[derive(Debug, Args)]
struct PendingArgs {
    /// Include consumed, denied, and expired records.
    #[arg(long)]
    all: bool,

    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct PrintConfigArgs {
    /// Emit JSON instead of TOML.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ToolsArgs {
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Destination file, or '-' for stdout.
    #[arg(long, short, value_name = "FILE", default_value = "mcp-cura.toml")]
    output: String,

    /// Overwrite an existing file.
    #[arg(long)]
    force: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("mcp-cura: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Some(Command::Tools(args)) => Ok(list_tools(&args)),
        Some(Command::Init(args)) => init_config(&args),
        Some(command) => run_with_config(cli.config.as_deref(), command).await,
        None => run_with_config(cli.config.as_deref(), Command::Serve(ServeArgs::default())).await,
    }
}

async fn run_with_config(config_path: Option<&Path>, command: Command) -> Result<ExitCode> {
    let mut config = Config::load(config_path)?;
    match command {
        Command::Serve(args) => {
            args.apply(&mut config)?;
            init_tracing(&config.logging.level);
            serve(&config).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor(args) => {
            init_tracing(&config.logging.level);
            doctor(&config, &args).await
        }
        Command::Approve { approval_id } => {
            init_tracing(&config.logging.level);
            approve(&config, &approval_id)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Deny { approval_id } => {
            init_tracing(&config.logging.level);
            deny(&config, &approval_id)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Pending(args) => {
            init_tracing(&config.logging.level);
            pending(&config, &args)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::PrintConfig(args) => print_config(&config, &args),
        Command::Tools(_) | Command::Init(_) => unreachable!("handled before config load"),
    }
}

/// Initialise tracing with stderr as the only sink.
///
/// This is essential: stdout is the MCP protocol channel in stdio mode.
fn init_tracing(level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

async fn serve(config: &Config) -> Result<()> {
    let context = Arc::new(ToolContext::new(config.clone())?);
    let registry = Arc::new(ToolRegistry::build());
    let handler = CuraServerHandler::new(registry, context);

    tracing::info!(
        target: "mcp_cura",
        version = env!("CARGO_PKG_VERSION"),
        transport = ?config.server.transport,
        workspace = %config.workspace.root,
        engine = %config.engine.binary,
        "starting mcp-cura server"
    );

    let details = server_details();
    match config.server.transport {
        TransportKind::Stdio => transport::run_stdio(config, handler, details).await,
        TransportKind::Http => transport::run_http(config, handler, details).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: CheckStatus,
    detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Warn,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Fail,
            detail: detail.into(),
        }
    }
}

async fn doctor(config: &Config, args: &DoctorArgs) -> Result<ExitCode> {
    let mut checks = Vec::new();
    let context = match ToolContext::new(config.clone()) {
        Ok(context) => Some(Arc::new(context)),
        Err(err) => {
            checks.push(Check::fail(
                "local state",
                format!(
                    "cannot initialise workspace or state directory: {}",
                    err.public_message()
                ),
            ));
            None
        }
    };

    checks.push(Check::ok(
        "configuration",
        format!(
            "transport={:?}, engine_runner={:?}, engine_mode={:?}, approval={:?}",
            config.server.transport, config.engine.runner, config.engine.mode, config.approval.mode
        ),
    ));

    if let Some(context) = &context {
        match context.engine.probe().await {
            info if info.available => match &info.version {
                Some(version) => checks.push(Check::ok(
                    "engine",
                    format!("CuraEngine {version} ({:?})", info.runner),
                )),
                None => checks.push(Check::warn(
                    "engine",
                    format!("engine found but version is unknown: {}", info.detail),
                )),
            },
            info => checks.push(Check::warn(
                "engine",
                format!("{} (slicing will fail until configured)", info.detail),
            )),
        }

        match context.profiles.catalog() {
            Ok(catalog) => {
                if catalog.entries.is_empty() {
                    checks.push(Check::warn(
                        "profiles",
                        "no definitions or profiles found; set MCP_CURA_PROFILE_DIRS",
                    ));
                } else {
                    checks.push(Check::ok(
                        "profiles",
                        format!(
                            "{} definitions, {} profiles across {} directories{}",
                            catalog.definitions.len(),
                            catalog.entries.len(),
                            catalog.scanned_dirs.len(),
                            if catalog.warnings.is_empty() {
                                String::new()
                            } else {
                                format!(" ({} scan warnings)", catalog.warnings.len())
                            }
                        ),
                    ));
                }
            }
            Err(err) => checks.push(Check::fail(
                "profiles",
                format!("cannot scan profiles: {}", err.public_message()),
            )),
        }

        checks.push(Check::ok(
            "workspace",
            format!(
                "{} (artifact limit {}, max import {})",
                context.store.root().display(),
                config.workspace.max_artifacts,
                crate::human_size(config.workspace.max_import_bytes as u64)
            ),
        ));

        match context.approvals.list_open() {
            Ok(open) => checks.push(Check::ok(
                "approvals",
                format!(
                    "mode={:?}, ttl={}s, {} pending",
                    config.approval.mode,
                    config.approval.ttl_secs,
                    open.len()
                ),
            )),
            Err(err) => checks.push(Check::warn(
                "approvals",
                format!("cannot read approval state: {}", err.public_message()),
            )),
        }
    }

    let registry = ToolRegistry::build();
    let policy = Policy::new(config.policy.clone());
    let mut immediate = 0usize;
    let mut approval = 0usize;
    let mut denied = 0usize;
    let mut internal = 0usize;
    for name in registry.names() {
        let Some(spec) = registry.spec(name) else {
            continue;
        };
        match spec.gate {
            Gate::Internal | Gate::Confirm => internal += 1,
            Gate::Policy => match policy.evaluate(spec.capability, spec.risk) {
                Decision::Allow => immediate += 1,
                Decision::NeedsApproval => approval += 1,
                Decision::Deny(_) => denied += 1,
            },
        }
    }
    checks.push(Check::ok(
        "tools",
        format!(
            "{} registered: {immediate} immediate, {approval} approval-gated, {denied} denied by \
             policy, {internal} local",
            registry.names().len()
        ),
    ));

    checks.push(Check::ok(
        "audit log",
        config
            .audit_log_path()
            .map_or_else(|| "stderr".to_string(), |path| path.display().to_string()),
    ));

    let failures = checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    let warnings = checks
        .iter()
        .filter(|check| check.status == CheckStatus::Warn)
        .count();
    let success = failures == 0 && (!args.strict || warnings == 0);

    if args.json {
        let report = serde_json::json!({
            "ok": success,
            "summary": {
                "ok": checks.iter().filter(|check| check.status == CheckStatus::Ok).count(),
                "warnings": warnings,
                "failures": failures,
            },
            "checks": checks.iter().map(|check| serde_json::json!({
                "name": check.name,
                "status": check.status.label(),
                "detail": check.detail,
            })).collect::<Vec<_>>(),
            "config": config.public_view(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("mcp-cura doctor");
        println!("===============");
        for check in &checks {
            let marker = match check.status {
                CheckStatus::Ok => "OK  ",
                CheckStatus::Warn => "WARN",
                CheckStatus::Fail => "FAIL",
            };
            println!("[{marker}] {:<16} {}", check.name, check.detail);
        }
        println!();
        if success {
            println!("Result: OK ({warnings} warning(s))");
        } else {
            println!("Result: FAILED ({failures} failure(s), {warnings} warning(s))");
        }
    }

    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn approval_store(config: &Config) -> Result<ApprovalStore> {
    ApprovalStore::new(
        config.state_dir(),
        config.approval_ttl(),
        config.approval.mode,
    )
}

fn approve(config: &Config, approval_id: &str) -> Result<()> {
    let store = approval_store(config)?;
    let record = store.human_approve(approval_id)?;
    println!("Approved {}.", record.id);
    println!("Tool:    {}", record.tool);
    println!("Summary: {}", record.summary);
    println!("Risk:    {}", record.risk.as_str());
    if store.requires_phrase() {
        println!("Phrase:  {}", record.phrase);
        println!();
        println!(
            "The agent must now call cura_confirm_action with this phrase and acknowledge=true."
        );
    } else {
        println!();
        println!("The agent may now call cura_confirm_action with acknowledge=true.");
    }
    Ok(())
}

fn deny(config: &Config, approval_id: &str) -> Result<()> {
    let store = approval_store(config)?;
    let record = store.deny(approval_id)?;
    println!("Denied {}.", record.id);
    println!("Tool:    {}", record.tool);
    println!("Summary: {}", record.summary);
    println!("The action cannot be executed.");
    Ok(())
}

fn pending(config: &Config, args: &PendingArgs) -> Result<()> {
    let store = approval_store(config)?;
    let records = if args.all {
        store.list()?
    } else {
        store.list_open()?
    };

    if args.json {
        let output = serde_json::json!({
            "count": records.len(),
            "mode": config.approval.mode,
            "approvals": records.iter().map(|record| serde_json::json!({
                "approval_id": record.id,
                "tool": record.tool,
                "summary": record.summary,
                "risk": record.risk.as_str(),
                "status": record.status,
                "created_at": record.created_at,
                "expires_at": record.expires_at,
                "expires_in_secs": record.remaining_secs(),
                "human_approved": record.human_approved_at.is_some(),
                "result": record.result,
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    if records.is_empty() {
        println!("No pending approvals.");
        return Ok(());
    }

    println!(
        "{} approval(s) ({:?} mode):",
        records.len(),
        config.approval.mode
    );
    for record in &records {
        println!();
        println!("  {}", record.id);
        println!("    tool:    {}", record.tool);
        println!("    summary: {}", record.summary);
        println!("    risk:    {}", record.risk.as_str());
        println!("    status:  {:?}", record.status);
        if record.is_open() {
            println!("    expires: {}s", record.remaining_secs());
            println!("    approve: mcp-cura approve {}", record.id);
            println!("    deny:    mcp-cura deny {}", record.id);
        }
        if let Some(result) = &record.result {
            println!("    result:  {result}");
        }
    }
    Ok(())
}

fn print_config(config: &Config, args: &PrintConfigArgs) -> Result<ExitCode> {
    if args.json {
        let value = serde_json::json!({
            "secrets_omitted": true,
            "config": config.public_view(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(ExitCode::SUCCESS);
    }

    let mut redacted = config.clone();
    redacted.server.http.token = None;
    let body = toml::to_string_pretty(&redacted).map_err(|err| {
        Error::Config(format!(
            "could not serialize the resolved configuration: {err}"
        ))
    })?;
    println!("# Resolved mcp-cura configuration.");
    println!("# Secrets are not printed. Supply them through environment variables.");
    print!("{body}");
    Ok(ExitCode::SUCCESS)
}

fn list_tools(args: &ToolsArgs) -> ExitCode {
    let registry = ToolRegistry::build();
    let names = registry.names();

    if args.json {
        let tools = names
            .iter()
            .filter_map(|name| registry.spec(name))
            .map(|spec| {
                serde_json::json!({
                    "name": spec.name,
                    "title": spec.title,
                    "description": spec.description,
                    "risk": spec.risk.as_str(),
                    "capability": spec.capability.as_str(),
                    "gate": gate_label(spec.gate),
                    "arguments": spec.allowed_args,
                })
            })
            .collect::<Vec<_>>();
        let output = serde_json::json!({"count": tools.len(), "tools": tools});
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
        );
        return ExitCode::SUCCESS;
    }

    let width = names
        .iter()
        .map(|name| name.len())
        .max()
        .unwrap_or(0)
        .max(24);
    println!("{:<width$}  {:<8}  {:<7}  TITLE", "TOOL", "RISK", "GATE");
    println!("{}", "-".repeat(width + 44));
    for name in &names {
        let Some(spec) = registry.spec(name) else {
            continue;
        };
        println!(
            "{:<width$}  {:<8}  {:<7}  {}",
            spec.name,
            spec.risk.as_str(),
            gate_label(spec.gate),
            spec.title
        );
    }
    println!();
    println!("{} tools.", names.len());
    ExitCode::SUCCESS
}

fn gate_label(gate: Gate) -> &'static str {
    match gate {
        Gate::Policy => "policy",
        Gate::Internal => "local",
        Gate::Confirm => "confirm",
    }
}

fn init_config(args: &InitArgs) -> Result<ExitCode> {
    if args.output == "-" {
        print!("{STARTER_CONFIG}");
        return Ok(ExitCode::SUCCESS);
    }

    let path = PathBuf::from(&args.output);
    if path.exists() && !args.force {
        return Err(Error::Config(format!(
            "{} already exists; use --force to overwrite it",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, STARTER_CONFIG)?;

    println!("Wrote {}", path.display());
    println!();
    println!("Next steps:");
    println!("  1. Install CuraEngine or set CURA_ENGINE_BIN / engine.docker_image.");
    println!("  2. Run: mcp-cura --config {} doctor", path.display());
    println!(
        "  3. Point your MCP client at: mcp-cura --config {}",
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn human_size(bytes: u64) -> String {
    mcp_cura::tools::human_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_cura::config::ApprovalMode;

    #[test]
    fn starter_config_parses_and_stays_safe() {
        let mut config: Config = toml::from_str(STARTER_CONFIG).unwrap();
        assert_eq!(config.server.transport, TransportKind::Stdio);
        assert_eq!(config.engine.runner, EngineRunner::Binary);
        assert_eq!(config.approval.mode, ApprovalMode::InBand);
        assert!(!config.policy.read_only);
        assert!(config.policy.allow_slicing);
        assert!(config.server.http.token.is_none());
        config.revalidate().unwrap();
    }

    #[test]
    fn starter_config_contains_no_secret_placeholders() {
        let lower = STARTER_CONFIG.to_ascii_lowercase();
        assert!(!lower.contains("changeme"));
        assert!(!lower.contains("token ="));
        assert!(!lower.contains("password"));
    }

    #[test]
    fn serve_overrides_revalidate() {
        let mut config = Config::default();
        let args = ServeArgs {
            transport: Some("http".to_string()),
            http_host: Some("0.0.0.0".to_string()),
            ..ServeArgs::default()
        };
        // Non-loopback HTTP without a token must be rejected.
        assert!(args.apply(&mut config).is_err());
    }

    #[test]
    fn tool_gates_are_labelled() {
        assert_eq!(gate_label(Gate::Policy), "policy");
        assert_eq!(gate_label(Gate::Internal), "local");
        assert_eq!(gate_label(Gate::Confirm), "confirm");
    }
}

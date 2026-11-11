# Configuration

Configuration is layered; later sources win:

1. Built-in defaults
2. TOML file (`--config FILE`, `MCP_CURA_CONFIG`, or explicitly passed during `serve`)
3. Environment variables
4. CLI flags (applied by `serve`)

Unknown TOML keys are rejected so a typo cannot silently fall back to a less safe default.
Relative state and audit paths are resolved from the process working directory; `~/` is expanded.

Generate a commented starter file with `mcp-cura init --output mcp-cura.toml`.
Print the resolved values (secrets omitted) with `mcp-cura print-config`.

## server

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `server.transport` | `MCP_CURA_TRANSPORT` | `stdio` | `stdio` or `http` |
| `server.http.host` | `MCP_CURA_HTTP_HOST` | `127.0.0.1` | Bind address |
| `server.http.port` | `MCP_CURA_HTTP_PORT` | `8765` | 1–65535 |
| `server.http.path` | `MCP_CURA_HTTP_PATH` | `/mcp` | Streamable HTTP endpoint |
| `server.http.token` | `MCP_CURA_HTTP_TOKEN` | unset | Static bearer token; required off-loopback |
| `server.http.allowed_hosts` | `MCP_CURA_HTTP_ALLOWED_HOSTS` | `[]` | Comma-separated extra `Host` values for DNS-rebinding protection |
| `server.http.allowed_origins` | `MCP_CURA_HTTP_ALLOWED_ORIGINS` | `[]` | Extra browser origins allowed by CORS |
| `server.http.allow_unauthenticated` | `MCP_CURA_HTTP_ALLOW_UNAUTHENTICATED` | `false` | Explicit opt-out of the off-loopback token requirement |

The SSE compatibility endpoints are fixed at `/sse` and `/messages`; the health endpoint is
`/health` and is unauthenticated.

## workspace

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `workspace.root` | `MCP_CURA_WORKSPACE` | OS data directory `mcp-cura` | Artifacts, jobs, custom profiles, approvals, and temporary data |
| `workspace.import_roots` | `MCP_CURA_IMPORT_ROOTS` | `~/Downloads`, `~/Documents`, `~/Desktop`, `~/models` | Filesystem roots accepted by `cura_artifact_import` |
| `workspace.export_roots` | `MCP_CURA_EXPORT_ROOTS` | `~/Downloads`, `~/Documents`, `~/Desktop` | Filesystem roots accepted by `cura_artifact_export` |
| `workspace.max_import_bytes` | `MCP_CURA_MAX_IMPORT_BYTES` | `536870912` | 1 KiB–2 GiB |
| `workspace.max_artifact_bytes` | `MCP_CURA_MAX_ARTIFACT_BYTES` | `1073741824` | 1 KiB–2 GiB per stored artifact |
| `workspace.max_artifacts` | `MCP_CURA_MAX_ARTIFACTS` | `10000` | Retained artifacts before GC |
| `workspace.keep_jobs` | `MCP_CURA_KEEP_JOBS` | `100` | Job records retained |

Artifact ids are content-derived: `art_<first 32 hex of SHA-256>`. Importing identical bytes twice
returns the same id.

## engine

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `engine.runner` | `MCP_CURA_ENGINE_RUNNER` | `binary` | `binary` or `docker` |
| `engine.binary` | `MCP_CURA_ENGINE_BINARY` | `CuraEngine` | Resolved with `PATH` when relative |
| `engine.docker_image` | `MCP_CURA_DOCKER_IMAGE` | unset | Used when `runner = "docker"` |
| `engine.docker_args` | `MCP_CURA_DOCKER_ARGS` | `[]` | Extra `docker run` arguments before the image |
| `engine.search_paths` | `MCP_CURA_ENGINE_DEFINITIONS` | `[]` | Comma-separated definition paths passed as `-d`; also reads `CURA_ENGINE_SEARCH_PATH` |
| `engine.timeout_secs` | `MCP_CURA_ENGINE_TIMEOUT_SECS` | `900` | Hard wall-clock limit for one slice |
| `engine.max_log_bytes` | `MCP_CURA_ENGINE_MAX_LOG_BYTES` | `1048576` | Per-stream engine log capture cap |
| `engine.mode` | `MCP_CURA_ENGINE_MODE` | `resolved_json` | `resolved_json` (`-r`) or `settings_args` (`-s`) |

In `resolved_json` mode the server writes a resolved settings JSON file and passes it with `-r`.
In `settings_args` mode every changed setting is passed with `-s`; per-model or per-extruder overrides
are rejected in that mode.

With the `docker` runner the job directory is mounted at `/work` and the image entrypoint is
invoked as `<image> CuraEngine`.

## profiles

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `profiles.dirs` | `MCP_CURA_PROFILE_DIRS` | `[]` | Comma-separated profile directories |
| `profiles.auto_discover` | `MCP_CURA_PROFILE_AUTO_DISCOVER` | `true` | Discover common Cura installation locations |
| `profiles.max_profiles` | `MCP_CURA_MAX_PROFILES` | `20000` | Scan limit |

The index classifies `.def.json`, `.inst.cfg`, and `.xml.fdm_material` files, tracks inheritance,
and supports quality, material, variant, intent, extruder, user/custom, and quality-change
profiles.

## policy

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `policy.read_only` | `MCP_CURA_READ_ONLY` | `false` | Denies every non-read tool |
| `policy.allow_workspace_writes` | `MCP_CURA_ALLOW_WORKSPACE_WRITES` | `true` | Imports and custom-profile creation |
| `policy.allow_slicing` | `MCP_CURA_ALLOW_SLICING` | `true` | CuraEngine execution |
| `policy.allow_export` | `MCP_CURA_ALLOW_EXPORT` | `true` | Copy artifacts outside the workspace |
| `policy.allow_delete` | `MCP_CURA_ALLOW_DELETE` | `true` | Delete stored artifacts (approval-gated) |
| `policy.max_hotend_temp` | `MCP_CURA_MAX_HOTEND_TEMP` | `320.0` | °C hard cap for profiles and G-code |
| `policy.max_bed_temp` | `MCP_CURA_MAX_BED_TEMP` | `130.0` | °C hard cap for profiles and G-code |
| `policy.max_triangles` | `MCP_CURA_MAX_TRIANGLES` | `5000000` | Mesh inspection/processing limit |
| `policy.max_voxel_cells` | `MCP_CURA_MAX_VOXEL_CELLS` | `64000000` | Voxel operation cell budget |
| `policy.max_concurrent_jobs` | `MCP_CURA_MAX_CONCURRENT_JOBS` | `2` | Parallel slice jobs |
| `policy.max_slice_secs` | `MCP_CURA_MAX_SLICE_SECS` | `900` | Slice job timeout |

`read_only = true` denies writes and processing while keeping reads available. Slice jobs are
medium risk; temperature or geometry limits are evaluated before commands are accepted.

## approval

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `approval.mode` | `MCP_CURA_APPROVAL_MODE` | `in_band` | `in_band`, `out_of_band`, or `both` |
| `approval.ttl_secs` | `MCP_CURA_APPROVAL_TTL_SECS` | `120` | 5–3600 seconds |
| `approval.state_dir` | `MCP_CURA_STATE_DIR` | OS state dir | Approval records directory |

Modes:

| Mode | Phrase returned to agent | Human `approve` required | Use case |
| --- | --- | --- | --- |
| `in_band` | yes | no | Trusted local agent |
| `out_of_band` | no | yes | Autonomous agent or shared host |
| `both` | yes | yes | Remote exposure and strongest control |

Approval records live in `<state_dir>/approvals/`, use `0700`/`0600` permissions on Unix, are
single-use, and expire at confirmation time.

## logging

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `logging.level` | `MCP_CURA_LOG_LEVEL` | `info` | `tracing` filter |
| `logging.audit_log` | `MCP_CURA_AUDIT_LOG` | unset | JSON-lines audit log; stderr when unset |

## CLI flags for `serve`

```bash
mcp-cura serve \
  --transport http \
  --http-host 0.0.0.0 \
  --http-port 8765 \
  --engine-runner docker \
  --docker-image cura-engine:5.14.0 \
  --read-only \
  --approval-mode out_of_band \
  --state-dir /var/lib/mcp-cura \
  --log-level mcp_cura=debug
```

Capability flags (`--allow-slicing`, `--allow-workspace-writes`, `--allow-export`,
`--allow-delete`) only turn capabilities on; policy file keys turn them off. Prefer the config
file for durable policy.

## Environment file

`.env.example` documents the available variables. The binary does not read `.env` files
automatically; load them in the process manager, container runtime, or shell.

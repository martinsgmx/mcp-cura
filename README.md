# mcp-cura

A safe, structured [Model Context Protocol](https://modelcontextprotocol.io) server for
[CuraEngine](https://github.com/Ultimaker/Cura) and FDM 3D printing workflows.

It wraps the Cura slicer behind well-behaved MCP tools: mesh preparation, Cura profile
discovery and resolution, background slicing, and G-code review. The server never talks to a
printer, never executes G-code, and never modifies a host Cura installation.

- **32 tools** for artifacts, mesh processing, Cura profiles, slicing jobs, and G-code review.
- **Content-addressed workspace** with stable artifact ids, content hashes, job records, and a
  GC-prunable artifact store.
- **Layered architecture** with policy, approval, and audit gates around every mutation.
- **Cura profile inheritance and expression resolution** for definitions, quality/material/variant
  instance profiles, and custom quality changes.
- **Background slice jobs** with engine timeouts, cancellation tokens, concurrency limits, and
  engine-log capture.
- **One binary, two transports.** stdio is the default for local agents; streamable HTTP/SSE is
  optional and token-gated.
- **Graceful engine degradation.** `doctor` and read-only tools work without CuraEngine installed;
  slicing tools report the missing engine clearly.

## Quick start

Requires Rust 1.89+.

```bash
git clone https://github.com/martinsgmx/mcp-cura.git
cd mcp-cura
cargo install --path .

mcp-cura doctor
mcp-cura tools
mcp-cura serve
```

`doctor` checks configuration, workspace, engine, profiles, tools, and approvals, and explains
what is missing.

### Client configuration

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "cura": {
      "command": "/home/you/.cargo/bin/mcp-cura",
      "env": {
        "MCP_CURA_ENGINE_BINARY": "/usr/bin/CuraEngine"
      }
    }
  }
}
```

OpenCode (`opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "cura": {
      "type": "local",
      "command": ["mcp-cura"],
      "enabled": true,
      "environment": {
        "MCP_CURA_ENGINE_BINARY": "/usr/bin/CuraEngine"
      }
    }
  }
}
```

Other clients: run `mcp-cura serve` with no subcommand and point the client at the stdio process.
More examples live in [`examples/`](examples/).

### HTTP transport

```bash
export MCP_CURA_HTTP_HOST=0.0.0.0
export MCP_CURA_HTTP_PORT=8765
export MCP_CURA_HTTP_TOKEN="$(openssl rand -hex 32)"
mcp-cura serve --transport http
```

The server exposes `/mcp`, `/sse`, `/messages`, and `/health`. A bearer token is required whenever
the bind address is not loopback unless `MCP_CURA_HTTP_ALLOW_UNAUTHENTICATED` is explicitly set.

## CuraEngine setup

The default engine runner is `binary`, resolved as a command name or absolute path.

```bash
mcp-cura doctor --engine-bin /usr/bin/CuraEngine
```

A local CuraEngine binary comes from a normal Cura installation, for example
`/usr/lib/cura/CuraEngine` on Debian/Ubuntu packages. The server never bundles or modifies it.

Alternatively use the `docker` runner with a pinned engine image that contains a `CuraEngine`
executable. This repo ships such an image (multi-stage, Ubuntu, builds from the pinned source tag)
at `docker/cura-engine/Dockerfile`; see [docs/cura-engine-docker.md](docs/cura-engine-docker.md):

```bash
docker build -t cura-engine:5.14.0 -f docker/cura-engine/Dockerfile docker/cura-engine

export MCP_CURA_ENGINE_DEFINITIONS=/path/to/cura-profiles
mcp-cura serve --engine-runner docker \
  --docker-image cura-engine:5.14.0
```

The server mounts each slice job directory at `/work` and runs
`<docker-image> CuraEngine`. When definitions live outside the job directory, configure them with
the engine runner (`--engine-definitions`) or copy the needed `.def.json` files into the job
directory (or a shared mount) before slicing.

To fetch a CuraEngine AppImage locally:

```bash
scripts/fetch-cura-engine.sh /tmp/CuraEngine.AppImage
scripts/fetch-cura-engine.sh
```

The script expects either a local AppImage path or `CURA_ENGINE_APPIMAGE_URL`, extracts the
binary, and copies it to the workspace-local `.cura-engine/` directory.

### Docker

The server image is a minimal Debian runtime around the Rust binary. It does not contain CuraEngine;
run CuraEngine as a separate container or host binary.

```bash
docker build -t mcp-cura:local .
docker run --rm -p 127.0.0.1:8765:8765 \
  -v mcp-cura-state:/var/lib/mcp-cura \
  -e MCP_CURA_HTTP_TOKEN=change-me \
  mcp-cura:local
```

## Configuration

Sources are applied lowest to highest: built-in defaults → TOML file → environment variables →
CLI flags. Generate a starter file:

```bash
mcp-cura init --output mcp-cura.toml
mcp-cura --config mcp-cura.toml doctor
```

Inspect the resolved, secret-free values:

```bash
mcp-cura print-config
```

See [docs/configuration.md](docs/configuration.md) for every key and environment variable, and
[examples/mcp-cura.toml](examples/mcp-cura.toml) for a fully documented sample.

## Safety model

Every tool declares a capability and a risk level. The central registry resolves capability →
risk → gate → approval, and there is no second code path.

- Capability gates: `read`, `workspace_write`, `process`, `export`, `destructive`.
- Risk levels: `read`, `low`, `medium`, `high`.
- Approval modes: `in_band` (single-use phrase), `out_of_band` (human CLI approval), `both`.
- Approvals are single-use, expiring, stored with restrictive permissions, and replayed with the
  original action and arguments only.

Exports and deletions are approval-gated. Slicing is capability-`process`, medium-risk, and subject
to policy limits (temperature caps, mesh size, voxel cell budget, concurrency, slice timeout).
G-code preparation refuses dangerous commands and adds a safe shutdown.

See [docs/safety.md](docs/safety.md) for the matrix and validation rules, and
[docs/configuration.md](docs/configuration.md) for policy keys.

## Tool families

| Prefix | Meaning |
| --- | --- |
| `cura_server_info`, `cura_engine_info`, `cura_policy_info`, `cura_pending_approvals` | Local state and safety controls |
| `cura_artifact_*` | Content-addressed workspace: import, list, get, read, export, delete |
| `cura_mesh_*` | Inspect, transform, merge, boolean, offset, remesh, decimate |
| `cura_profile_*` | Discover, load, refresh, resolve, verify, create custom profiles |
| `cura_gcode_*` | Inspect, verify, prepare G-code |
| `cura_slice_*` | Create, wait, cancel, list, and inspect slice jobs |

The two `cura_confirm_action` tools (`cura_pending_approvals` lists prepared actions) close the
approval flow. For destructive tools the prepare call returns an approval id; the agent supplies
the confirmation phrase via `cura_confirm_action`.

Detailed arguments for every tool are returned by the `tools/list` MCP handler and summarized by
`mcp-cura tools`.

## Workspace layout

```
<workspace root>/blobs/<sha256>.<ext>
<workspace root>/meta/art_<sha256-prefix>.json
<workspace root>/jobs/<job_id>/
<workspace root>/profiles/<custom profiles>/
<workspace root>/tmp/
```

Artifact ids are `art_<first 32 hex characters of the SHA-256 of the bytes>`. G-code and prepared
artifacts are stored as artifacts; job records live under `jobs/<job_id>/job.json`.

## Development

```bash
cargo test --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

A stdio smoke script is included:

```bash
python3 scripts/smoke_mcp.py target/debug/mcp-cura
```

## License

MIT

# Deployment

## Local development

```bash
cargo build
target/debug/mcp-cura serve
python3 scripts/smoke_mcp.py target/debug/mcp-cura
```

## Docker

The server image is a Debian runtime around the release binary; it does not include CuraEngine.

```bash
docker build -t mcp-cura:local .
docker run --rm -p 127.0.0.1:8765:8765 \
  -v mcp-cura-state:/var/lib/mcp-cura \
  -e MCP_CURA_HTTP_TOKEN=change-me \
  -e MCP_CURA_APPROVAL_MODE=out_of_band \
  mcp-cura:local
```

Health check: `curl -fsS http://127.0.0.1:8765/health`.

Use the example compose file at `examples/docker-compose.yml`.

## Engine as a container

For a host without CuraEngine, use `docker/cura-engine/Dockerfile` — a
multi-stage, source-built, minimal Ubuntu image (binary plus shared-library
closure only). See [`cura-engine-docker.md`](cura-engine-docker.md) for the
full recipe, profile bind-mounts for custom definitions, STL flow, and
troubleshooting.

```bash
mcp-cura serve \
  --engine-runner docker \
  --docker-image cura-engine:5.14.0
```

The server mounts the slice job directory at `/work` and invokes the image's `CuraEngine`.
Definitions must be resolvable through `engine.search_paths` or present in the job directory.

## stdio behind a local process manager

For Claude Desktop, OpenCode, or similar clients:

```json
{
  "mcpServers": {
    "cura": {
      "command": "/usr/local/bin/mcp-cura",
      "env": {
        "MCP_CURA_ENGINE_BINARY": "/usr/lib/cura/CuraEngine",
        "MCP_CURA_STATE_DIR": "/var/lib/mcp-cura"
      }
    }
  }
}
```

Prefer an absolute binary path and an explicit engine path when the client uses a minimal
environment.

## Operational checks

- `mcp-cura doctor` reports engine availability, profile counts, workspace health, and policy
  blockers.
- `mcp-cura print-config` shows the effective configuration without secrets.
- `mcp-cura pending` lists prepared approvals; run `mcp-cura approve` or `mcp-cura deny`.
- Set `MCP_CURA_LOG_LEVEL=mcp_cura=debug` for detailed tool tracing on stderr.

# Changelog

## 0.1.0

- Initial structured release of `mcp-cura`.
- 32 MCP tools covering artifacts, mesh preparation, Cura profiles, G-code review, and slicing.
- Content-addressed artifact store with stable ids and GC support.
- Cura definition/instance/material profile discovery, inheritance, resolution, and verification.
- Background CuraEngine slice jobs with cancellation and engine-log capture.
- Local policy, capability gates, double-confirmation approvals, and JSON-lines audit log.
- stdio and streamable HTTP/SSE transports with static bearer-token authentication.
- CLI: `serve`, `doctor`, `approve`, `deny`, `pending`, `print-config`, `tools`, `init`.
- Docker engine runner now mounts configured `engine.search_paths` read-only into the engine container and passes container-side `-d` flags, fixing missing profile resolution for sliced jobs (previously Docker mode silently dropped definition search paths).
- `docker/cura-engine/Dockerfile`: minimal, source-built Ubuntu (glibc) Docker image for CuraEngine (`cura-engine:5.14.0`). See `docs/cura-engine-docker.md`.

## 0.0.0

- Pre-release scaffold and library surface.

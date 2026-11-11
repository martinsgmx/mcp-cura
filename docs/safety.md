# Safety model

This server is designed to let an agent be useful without giving it an unfiltered Cura or
FDM control loop. The safety model has four independent layers:

1. **Local policy** — capability and risk decide whether a tool may run at all.
2. **Double confirmation** — high-risk actions require a prepared, single-use approval.
3. **Structured tools** — arguments are validated against hard limits before a side effect.
4. **Audit and observability** — every call is recorded with its outcome.

No layer relies on the agent behaving well.

## Capabilities and risk

Every tool declares one capability and one risk level. The single registry resolves capability →
risk → gate → approval; there is no second code path.

| Capability | Meaning | Default decision |
| --- | --- | --- |
| `read` | No side effect on CuraEngine or the filesystem | Allow |
| `workspace_write` | Write new artifacts or custom profiles | Allow, subject to policy |
| `process` | Run CPU-heavy geometry or CuraEngine processing | Allow, subject to limits |
| `export` | Copy artifacts outside the workspace | Allow, subject to policy |
| `destructive` | Delete stored artifacts | Allow, subject to approval |
| `internal` | Server-local state and policy endpoints | Allow (read-only) |

| Risk | Examples |
| --- | --- |
| `read` | Status, artifact reads, profile reads, job status |
| `low` | G-code preparation, profile creation, job cancellation, workspace writes |
| `medium` | Mesh processing, slicing |
| `high` | Artifact export, artifact delete, action confirmation |

## Default approval-gated actions

With default configuration, these tools prepare an approval instead of acting:

- `cura_artifact_export`
- `cura_artifact_delete`

The agent receives an approval id and single-use phrase from the prepare call; it then supplies the
phrase to `cura_confirm_action` with `acknowledge: true`. The confirmation handler replays the
stored tool and arguments only.

The CLI supports out-of-band decisions while the server runs:

```bash
mcp-cura pending
mcp-cura approve apr_...
mcp-cura deny apr_...
```

## Approval flow

```text
agent                         server                         human (out-of-band)
  |                              |                                  |
  |-- cura_artifact_export --->|                                  |
  |                              | policy: NeedsApproval           |
  |                              | create apr_... (store, 0600)    |
  |<-- approval_id, phrase ----|                                  |
  |                              |                                  |
  |                              |<---- mcp-cura approve apr_..    |
  |-- cura_confirm_action --->|                                  |
  |   id + phrase + ack=true     | verify, lock, mark consumed      |
  |                              | execute stored tool + args       |
  |<-- result + consumption -----|                                  |
```

Properties:

- The action is fixed at prepare time; confirmation cannot swap tool or arguments.
- Confirmation requires `acknowledge: true`; omitting it leaves the approval open.
- In `in_band` and `both` modes the phrase is compared in constant time.
- In `out_of_band` and `both` modes a human must set `human_approved_at` via the CLI.
- Records are single-use; consumed approvals cannot be retried.
- Expired, denied, and consumed records are rejected with distinct messages.
- Policy is re-evaluated at execution time; a tightened policy refuses prepared actions.
- Argument-shape validation runs at prepare time; target/existence checks run at execution time.

## Structured safety checks

- **G-code** — dangerous commands (for example `M502`) are refused by `cura_gcode_prepare`; files
  are inspected for runaway temperatures, missing heater shutdown, and missing stepper disable.
- **Profiles** — hard temperature caps, machine dimensions, extruder counts, nozzle size,
  layer height, line width, and speed checks run before resolution values reach the engine.
- **Slicing** — resolved settings that violate policy temperature limits are rejected; per-model or
  per-extruder overrides are only supported in `resolved_json` mode.
- **Artifacts** — content-addressed ids, path-traversal checks, size limits, and export-root
  validation.
- **Mesh** — triangle-count and voxel-cell budgets gate CPU-heavy operations.

## Policy switches

- `policy.read_only` denies every non-read tool, including writes and processing. It does not
  disable approvals or reads.
- `policy.allow_workspace_writes`, `allow_slicing`, `allow_export`, and `allow_delete` are
  capability master switches. Export and delete are still approval-gated when enabled.
- `policy.max_hotend_temp` and `policy.max_bed_temp` apply to profiles and G-code analysis.
- `policy.max_concurrent_jobs` and `policy.max_slice_secs` bound slice execution.

## Audit

Each call writes an event with tool name, capability, risk, approval id, outcome, duration, and
redacted arguments. Outcomes include `prepared`, `allowed`, `denied`, `approval_required`,
`confirmed`, `executed`, and `failed`.

Set `logging.audit_log` to persist JSON-lines events; by default events go to stderr under the
`mcp_cura::audit` target.

## What this model does not protect against

- A malicious agent in `in_band` mode. Prefer `out_of_band` or `both` for unattended agents.
- A compromised user account or host. Approval files are readable by that user.
- Physical failure, fire, or injury. Hardware safety is your responsibility.

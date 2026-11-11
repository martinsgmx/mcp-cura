# Tools

`mcp-cura` registers **32 tools**. The MCP `tools/list` response is authoritative and is served
by the server; this page summarizes each family and the main arguments.

Run:

```bash
mcp-cura tools
```

to print a compact risk/gate table, or read the full JSON schema from the MCP client.

## Safety quick reference

| Tool | Capability | Risk | Gate |
| --- | --- | --- | --- |
| `cura_artifact_import` | `workspace_write` | low | policy |
| `cura_artifact_list` | `read` | read | policy |
| `cura_artifact_get` | `read` | read | policy |
| `cura_artifact_read_text` | `read` | read | policy |
| `cura_artifact_export` | `export` | high | policy |
| `cura_artifact_delete` | `destructive` | high | policy |
| `cura_mesh_inspect` | `read` | read | policy |
| `cura_mesh_transform` | `process` | medium | policy |
| `cura_mesh_merge` | `process` | medium | policy |
| `cura_mesh_boolean` | `process` | medium | policy |
| `cura_mesh_offset` | `process` | medium | policy |
| `cura_mesh_remesh` | `process` | medium | policy |
| `cura_mesh_decimate` | `process` | medium | policy |
| `cura_profile_list` | `read` | read | policy |
| `cura_profile_get` | `read` | read | policy |
| `cura_profile_refresh` | `read` | read | policy |
| `cura_profile_resolve` | `read` | read | policy |
| `cura_profile_verify` | `read` | read | policy |
| `cura_profile_create` | `workspace_write` | low | policy |
| `cura_gcode_inspect` | `read` | read | policy |
| `cura_gcode_verify` | `read` | read | policy |
| `cura_gcode_prepare` | `workspace_write` | low | policy |
| `cura_slice` | `process` | medium | policy |
| `cura_slice_status` | `read` | read | policy |
| `cura_slice_wait` | `read` | read | policy |
| `cura_slice_cancel` | `workspace_write` | low | policy |
| `cura_slice_list` | `read` | read | policy |
| `cura_server_info` | `read` | read | internal |
| `cura_engine_info` | `read` | read | internal |
| `cura_policy_info` | `read` | read | internal |
| `cura_pending_approvals` | `read` | read | internal |
| `cura_confirm_action` | `read` | high | confirm |

`internal` tools reflect local server state and are not subject to the capability gate; they are
read-only. `confirm` tools are used only for prepared high-risk actions.

## Artifacts

### `cura_artifact_import`

Import a mesh, G-code, or profile into the managed workspace. Supply either `path` (absolute path
under `workspace.import_roots`) or `data` (base64 content).

| Argument | Required | Notes |
| --- | --- | --- |
| `path` | one of path/data | File under an import root |
| `data` | one of path/data | Base64 file content |
| `name` | no | Display name, for example `benchy.stl` |
| `kind` | no | `mesh`, `gcode`, `profile`, `log`, `job`, `other` |
| `format` | no | File format/extension, for example `stl` |
| `metadata` | no | Tool-specific JSON metadata |

### `cura_artifact_list`

List stored artifacts, newest first, filterable by kind and a case-insensitive substring query.

| Argument | Notes |
| --- | --- |
| `kind` | Optional artifact kind |
| `query` | Optional substring on name/id/hash |
| `limit` | 1–500, default 50 |

### `cura_artifact_get`

Return metadata for one artifact id, including kind, format, size, SHA-256, source, and metadata.

### `cura_artifact_read_text`

Read a UTF-8 text artifact with byte and line limits.

| Argument | Default | Notes |
| --- | --- | --- |
| `artifact_id` | required | `art_...` id |
| `start_line` | 1 | 1-based first line |
| `max_lines` | 400 | 1–20000 |
| `max_bytes` | 262144 | 1–5242880 |

Returns `truncated`, `lines_returned`, `total_lines`, and `next_start_line` so large files can be
paged. G-code analysis has a hard byte limit (256 MiB); larger files should be exported or split.

### `cura_artifact_export`

Copy an artifact to an absolute path under `workspace.export_roots`. Approval-gated.

| Argument | Notes |
| --- | --- | --- |
| `artifact_id` | required | Source artifact |
| `path` | required | Destination under an export root |
| `overwrite` | false | Allow replacing an existing file |

### `cura_artifact_delete`

Delete one artifact and its blob. Approval-gated and irreversible; job records referencing it are
retained.

## Mesh operations

### `cura_mesh_inspect`

Report triangle/vertex counts, bounds in millimetres, surface area, volume, watertightness,
boundary/non-manifold edges, and degenerate triangles.

### `cura_mesh_transform`

Translate, scale, rotate (XYZ Euler degrees), mirror, and/or center a mesh, producing a new mesh
artifact.

| Argument | Required | Notes |
| --- | --- | --- |
| `artifact_id` | yes | Source mesh |
| `name` | no | New artifact name |
| `translate` | no | `[x, y, z]` offset in millimetres |
| `scale` | no | `[sx, sy, sz]` scale factors |
| `rotate` | no | `[rx, ry, rz]` rotation in degrees |
| `mirror` | no | `x`, `y`, or `z`; fixes winding |
| `center_on_bed` | no | Center on XY and put lowest point at Z=0 |

Transform order: scale, rotate, translate, mirror, then centering.

### `cura_mesh_merge`

Concatenate two or more mesh artifacts into one triangle soup. This is a merge, not a boolean
union.

### `cura_mesh_boolean`

Voxel-based `union`, `subtract`, or `intersect` of two mesh artifacts.

| Argument | Required | Notes |
| --- | --- | --- |
| `left_artifact_id` | yes | Left operand |
| `right_artifact_id` | yes | Right operand |
| `operation` | yes | `union`, `subtract`, or `intersect` |
| `voxel_size` | no | 0.05–20, default 0.5 |
| `name` | no | New artifact name |

### `cura_mesh_offset`

Expand (positive) or shrink (negative) a closed mesh by a signed-distance distance.

| Argument | Required | Notes |
| --- | --- | --- |
| `artifact_id` | yes | Source mesh |
| `distance` | yes | Offset in millimetres; positive expands |
| `voxel_size` | no | 0.05–20, default 0.5 |
| `name` | no | New artifact name |

### `cura_mesh_remesh`

Rebuild a mesh surface at a new voxel resolution.

| Argument | Required | Notes |
| --- | --- | --- |
| `artifact_id` | yes | Source mesh |
| `voxel_size` | no | 0.05–20, default 0.5 |
| `name` | no | New artifact name |

### `cura_mesh_decimate`

Reduce triangle count towards `target_triangles`; the original artifact is unchanged.

| Argument | Required | Notes |
| --- | --- | --- |
| `artifact_id` | yes | Source mesh |
| `target_triangles` | yes | Desired maximum triangle count |
| `name` | no | New artifact name |

## Cura profiles

### `cura_profile_list`

List discovered definitions, quality, material, variant, intent, user/custom, and quality-change
profiles.

| Argument | Notes |
| --- | --- | --- |
| `kind` | Optional enum of profile kinds |
| `query` | Optional case-insensitive substring |
| `limit` | 1–5000, default 200 |

### `cura_profile_get`

Load one profile by id, name, or path.

| Argument | Required | Notes |
| --- | --- | --- |
| `profile_id` | yes | Profile id (`prf_...`), name, or path |
| `include_settings` | no | Include catalog or values map |
| `settings_limit` | no | 1–20000, default 500 |

### `cura_profile_refresh`

Rescan configured profile directories.

### `cura_profile_resolve`

Resolve a definition plus quality/material/variant/intent/custom layers into one settings map.

| Argument | Required | Notes |
| --- | --- | --- |
| `definition` | yes | Definition id, name, or path |
| `quality` | no | Quality profile |
| `quality_changes` | no | Array of quality-change profile names, applied in order |
| `material` | no | Material profile |
| `variant` | no | Variant profile |
| `intent` | no | Intent profile |
| `profiles` | no | Extra instance profiles applied before caller overrides |
| `overrides` | no | Caller setting overrides applied last |
| `extruders` | no | Per-extruder settings keyed by index, for example `{"0": {"speed_print": 40}}` |
| `include_settings` | yes | Include the resolved settings map |

Cura expressions that cannot be evaluated locally are reported as warnings and omitted from the
engine-facing values.

### `cura_profile_verify`

Resolve a profile and check machine dimensions, extruder count, nozzle size, temperatures, layer
height, line width, and speed.

| Argument | Required | Notes |
| --- | --- | --- |
| `definition` | yes | Definition id, name, or path |
| `quality` | no | Quality profile |
| `quality_changes` | no | Array of quality-change profiles |
| `material` | no | Material profile |
| `variant` | no | Variant profile |
| `intent` | no | Intent profile |
| `profiles` | no | Extra instance profiles |
| `overrides` | no | Caller overrides |
| `extruders` | no | Per-extruder overrides |

### `cura_profile_create`

Write a custom quality-changes `.inst.cfg` into the managed workspace and import it as a profile
artifact.

| Argument | Required | Notes |
| --- | --- | --- |
| `definition` | yes | Definition name the profile applies to |
| `name` | yes | Human-readable custom profile name |
| `values` | yes | Cura setting values, for example `{"layer_height": 0.16}` |
| `setting_version` | no | Optional Cura `setting_version` |

## G-code

### `cura_gcode_inspect`

Summarise a G-code artifact: line/command counts, layer count, Z range, estimated time, filament
usage, maximum temperatures, and dangerous commands.

### `cura_gcode_verify`

Check a G-code artifact for dangerous commands, temperatures above policy, missing heater shutdown,
missing stepper disable, and missing metadata. Returns errors/warnings and the offending issues.

### `cura_gcode_prepare`

Create a new G-code artifact with a safe heater shutdown and optionally a full end sequence.
Refuses files containing dangerous commands; the source artifact is unchanged.

| Argument | Default | Notes |
| --- | --- | --- |
| `artifact_id` | required | Source G-code artifact |
| `add_end_temperature` | true | Add `M104 S0` / `M140 S0` when missing |
| `add_end_sequence` | false | Add fan-off, Z lift, X/Y home, and `M84` when missing |
| `ensure_final_newline` | true | Ensure final newline |
| `name` | `<stem>_prepared.gcode` | New artifact name |

## Slicing jobs

### `cura_slice`

Resolve a Cura profile and start a background CuraEngine slice for one or more mesh artifacts.
Returns a job id immediately unless `wait=true`.

| Argument | Required | Notes |
| --- | --- | --- |
| `model_artifact_ids` | yes | Mesh artifact ids to slice, in order |
| `definition` | yes | Definition id, name, or path |
| `quality` | no | Quality profile |
| `quality_changes` | no | Array of quality-change profiles, in order |
| `material` | no | Material profile |
| `variant` | no | Variant profile |
| `intent` | no | Intent profile |
| `profiles` | no | Extra instance profiles |
| `overrides` | no | Global Cura setting overrides |
| `extruders` | no | Per-extruder overrides keyed by index |
| `model_overrides` | no | Per-model overrides aligned with `model_artifact_ids` |
| `name` | no | Job display name |
| `threads` | no | Engine thread count |
| `wait` | false | Block until completion or timeout |
| `wait_timeout_secs` | 900 | Maximum wait for synchronous behavior |

`settings_args` mode rejects per-model/per-extruder overrides because they must be expressed with
the resolved JSON mechanism.

### `cura_slice_status`

Return one slice job record, including state, timings, engine log tail, error, and (when complete)
the G-code artifact and statistics.

### `cura_slice_wait`

Wait until a job reaches a terminal state or the timeout elapses.

| Argument | Default | Notes |
| --- | --- | --- |
| `job_id` | required | Job id |
| `timeout_secs` | 300 | 1–3600 |

### `cura_slice_cancel`

Cancel a queued or running job; a running engine process is killed.

### `cura_slice_list`

List recent slice jobs, newest first, with state and artifact ids.

## Admin and approvals

| Tool | Purpose |
| --- | --- |
| `cura_server_info` | Server version, capabilities, and configuration summary |
| `cura_engine_info` | CuraEngine binary, probe result, mode, and configuration |
| `cura_policy_info` | Resolved policy limits and capability switches |
| `cura_pending_approvals` | List prepared approval-gated actions |
| `cura_confirm_action` | Confirm a prepared action with the single-use phrase and `acknowledge: true` |

Destructive actions (`cura_artifact_export`, `cura_artifact_delete`) prepare an approval; the
human (or the approved in-band caller) then confirms it.

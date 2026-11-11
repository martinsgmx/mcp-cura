//! Artifact import, inspection, export, and deletion tools.

use std::io::Write;

use base64::Engine as _;
use serde_json::{json, Map, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{validate_export_path, validate_import_path, validate_name, Args},
        human_bytes, json_result, no_summary, schema_object, Gate, ToolContext, ToolFuture,
        ToolSpec,
    },
    workspace::{ArtifactKind, ArtifactRecord},
};

const MAX_TEXT_ANALYSIS_BYTES: u64 = 512 * 1024 * 1024;

/// Tool specs.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "cura_artifact_import",
            title: "Import an artifact",
            description: "Copy a mesh, G-code file, or profile into the managed workspace and \
                          return a stable content-addressed artifact id. Supply either an absolute \
                          path under the configured import roots or base64 data. Importing the \
                          same bytes twice returns the same id.",
            allowed_args: &["path", "data", "name", "kind", "format", "metadata"],
            capability: Capability::WorkspaceWrite,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: summarize_import,
            schema: || {
                schema_object(
                    json!({
                        "path": {
                            "type": "string",
                            "description": "Absolute path to a file under workspace.import_roots."
                        },
                        "data": {
                            "type": "string",
                            "description": "Base64-encoded file contents (alternative to path)."
                        },
                        "name": {
                            "type": "string",
                            "description": "Display name, for example benchy.stl."
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["mesh", "gcode", "profile", "log", "job", "other"],
                            "description": "Artifact kind. Defaults to other."
                        },
                        "format": {
                            "type": "string",
                            "description": "File format/extension, for example stl or gcode. Inferred from the name when omitted."
                        },
                        "metadata": {
                            "type": "object",
                            "description": "Optional tool-specific JSON metadata."
                        }
                    }),
                    &["name"],
                )
            },
            handler: import,
        },
        ToolSpec {
            name: "cura_artifact_list",
            title: "List artifacts",
            description: "List stored artifacts, newest first, optionally filtered by kind and a \
                          case-insensitive name/id/hash substring.",
            allowed_args: &["kind", "query", "limit"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "kind": {
                            "type": "string",
                            "enum": ["mesh", "gcode", "profile", "log", "job", "other"]
                        },
                        "query": {"type": "string"},
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 500,
                            "default": 50
                        }
                    }),
                    &[],
                )
            },
            handler: list,
        },
        ToolSpec {
            name: "cura_artifact_get",
            title: "Get artifact metadata",
            description: "Return metadata for one artifact id, including kind, format, size, \
                          SHA-256, source, and any recorded metadata.",
            allowed_args: &["artifact_id"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {
                            "type": "string",
                            "description": "Content-addressed artifact id (art_...)."
                        }
                    }),
                    &["artifact_id"],
                )
            },
            handler: get,
        },
        ToolSpec {
            name: "cura_artifact_read_text",
            title: "Read artifact text",
            description: "Read a UTF-8 text artifact such as G-code or a profile with byte and \
                          line limits. Use a line range to page through large files.",
            allowed_args: &["artifact_id", "start_line", "max_lines", "max_bytes"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "start_line": {
                            "type": "integer",
                            "minimum": 1,
                            "default": 1,
                            "description": "1-based first line to return."
                        },
                        "max_lines": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 20000,
                            "default": 400
                        },
                        "max_bytes": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 5_242_880,
                            "default": 262_144
                        }
                    }),
                    &["artifact_id"],
                )
            },
            handler: read_text,
        },
        ToolSpec {
            name: "cura_artifact_export",
            title: "Export an artifact",
            description: "Copy an artifact to an absolute path under the configured export roots. \
                          This is approval-gated because it can overwrite user-visible files. The \
                          action summary names the exact destination.",
            allowed_args: &["artifact_id", "path", "overwrite"],
            capability: Capability::Export,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: summarize_export,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "path": {
                            "type": "string",
                            "description": "Absolute destination path under workspace.export_roots."
                        },
                        "overwrite": {
                            "type": "boolean",
                            "default": false,
                            "description": "Allow replacing an existing file."
                        }
                    }),
                    &["artifact_id", "path"],
                )
            },
            handler: export,
        },
        ToolSpec {
            name: "cura_artifact_delete",
            title: "Delete an artifact",
            description: "Remove one artifact and its blob from the workspace. Approval-gated and \
                          irreversible; job records that reference the artifact are kept.",
            allowed_args: &["artifact_id"],
            capability: Capability::Destructive,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: summarize_delete,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"}
                    }),
                    &["artifact_id"],
                )
            },
            handler: delete,
        },
    ]
}

fn summarize_import(args: &Value) -> String {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("artifact");
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        format!("Import '{path}' as artifact '{name}'")
    } else {
        format!("Import base64 data as artifact '{name}'")
    }
}

fn summarize_export(args: &Value) -> String {
    format!(
        "Export {} to {}",
        args.get("artifact_id")
            .and_then(Value::as_str)
            .unwrap_or("artifact"),
        args.get("path")
            .and_then(Value::as_str)
            .unwrap_or("(no path)")
    )
}

fn summarize_delete(args: &Value) -> String {
    format!(
        "Permanently delete artifact {}",
        args.get("artifact_id")
            .and_then(Value::as_str)
            .unwrap_or("(none)")
    )
}

fn import(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &["path", "data", "name", "kind", "format", "metadata"],
        )?;
        let name = validate_name(&args.required_string("name")?, "name")?;
        let kind = args
            .optional_string("kind")?
            .map_or(Ok(ArtifactKind::Other), |kind| {
                kind.parse::<ArtifactKind>()
                    .map_err(Error::InvalidArguments)
            })?;
        let format = args.optional_string("format")?.unwrap_or_else(|| {
            std::path::Path::new(&name)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("bin")
                .to_string()
        });
        let metadata = args
            .optional_object("metadata")?
            .map_or(Value::Null, Value::Object);
        let path = args.optional_string("path")?;
        let data = args.optional_string("data")?;

        let record = match (path, data) {
            (Some(path), None) => {
                let path = validate_import_path(&path, &ctx.config.workspace.import_roots)?;
                let source = path.to_string_lossy().to_string();
                ctx.store
                    .import_file(&name, kind, &format, &path, Some(source), metadata)?
            }
            (None, Some(data)) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .map_err(|err| {
                        Error::InvalidArguments(format!("'data' is not valid base64: {err}"))
                    })?;
                ctx.store.import_bytes(
                    &name,
                    kind,
                    &format,
                    &bytes,
                    Some("tool:base64".into()),
                    metadata,
                )?
            }
            (Some(_), Some(_)) => {
                return Err(Error::InvalidArguments(
                    "supply either 'path' or 'data', not both".to_string(),
                ));
            }
            (None, None) => {
                return Err(Error::InvalidArguments(
                    "one of 'path' or 'data' is required".to_string(),
                ));
            }
        };
        Ok(json_result(&record_json(ctx, &record), None))
    })
}

fn list(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["kind", "query", "limit"])?;
        let kind = args.optional_string("kind")?.map_or(Ok(None), |kind| {
            kind.parse::<ArtifactKind>()
                .map(Some)
                .map_err(Error::InvalidArguments)
        })?;
        let query = args.optional_string("query")?;
        let limit = args
            .optional_u64("limit")?
            .map_or(50, |limit| limit.min(500) as usize);
        let records = ctx.store.list(kind, query.as_deref(), limit)?;
        let artifacts = records
            .iter()
            .map(|record| record_json(ctx, record))
            .collect::<Vec<_>>();
        Ok(json_result(
            &json!({
                "count": artifacts.len(),
                "root": ctx.store.root(),
                "artifacts": artifacts,
            }),
            None,
        ))
    })
}

fn get(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id"])?;
        let id = args.required_string("artifact_id")?;
        let record = ctx.store.get(&id)?;
        Ok(json_result(&record_json(ctx, &record), None))
    })
}

fn read_text(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &["artifact_id", "start_line", "max_lines", "max_bytes"],
        )?;
        let id = args.required_string("artifact_id")?;
        let start_line = args.optional_u64("start_line")?.unwrap_or(1).max(1) as usize;
        let max_lines = args
            .optional_u64("max_lines")?
            .map_or(400, |value| value.min(20_000) as usize);
        let max_bytes = args
            .optional_u64("max_bytes")?
            .map_or(262_144, |value| value.min(5_242_880) as usize);
        let record = ctx.store.get(&id)?;
        if record.bytes > MAX_TEXT_ANALYSIS_BYTES {
            return Err(Error::Artifact(format!(
                "artifact '{id}' is too large to analyse as text ({} bytes); export it instead",
                record.bytes
            )));
        }
        let text = ctx.store.read_text(&id, record.bytes as usize)?;
        let total_lines = text.lines().count();
        let selected_lines = text
            .lines()
            .skip(start_line - 1)
            .take(max_lines)
            .collect::<Vec<_>>();
        let line_truncated = start_line - 1 + selected_lines.len() < total_lines;
        let selected = selected_lines.join("\n");
        let byte_truncated = selected.len() > max_bytes;
        let selected = if byte_truncated {
            let mut end = max_bytes;
            while !selected.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            selected[..end].to_string()
        } else {
            selected
        };
        let lines_returned = selected.lines().count();
        Ok(json_result(
            &json!({
                "artifact_id": id,
                "start_line": start_line,
                "lines_returned": lines_returned,
                "total_lines": total_lines,
                "truncated": line_truncated || byte_truncated,
                "next_start_line": (start_line + lines_returned).min(total_lines + 1),
                "text": selected,
            }),
            None,
        ))
    })
}

fn export(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id", "path", "overwrite"])?;
        let id = args.required_string("artifact_id")?;
        let path = validate_export_path(
            &args.required_string("path")?,
            &ctx.config.workspace.export_roots,
        )?;
        let overwrite = args.optional_bool("overwrite")?.unwrap_or(false);
        let overwritten = path.exists();
        if overwritten && !overwrite {
            return Err(Error::InvalidArguments(format!(
                "{} already exists; set overwrite=true to replace it",
                path.display()
            )));
        }
        let record = ctx.store.get(&id)?;
        let bytes = ctx.store.read_bytes(&id)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true);
        if overwrite {
            options.truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = options.open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(json_result(
            &json!({
                "artifact_id": id,
                "path": path.to_string_lossy(),
                "bytes": bytes.len(),
                "sha256": record.sha256,
                "overwritten": overwritten,
            }),
            None,
        ))
    })
}

fn delete(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id"])?;
        let id = args.required_string("artifact_id")?;
        let record = ctx.store.delete(&id)?;
        Ok(json_result(
            &json!({
                "deleted": record.id,
                "name": record.name,
                "bytes": record.bytes,
            }),
            None,
        ))
    })
}

/// Shared artifact representation used by other tool modules.
#[must_use]
pub fn record_json(ctx: &ToolContext, record: &ArtifactRecord) -> Value {
    json!({
        "artifact_id": record.id,
        "name": record.name,
        "kind": record.kind.as_str(),
        "format": record.format,
        "bytes": record.bytes,
        "bytes_human": human_bytes(record.bytes),
        "sha256": record.sha256,
        "created_at": record.created_at,
        "source": record.source,
        "metadata": record.metadata,
        "blob_path": ctx.store.blob_path(record),
    })
}

/// Load an artifact as a mesh.
pub fn load_mesh(ctx: &ToolContext, id: &str) -> Result<crate::geometry::Mesh> {
    let record = ctx.store.get(id)?;
    if record.kind != ArtifactKind::Mesh {
        return Err(Error::InvalidArguments(format!(
            "artifact '{id}' is a {} artifact, not a mesh",
            record.kind.as_str()
        )));
    }
    let bytes = ctx.store.read_bytes(id)?;
    match record.format.as_str() {
        "stl" => crate::geometry::Mesh::from_stl_bytes(&bytes),
        "obj" | "ply" => crate::geometry::Mesh::load_path(&ctx.store.blob_path(&record)),
        other => Err(Error::InvalidArguments(format!(
            "unsupported mesh format '{other}' (expected stl, obj or ply)"
        ))),
    }
}

/// Store a mesh generated in memory, returning the artifact record.
pub fn store_mesh(
    ctx: &ToolContext,
    name: &str,
    mesh: &crate::geometry::Mesh,
    metadata: Map<String, Value>,
    source: &str,
) -> Result<ArtifactRecord> {
    let temp = ctx
        .store
        .tmp_dir()
        .join(format!(".mesh.{}.stl", uuid::Uuid::new_v4().simple()));
    mesh.write_path(&temp)?;
    let result = ctx.store.import_file(
        name,
        ArtifactKind::Mesh,
        "stl",
        &temp,
        Some(source.to_string()),
        Value::Object(metadata),
    );
    let _ = std::fs::remove_file(temp);
    result
}

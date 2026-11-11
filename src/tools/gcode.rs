//! G-code inspection, verification, and preparation tools.

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    gcode::{self, PrepareOptions},
    safety::{Capability, Risk},
    tools::{
        args::Args, artifacts::record_json, json_result, no_summary, schema_object, Gate,
        ToolContext, ToolFuture, ToolSpec,
    },
    workspace::ArtifactKind,
};

const MAX_GCODE_BYTES: u64 = 256 * 1024 * 1024;

/// Tool specs.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "cura_gcode_inspect",
            title: "Inspect G-code",
            description: "Summarise a G-code artifact: line/command counts, layer count, Z range, \
                          estimated time, filament usage, maximum temperatures, and dangerous \
                          commands. This is a read-only text analysis.",
            allowed_args: &["artifact_id"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || schema_object(json!({"artifact_id": {"type": "string"}}), &["artifact_id"]),
            handler: inspect,
        },
        ToolSpec {
            name: "cura_gcode_verify",
            title: "Verify G-code",
            description: "Check a G-code artifact for dangerous commands, temperatures above the \
                          local safety policy, missing heater shutdown, missing stepper disable, \
                          and missing metadata.",
            allowed_args: &["artifact_id"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || schema_object(json!({"artifact_id": {"type": "string"}}), &["artifact_id"]),
            handler: verify,
        },
        ToolSpec {
            name: "cura_gcode_prepare",
            title: "Prepare G-code",
            description:
                "Create a new G-code artifact with a safe heater shutdown and optionally a \
                          full end sequence (fan off, lift, home X/Y, steppers off). Refuses files \
                          that contain dangerous commands. The original artifact is unchanged.",
            allowed_args: &[
                "artifact_id",
                "add_end_temperature",
                "add_end_sequence",
                "ensure_final_newline",
                "name",
            ],
            capability: Capability::WorkspaceWrite,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "add_end_temperature": {
                            "type": "boolean",
                            "default": true,
                            "description": "Add M104 S0/M140 S0 when no heater shutdown is present."
                        },
                        "add_end_sequence": {
                            "type": "boolean",
                            "default": false,
                            "description": "Add fan-off, Z lift, X/Y home, and M84 when missing."
                        },
                        "ensure_final_newline": {
                            "type": "boolean",
                            "default": true
                        },
                        "name": {
                            "type": "string",
                            "description": "Name for the new artifact; defaults to <stem>_prepared.gcode."
                        }
                    }),
                    &["artifact_id"],
                )
            },
            handler: prepare,
        },
    ]
}

fn inspect(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id"])?;
        let id = args.required_string("artifact_id")?;
        let (record, text) = read_gcode(ctx, &id)?;
        let stats = gcode::analyze(&text);
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": stats,
            }),
            None,
        ))
    })
}

fn verify(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id"])?;
        let id = args.required_string("artifact_id")?;
        let (record, text) = read_gcode(ctx, &id)?;
        let report = gcode::verify(&text, &ctx.config.policy);
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "policy": {
                    "max_hotend_temp": ctx.config.policy.max_hotend_temp,
                    "max_bed_temp": ctx.config.policy.max_bed_temp,
                },
                "report": report,
            }),
            None,
        ))
    })
}

fn prepare(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &[
                "artifact_id",
                "add_end_temperature",
                "add_end_sequence",
                "ensure_final_newline",
                "name",
            ],
        )?;
        let id = args.required_string("artifact_id")?;
        let (record, text) = read_gcode(ctx, &id)?;
        let options = PrepareOptions {
            add_end_temperature: args.optional_bool("add_end_temperature")?.unwrap_or(true),
            add_end_sequence: args.optional_bool("add_end_sequence")?.unwrap_or(false),
            ensure_final_newline: args.optional_bool("ensure_final_newline")?.unwrap_or(true),
        };
        let prepared = gcode::prepare(&text, options)?;
        let name = args.optional_string("name")?.unwrap_or_else(|| {
            let stem = std::path::Path::new(&record.name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("output");
            format!("{stem}_prepared.gcode")
        });
        let artifact = ctx.store.import_bytes(
            &name,
            ArtifactKind::Gcode,
            "gcode",
            prepared.text.as_bytes(),
            Some(format!("tool:cura_gcode_prepare:{id}")),
            json!({
                "source_artifact": id,
                "changes": prepared.changes,
            }),
        )?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &artifact),
                "source_artifact": id,
                "changes": prepared.changes,
                "stats_before": prepared.stats_before,
                "stats_after": prepared.stats_after,
            }),
            None,
        ))
    })
}

fn read_gcode(ctx: &ToolContext, id: &str) -> Result<(crate::workspace::ArtifactRecord, String)> {
    let record = ctx.store.get(id)?;
    if record.kind != ArtifactKind::Gcode {
        return Err(Error::InvalidArguments(format!(
            "artifact '{id}' is a {} artifact, not G-code",
            record.kind.as_str()
        )));
    }
    if record.bytes > MAX_GCODE_BYTES {
        return Err(Error::InvalidArguments(format!(
            "G-code artifact is {} bytes, above the {} byte analysis limit",
            record.bytes, MAX_GCODE_BYTES
        )));
    }
    let bytes = ctx.store.read_bytes(id)?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    Ok((record, text))
}

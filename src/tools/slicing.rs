//! Slice job tools.

use std::{path::PathBuf, time::Duration};

use serde_json::{json, Map, Value};

use crate::{
    engine::{PlanModel, SlicePlan},
    error::{Error, Result},
    safety::{Capability, Risk},
    slicing::{JobRecord, SliceJobRequest},
    tools::{
        args::Args, artifacts::record_json, json_result, no_summary, schema_object, Gate,
        ToolContext, ToolFuture, ToolSpec,
    },
    workspace::ArtifactKind,
};

/// Tool specs.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "cura_slice",
            title: "Slice models",
            description:
                "Resolve a Cura profile and start a background CuraEngine slice for one or \
                          more mesh artifacts. Returns a job id immediately unless wait=true. The \
                          G-code is imported as an artifact when the job completes.",
            allowed_args: &[
                "model_artifact_ids",
                "definition",
                "quality",
                "quality_changes",
                "material",
                "variant",
                "intent",
                "profiles",
                "overrides",
                "extruders",
                "model_overrides",
                "name",
                "threads",
                "wait",
                "wait_timeout_secs",
            ],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "model_artifact_ids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "minItems": 1,
                            "description": "Mesh artifact ids to slice, in order."
                        },
                        "definition": {
                            "type": "string",
                            "description": "Machine definition id, name, or path."
                        },
                        "quality": {"type": "string"},
                        "quality_changes": {
                            "type": "array",
                            "items": {"type": "string"}
                        },
                        "material": {"type": "string"},
                        "variant": {"type": "string"},
                        "intent": {"type": "string"},
                        "profiles": {
                            "type": "array",
                            "items": {"type": "string"}
                        },
                        "overrides": {
                            "type": "object",
                            "description": "Global Cura setting overrides."
                        },
                        "extruders": {
                            "type": "object",
                            "description": "Per-extruder settings keyed by index."
                        },
                        "model_overrides": {
                            "type": "array",
                            "items": {"type": "object"},
                            "description": "Per-model settings aligned with model_artifact_ids."
                        },
                        "name": {"type": "string"},
                        "threads": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 256
                        },
                        "wait": {
                            "type": "boolean",
                            "default": false,
                            "description": "Block until the job finishes (or the wait timeout)."
                        },
                        "wait_timeout_secs": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 86400,
                            "default": 900
                        }
                    }),
                    &["model_artifact_ids", "definition"],
                )
            },
            handler: slice,
        },
        ToolSpec {
            name: "cura_slice_status",
            title: "Slice job status",
            description: "Return one slice job record, including state, timings, engine log tail, \
                          error, and (when complete) the G-code artifact and statistics.",
            allowed_args: &["job_id"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || schema_object(json!({"job_id": {"type": "string"}}), &["job_id"]),
            handler: status,
        },
        ToolSpec {
            name: "cura_slice_wait",
            title: "Wait for a slice job",
            description: "Wait until a job reaches a terminal state or the timeout elapses, then \
                          return the record. Prefer polling cura_slice_status for long jobs.",
            allowed_args: &["job_id", "timeout_secs"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "job_id": {"type": "string"},
                        "timeout_secs": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 3600,
                            "default": 300
                        }
                    }),
                    &["job_id"],
                )
            },
            handler: wait,
        },
        ToolSpec {
            name: "cura_slice_cancel",
            title: "Cancel a slice job",
            description:
                "Cancel a queued or running slice job. A running engine process is killed \
                          and the job is marked cancelled. Completed jobs cannot be cancelled.",
            allowed_args: &["job_id"],
            capability: Capability::WorkspaceWrite,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || schema_object(json!({"job_id": {"type": "string"}}), &["job_id"]),
            handler: cancel,
        },
        ToolSpec {
            name: "cura_slice_list",
            title: "List slice jobs",
            description: "List recent slice jobs, newest first, with state and artifact ids.",
            allowed_args: &["limit"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 200,
                            "default": 20
                        }
                    }),
                    &[],
                )
            },
            handler: list,
        },
    ]
}

fn slice(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &[
                "model_artifact_ids",
                "definition",
                "quality",
                "quality_changes",
                "material",
                "variant",
                "intent",
                "profiles",
                "overrides",
                "extruders",
                "model_overrides",
                "name",
                "threads",
                "wait",
                "wait_timeout_secs",
            ],
        )?;
        let model_ids = args.required_string_list("model_artifact_ids")?;
        let model_overrides = parse_model_overrides(&args, model_ids.len())?;
        let resolved = crate::tools::profiles::resolve_profile(ctx, &args)?;

        let violations = crate::profiles::temperature_violations(
            &resolved.resolution.values,
            &ctx.config.policy,
        );
        if !violations.is_empty() {
            return Err(Error::Policy(format!(
                "resolved profile exceeds the safety temperature limits: {}",
                violations.join("; ")
            )));
        }

        let timeout_secs = ctx
            .config
            .engine
            .timeout_secs
            .min(ctx.config.policy.max_slice_secs)
            .max(1);
        let mut plan = SlicePlan::new(ctx.config.engine.mode, Duration::from_secs(timeout_secs));
        plan.definition = Some(PathBuf::from(&resolved.definition.path));
        plan.global = resolved.resolution.engine_values();
        plan.extruders = resolved.extruders.clone();
        plan.threads = args
            .optional_u32("threads")?
            .map(|threads| threads as usize)
            .or(ctx.config.engine.threads);
        plan.verbose = ctx.config.engine.verbose;
        plan.search_paths = ctx.config.engine_search_paths();

        for (index, id) in model_ids.iter().enumerate() {
            let record = ctx.store.get(id)?;
            if record.kind != ArtifactKind::Mesh {
                return Err(Error::InvalidArguments(format!(
                    "'{id}' is a {} artifact, not a mesh",
                    record.kind.as_str()
                )));
            }
            if !matches!(record.format.as_str(), "stl" | "obj" | "ply") {
                return Err(Error::InvalidArguments(format!(
                    "CuraEngine input format '{}' is not supported here; use STL, OBJ, or PLY",
                    record.format
                )));
            }
            plan.models.push(PlanModel {
                file_name: format!("model_{index}.{}", record.format),
                source: ctx.store.blob_path(&record),
                settings: model_overrides.get(index).cloned().unwrap_or_default(),
                artifact_id: Some(record.id.clone()),
            });
        }

        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| format!("slice {}", resolved.definition.name));
        let request = SliceJobRequest {
            name: name.clone(),
            plan,
            model_artifacts: model_ids.clone(),
            definition: Some(resolved.definition.name.clone()),
            profile_summary: json!({
                "definition": resolved.definition.name,
                "layers": resolved.resolution.layers,
                "settings": resolved.resolution.values.len(),
                "explicit": resolved.resolution.explicit.len(),
                "warnings": resolved.resolution.warnings.len(),
                "extruders": resolved.extruders.keys().collect::<Vec<_>>(),
            }),
            request: args.as_value(),
        };

        let record = ctx.jobs.start(request)?;
        let wait = args.optional_bool("wait")?.unwrap_or(false);
        if wait {
            let timeout = args
                .optional_u64("wait_timeout_secs")?
                .map_or(900, |seconds| seconds.min(86_400));
            let finished = ctx
                .jobs
                .wait(&record.id, Duration::from_secs(timeout))
                .await?;
            return Ok(json_result(&job_json(ctx, &finished), None));
        }

        Ok(json_result(
            &json!({
                "job": job_json(ctx, &record),
                "message": "Slice job queued. Poll cura_slice_status or call cura_slice_wait.",
                "poll_after_ms": 500,
            }),
            None,
        ))
    })
}

fn status(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["job_id"])?;
        let id = args.required_string("job_id")?;
        let record = ctx.jobs.status(&id)?;
        Ok(json_result(&job_json(ctx, &record), None))
    })
}

fn wait(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["job_id", "timeout_secs"])?;
        let id = args.required_string("job_id")?;
        let timeout = args
            .optional_u64("timeout_secs")?
            .map_or(300, |seconds| seconds.clamp(1, 3600));
        let record = ctx.jobs.wait(&id, Duration::from_secs(timeout)).await?;
        Ok(json_result(&job_json(ctx, &record), None))
    })
}

fn cancel(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["job_id"])?;
        let id = args.required_string("job_id")?;
        let record = ctx.jobs.cancel(&id)?;
        Ok(json_result(&job_json(ctx, &record), None))
    })
}

fn list(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["limit"])?;
        let limit = args
            .optional_u64("limit")?
            .map_or(20, |limit| limit.min(200) as usize);
        let records = ctx.jobs.list(limit)?;
        let jobs = records
            .iter()
            .map(|record| {
                json!({
                    "job_id": record.id,
                    "name": record.name,
                    "state": record.state.as_str(),
                    "created_at": record.created_at,
                    "duration_ms": record.duration_ms,
                    "gcode_artifact": record.gcode_artifact,
                    "error": record.error,
                })
            })
            .collect::<Vec<_>>();
        Ok(json_result(
            &json!({"count": jobs.len(), "jobs": jobs}),
            None,
        ))
    })
}

fn job_json(ctx: &ToolContext, record: &JobRecord) -> Value {
    let gcode = record.gcode_artifact.as_ref().and_then(|id| {
        ctx.store
            .get(id)
            .ok()
            .map(|artifact| record_json(ctx, &artifact))
    });
    json!({
        "job_id": record.id,
        "name": record.name,
        "state": record.state.as_str(),
        "terminal": record.state.is_terminal(),
        "created_at": record.created_at,
        "started_at": record.started_at,
        "finished_at": record.finished_at,
        "duration_ms": record.duration_ms,
        "model_artifacts": record.model_artifacts,
        "definition": record.definition,
        "profile_summary": record.profile_summary,
        "gcode_artifact_id": record.gcode_artifact,
        "gcode_artifact": gcode,
        "gcode_bytes": record.gcode_bytes,
        "gcode_stats": record.gcode_stats,
        "logs": record.logs,
        "error": record.error,
    })
}

fn parse_model_overrides(args: &Args, models: usize) -> Result<Vec<Map<String, Value>>> {
    let Some(value) = args.value("model_overrides") else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| Error::InvalidArguments("'model_overrides' must be an array".to_string()))?;
    if items.len() != models {
        return Err(Error::InvalidArguments(format!(
            "'model_overrides' has {} entries but there are {models} models",
            items.len()
        )));
    }
    items
        .iter()
        .map(|item| {
            item.as_object().cloned().ok_or_else(|| {
                Error::InvalidArguments(
                    "'model_overrides' must contain only JSON objects".to_string(),
                )
            })
        })
        .collect()
}

//! Mesh inspection and transformation tools.

use serde_json::{json, Map, Value};

use crate::{
    error::{Error, Result},
    geometry::Mesh,
    safety::{Capability, Risk},
    tools::{
        args::{validate_name, Args},
        artifacts::{load_mesh, record_json, store_mesh},
        json_result, no_summary, schema_object, Gate, ToolContext, ToolFuture, ToolSpec,
    },
};

/// Tool specs.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "cura_mesh_inspect",
            title: "Inspect a mesh",
            description: "Report triangle and vertex counts, bounds in millimetres, surface area, \
                          volume, watertightness, boundary/non-manifold edges, and degenerate \
                          triangles for a mesh artifact.",
            allowed_args: &["artifact_id"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || schema_object(json!({"artifact_id": {"type": "string"}}), &["artifact_id"]),
            handler: inspect,
        },
        ToolSpec {
            name: "cura_mesh_transform",
            title: "Transform a mesh",
            description: "Translate, scale, rotate (XYZ Euler degrees), mirror, and/or center a \
                          mesh on the bed, producing a new mesh artifact. Transform order is \
                          scale, rotate, translate, mirror, then centering.",
            allowed_args: &[
                "artifact_id",
                "name",
                "translate",
                "scale",
                "rotate",
                "mirror",
                "center_on_bed",
            ],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "name": {"type": "string", "description": "Name for the new artifact."},
                        "translate": {
                            "type": "array",
                            "items": {"type": "number"},
                            "minItems": 3,
                            "maxItems": 3,
                            "description": "[x, y, z] offset in millimetres."
                        },
                        "scale": {
                            "type": "array",
                            "items": {"type": "number"},
                            "minItems": 3,
                            "maxItems": 3,
                            "description": "[x, y, z] scale factors."
                        },
                        "rotate": {
                            "type": "array",
                            "items": {"type": "number"},
                            "minItems": 3,
                            "maxItems": 3,
                            "description": "[x, y, z] rotation in degrees."
                        },
                        "mirror": {
                            "type": "string",
                            "enum": ["x", "y", "z"],
                            "description": "Mirror across one axis and fix winding."
                        },
                        "center_on_bed": {
                            "type": "boolean",
                            "default": false,
                            "description": "Center on XY and put the lowest point at Z=0."
                        }
                    }),
                    &["artifact_id"],
                )
            },
            handler: transform,
        },
        ToolSpec {
            name: "cura_mesh_merge",
            title: "Merge meshes",
            description: "Concatenate two or more mesh artifacts into one triangle soup. This is \
                          not a boolean union; overlapping shells remain. Use cura_mesh_boolean \
                          for a true union.",
            allowed_args: &["artifact_ids", "name"],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_ids": {
                            "type": "array",
                            "items": {"type": "string"},
                            "minItems": 2,
                            "description": "Mesh artifact ids in merge order."
                        },
                        "name": {"type": "string"}
                    }),
                    &["artifact_ids"],
                )
            },
            handler: merge,
        },
        ToolSpec {
            name: "cura_mesh_boolean",
            title: "Boolean mesh operation",
            description: "Voxel-based union, subtract, or intersect of two mesh artifacts. Voxel \
                          resolution trades detail for memory; the policy caps estimated cells.",
            allowed_args: &[
                "left_artifact_id",
                "right_artifact_id",
                "operation",
                "voxel_size",
                "name",
            ],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "left_artifact_id": {"type": "string"},
                        "right_artifact_id": {"type": "string"},
                        "operation": {
                            "type": "string",
                            "enum": ["union", "subtract", "intersect"]
                        },
                        "voxel_size": {
                            "type": "number",
                            "minimum": 0.05,
                            "maximum": 20,
                            "default": 0.5,
                            "description": "Voxel edge length in millimetres."
                        },
                        "name": {"type": "string"}
                    }),
                    &["left_artifact_id", "right_artifact_id", "operation"],
                )
            },
            handler: boolean,
        },
        ToolSpec {
            name: "cura_mesh_offset",
            title: "Offset a mesh",
            description: "Expand (positive) or shrink (negative) a closed mesh by a distance in \
                          millimetres using a signed-distance volume.",
            allowed_args: &["artifact_id", "distance", "voxel_size", "name"],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "distance": {
                            "type": "number",
                            "minimum": -100,
                            "maximum": 100,
                            "description": "Offset in millimetres; positive expands."
                        },
                        "voxel_size": {
                            "type": "number",
                            "minimum": 0.05,
                            "maximum": 20,
                            "default": 0.5
                        },
                        "name": {"type": "string"}
                    }),
                    &["artifact_id", "distance"],
                )
            },
            handler: offset,
        },
        ToolSpec {
            name: "cura_mesh_remesh",
            title: "Remesh a mesh",
            description: "Rebuild a mesh surface at a new voxel resolution. Useful to clean up \
                          self-intersections before slicing, at the cost of detail.",
            allowed_args: &["artifact_id", "voxel_size", "name"],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "voxel_size": {
                            "type": "number",
                            "minimum": 0.05,
                            "maximum": 20,
                            "default": 0.5
                        },
                        "name": {"type": "string"}
                    }),
                    &["artifact_id"],
                )
            },
            handler: remesh,
        },
        ToolSpec {
            name: "cura_mesh_decimate",
            title: "Decimate a mesh",
            description: "Reduce triangle count towards a target while keeping the overall shape. \
                          The result is a new mesh artifact; the original is unchanged.",
            allowed_args: &["artifact_id", "target_triangles", "name"],
            capability: Capability::Process,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "artifact_id": {"type": "string"},
                        "target_triangles": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Desired maximum triangle count."
                        },
                        "name": {"type": "string"}
                    }),
                    &["artifact_id", "target_triangles"],
                )
            },
            handler: decimate,
        },
    ]
}

fn inspect(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id"])?;
        let id = args.required_string("artifact_id")?;
        let record = ctx.store.get(&id)?;
        let mesh = load_mesh(ctx, &id)?;
        let stats = mesh.stats();
        let over_limit = stats.triangles > ctx.config.policy.max_triangles;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": stats,
                "triangle_limit": ctx.config.policy.max_triangles,
                "over_triangle_limit": over_limit,
                "notes": if stats.watertight {
                    json!([])
                } else {
                    json!([
                        "The mesh is not watertight. CuraEngine can still slice it, but booleans and offsets may fail or produce unexpected results."
                    ])
                }
            }),
            None,
        ))
    })
}

fn transform(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &[
                "artifact_id",
                "name",
                "translate",
                "scale",
                "rotate",
                "mirror",
                "center_on_bed",
            ],
        )?;
        let id = args.required_string("artifact_id")?;
        let translate = optional_vec3(&args, "translate")?;
        let scale = optional_vec3(&args, "scale")?;
        let rotate = optional_vec3(&args, "rotate")?;
        let mirror = args.optional_string("mirror")?;
        let center = args.optional_bool("center_on_bed")?.unwrap_or(false);

        if translate.is_none() && scale.is_none() && rotate.is_none() && mirror.is_none() && !center
        {
            return Err(Error::InvalidArguments(
                "supply at least one of translate, scale, rotate, mirror, or center_on_bed"
                    .to_string(),
            ));
        }
        if let Some(scale) = scale {
            if scale.iter().any(|value| value.abs() < 1e-9) {
                return Err(Error::InvalidArguments(
                    "scale factors must not be zero".to_string(),
                ));
            }
        }

        let source = ctx.store.get(&id)?;
        let mut mesh = load_mesh(ctx, &id)?;
        let _permit = ctx
            .process_slots
            .acquire()
            .await
            .map_err(|_| Error::Process("mesh processing queue was closed".to_string()))?;
        if let Some(scale) = scale {
            mesh.scale(scale);
        }
        if let Some(rotate) = rotate {
            mesh.rotate_degrees(rotate);
        }
        if let Some(translate) = translate {
            mesh.translate(translate);
        }
        if let Some(ref axis) = mirror {
            let axis = match axis.to_ascii_lowercase().as_str() {
                "x" | "0" => 0,
                "y" | "1" => 1,
                "z" | "2" => 2,
                other => {
                    return Err(Error::InvalidArguments(format!(
                        "mirror must be x, y or z, got '{other}'"
                    )));
                }
            };
            mesh.mirror(axis);
        }
        if center {
            mesh.center_on_bed(true);
        }

        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| derived_name(&source.name, "transformed"));
        let mut metadata = Map::new();
        metadata.insert("operation".into(), json!("transform"));
        metadata.insert("source_artifact".into(), json!(id));
        metadata.insert(
            "transform".into(),
            json!({
                "translate": translate,
                "scale": scale,
                "rotate": rotate,
                "mirror": mirror,
                "center_on_bed": center,
            }),
        );
        let record = store_mesh(ctx, &name, &mesh, metadata, "tool:cura_mesh_transform")?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": mesh.stats(),
            }),
            None,
        ))
    })
}

fn merge(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_ids", "name"])?;
        let ids = args.required_string_list("artifact_ids")?;
        if ids.len() < 2 {
            return Err(Error::InvalidArguments(
                "at least two artifact ids are required".to_string(),
            ));
        }
        let _permit = ctx
            .process_slots
            .acquire()
            .await
            .map_err(|_| Error::Process("mesh processing queue was closed".to_string()))?;
        let mut merged: Option<Mesh> = None;
        let mut sources = Vec::new();
        for id in &ids {
            let mesh = load_mesh(ctx, id)?;
            check_triangles(ctx, mesh.triangle_count())?;
            if let Some(merged) = &mut merged {
                merged.merge(&mesh);
            } else {
                merged = Some(mesh);
            }
            sources.push(id.clone());
        }
        let mesh = merged.expect("ids is non-empty");
        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| "merged.stl".to_string());
        let name = validate_name(&name, "name")?;
        let mut metadata = Map::new();
        metadata.insert("operation".into(), json!("merge"));
        metadata.insert("sources".into(), json!(sources));
        let record = store_mesh(ctx, &name, &mesh, metadata, "tool:cura_mesh_merge")?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": mesh.stats(),
            }),
            None,
        ))
    })
}

fn boolean(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &[
                "left_artifact_id",
                "right_artifact_id",
                "operation",
                "voxel_size",
                "name",
            ],
        )?;
        let left_id = args.required_string("left_artifact_id")?;
        let right_id = args.required_string("right_artifact_id")?;
        let operation = args.required_string("operation")?;
        let voxel_size = args.optional_f64("voxel_size")?.unwrap_or(0.5);
        let left = load_mesh(ctx, &left_id)?;
        let right = load_mesh(ctx, &right_id)?;
        check_voxels(ctx, &left, voxel_size)?;
        check_voxels(ctx, &right, voxel_size)?;
        let _permit = ctx
            .process_slots
            .acquire()
            .await
            .map_err(|_| Error::Process("mesh processing queue was closed".to_string()))?;
        let result = left.boolean(&right, &operation, voxel_size)?;
        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| format!("{operation}.stl"));
        let name = validate_name(&name, "name")?;
        let mut metadata = Map::new();
        metadata.insert("operation".into(), json!("boolean"));
        metadata.insert("boolean".into(), json!(operation));
        metadata.insert("left_artifact".into(), json!(left_id));
        metadata.insert("right_artifact".into(), json!(right_id));
        metadata.insert("voxel_size".into(), json!(voxel_size));
        let record = store_mesh(ctx, &name, &result, metadata, "tool:cura_mesh_boolean")?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": result.stats(),
                "voxel_size": voxel_size,
            }),
            None,
        ))
    })
}

fn offset(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &["artifact_id", "distance", "voxel_size", "name"],
        )?;
        let id = args.required_string("artifact_id")?;
        let distance = args.required_f64("distance")?;
        let voxel_size = args.optional_f64("voxel_size")?.unwrap_or(0.5);
        let mesh = load_mesh(ctx, &id)?;
        check_voxels(ctx, &mesh, voxel_size)?;
        let _permit = ctx
            .process_slots
            .acquire()
            .await
            .map_err(|_| Error::Process("mesh processing queue was closed".to_string()))?;
        let result = mesh.offset(distance, voxel_size)?;
        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| derived_name("offset", "offset"));
        let name = validate_name(&name, "name")?;
        let mut metadata = Map::new();
        metadata.insert("operation".into(), json!("offset"));
        metadata.insert("distance".into(), json!(distance));
        metadata.insert("voxel_size".into(), json!(voxel_size));
        metadata.insert("source_artifact".into(), json!(id));
        let record = store_mesh(ctx, &name, &result, metadata, "tool:cura_mesh_offset")?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": result.stats(),
            }),
            None,
        ))
    })
}

fn remesh(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id", "voxel_size", "name"])?;
        let id = args.required_string("artifact_id")?;
        let voxel_size = args.optional_f64("voxel_size")?.unwrap_or(0.5);
        let source = ctx.store.get(&id)?;
        let mesh = load_mesh(ctx, &id)?;
        check_voxels(ctx, &mesh, voxel_size)?;
        let _permit = ctx
            .process_slots
            .acquire()
            .await
            .map_err(|_| Error::Process("mesh processing queue was closed".to_string()))?;
        let result = mesh.remesh(voxel_size)?;
        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| derived_name(&source.name, "remesh"));
        let mut metadata = Map::new();
        metadata.insert("operation".into(), json!("remesh"));
        metadata.insert("voxel_size".into(), json!(voxel_size));
        metadata.insert("source_artifact".into(), json!(id));
        let record = store_mesh(ctx, &name, &result, metadata, "tool:cura_mesh_remesh")?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": result.stats(),
            }),
            None,
        ))
    })
}

fn decimate(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["artifact_id", "target_triangles", "name"])?;
        let id = args.required_string("artifact_id")?;
        let target = args.required_u64("target_triangles")?;
        let target = usize::try_from(target)
            .map_err(|_| Error::InvalidArguments("target_triangles is too large".to_string()))?;
        let source = ctx.store.get(&id)?;
        let mesh = load_mesh(ctx, &id)?;
        check_triangles(ctx, mesh.triangle_count())?;
        let _permit = ctx
            .process_slots
            .acquire()
            .await
            .map_err(|_| Error::Process("mesh processing queue was closed".to_string()))?;
        let result = mesh.decimate(target)?;
        let name = args
            .optional_string("name")?
            .unwrap_or_else(|| derived_name(&source.name, "decimated"));
        let mut metadata = Map::new();
        metadata.insert("operation".into(), json!("decimate"));
        metadata.insert("source_triangles".into(), json!(mesh.triangle_count()));
        metadata.insert("target_triangles".into(), json!(target));
        metadata.insert("source_artifact".into(), json!(id));
        let record = store_mesh(ctx, &name, &result, metadata, "tool:cura_mesh_decimate")?;
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "stats": result.stats(),
            }),
            None,
        ))
    })
}

fn check_triangles(ctx: &ToolContext, triangles: usize) -> Result<()> {
    if triangles > ctx.config.policy.max_triangles {
        return Err(Error::Policy(format!(
            "mesh has {triangles} triangles, above the configured limit of {}",
            ctx.config.policy.max_triangles
        )));
    }
    Ok(())
}

fn check_voxels(ctx: &ToolContext, mesh: &Mesh, voxel_size: f64) -> Result<()> {
    check_triangles(ctx, mesh.triangle_count())?;
    let cells = mesh.voxel_cells(voxel_size);
    if cells > ctx.config.policy.max_voxel_cells as u64 {
        return Err(Error::Policy(format!(
            "voxel operation needs about {cells} cells, above the configured limit of {}; \
             increase voxel_size",
            ctx.config.policy.max_voxel_cells
        )));
    }
    Ok(())
}

fn optional_vec3(args: &Args, key: &str) -> Result<Option<[f64; 3]>> {
    let Some(value) = args.value(key) else {
        return Ok(None);
    };
    let items = value.as_array().ok_or_else(|| {
        Error::InvalidArguments(format!("'{key}' must be an array of three numbers"))
    })?;
    if items.len() != 3 {
        return Err(Error::InvalidArguments(format!(
            "'{key}' must contain exactly three numbers"
        )));
    }
    let mut out = [0.0; 3];
    for (index, item) in items.iter().enumerate() {
        let number = item
            .as_f64()
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' must contain only numbers")))?;
        if !number.is_finite() {
            return Err(Error::InvalidArguments(format!(
                "'{key}' must contain finite numbers"
            )));
        }
        out[index] = number;
    }
    Ok(Some(out))
}

fn derived_name(source: &str, suffix: &str) -> String {
    let stem = std::path::Path::new(source)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("mesh");
    format!("{stem}_{suffix}.stl")
}

//! Cura profile discovery, resolution, verification, and custom-profile tools.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::{
    error::{Error, Result},
    profiles::{
        custom_profile_file_name, custom_profile_text,
        resolve::{ProfileLayer, Resolution},
        types::{Definition, Settings},
        verify, ProfileIndex, ProfileKind,
    },
    safety::{Capability, Risk},
    tools::{
        args::{validate_name, Args},
        artifacts::record_json,
        json_result, no_summary, schema_object, Gate, ToolContext, ToolFuture, ToolSpec,
    },
    workspace::ArtifactKind,
};

/// Tool specs.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "cura_profile_list",
            title: "List Cura profiles",
            description: "List discovered definitions, quality, material, variant, intent, user, \
                          and custom profiles. Filter by kind and a case-insensitive name/path \
                          substring. Use the returned id with the other profile tools.",
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
                            "enum": [
                                "definition", "quality", "quality_changes", "material",
                                "variant", "intent", "extruder", "user", "custom", "unknown"
                            ]
                        },
                        "query": {"type": "string"},
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 5000,
                            "default": 200
                        }
                    }),
                    &[],
                )
            },
            handler: list,
        },
        ToolSpec {
            name: "cura_profile_get",
            title: "Get a Cura profile",
            description: "Load one profile by id, name, or path. Definitions return their merged \
                          setting catalog and inheritance lineage; instances and materials return \
                          their parsed values and metadata.",
            allowed_args: &["profile_id", "include_settings", "settings_limit"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "profile_id": {
                            "type": "string",
                            "description": "Profile id (prf_...), name, or path."
                        },
                        "include_settings": {
                            "type": "boolean",
                            "default": false,
                            "description": "Include the setting catalog or values map."
                        },
                        "settings_limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 20000,
                            "default": 500
                        }
                    }),
                    &["profile_id"],
                )
            },
            handler: get,
        },
        ToolSpec {
            name: "cura_profile_refresh",
            title: "Refresh profile index",
            description: "Rescan configured profile directories. Returns counts by kind and any \
                          files that were skipped. Call after writing a custom profile outside the \
                          workspace.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: crate::tools::empty_schema,
            handler: refresh,
        },
        ToolSpec {
            name: "cura_profile_resolve",
            title: "Resolve a Cura profile",
            description: "Resolve a definition plus quality, material, variant, intent, custom, \
                          and caller layers into one settings map. Includes warnings for Cura \
                          expressions that cannot be evaluated and hard temperature violations.",
            allowed_args: &[
                "definition",
                "quality",
                "quality_changes",
                "material",
                "variant",
                "intent",
                "profiles",
                "overrides",
                "extruders",
                "include_settings",
            ],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || resolution_schema(true),
            handler: resolve,
        },
        ToolSpec {
            name: "cura_profile_verify",
            title: "Verify a Cura profile",
            description: "Resolve a profile and check machine dimensions, extruder count, nozzle \
                          size, temperatures, layer height, line width, and speed. Returns errors \
                          and warnings with the offending setting names.",
            allowed_args: &[
                "definition",
                "quality",
                "quality_changes",
                "material",
                "variant",
                "intent",
                "profiles",
                "overrides",
                "extruders",
            ],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || resolution_schema(false),
            handler: verify_profile,
        },
        ToolSpec {
            name: "cura_profile_create",
            title: "Create a custom Cura profile",
            description:
                "Write a custom quality-changes `.inst.cfg` into the workspace and import \
                          it as a profile artifact. The definition must already be discoverable. \
                          Values are written verbatim under [values].",
            allowed_args: &["definition", "name", "values", "setting_version"],
            capability: Capability::WorkspaceWrite,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "definition": {
                            "type": "string",
                            "description": "Definition name the profile applies to."
                        },
                        "name": {
                            "type": "string",
                            "description": "Human-readable custom profile name."
                        },
                        "values": {
                            "type": "object",
                            "description": "Cura setting values, for example {\"layer_height\": 0.16}."
                        },
                        "setting_version": {
                            "type": "string",
                            "description": "Optional Cura setting_version metadata."
                        }
                    }),
                    &["definition", "name", "values"],
                )
            },
            handler: create,
        },
    ]
}

fn resolution_schema(include_settings: bool) -> Value {
    let mut properties = json!({
        "definition": {
            "type": "string",
            "description": "Definition id, name, or path."
        },
        "quality": {"type": "string"},
        "quality_changes": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Quality-change profiles applied in order."
        },
        "material": {"type": "string"},
        "variant": {"type": "string"},
        "intent": {"type": "string"},
        "profiles": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Extra instance profiles applied last, before caller overrides."
        },
        "overrides": {
            "type": "object",
            "description": "Caller setting overrides applied after every profile layer."
        },
        "extruders": {
            "type": "object",
            "description": "Per-extruder settings keyed by index, for example {\"0\": {\"speed_print\": 40}}."
        }
    });
    if include_settings {
        properties["include_settings"] = json!({
            "type": "boolean",
            "default": true,
            "description": "Include the full resolved settings map."
        });
    }
    schema_object(properties, &["definition"])
}

pub(crate) struct Resolved {
    pub definition: Definition,
    pub resolution: Resolution,
    pub extruders: BTreeMap<String, Settings>,
}

fn list(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["kind", "query", "limit"])?;
        let kind = args
            .optional_string("kind")?
            .map(|kind| parse_kind(&kind))
            .transpose()?;
        let query = args.optional_string("query")?;
        let limit = args
            .optional_u64("limit")?
            .map_or(200, |limit| limit.min(5000) as usize);
        let catalog = ctx.profiles.catalog()?;
        let entries = catalog.find(kind, query.as_deref());
        let total = entries.len();
        let entries = entries
            .into_iter()
            .take(limit)
            .map(|entry| json!(entry))
            .collect::<Vec<_>>();
        Ok(json_result(
            &json!({
                "count": entries.len(),
                "total_matches": total,
                "scanned_dirs": catalog.scanned_dirs,
                "definitions": catalog.definitions,
                "warnings": catalog.warnings,
                "profiles": entries,
            }),
            None,
        ))
    })
}

fn get(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &["profile_id", "include_settings", "settings_limit"],
        )?;
        let id = args.required_string("profile_id")?;
        let include_settings = args.optional_bool("include_settings")?.unwrap_or(false);
        let settings_limit = args
            .optional_u64("settings_limit")?
            .map_or(500, |limit| limit.min(20_000) as usize);
        let catalog = ctx.profiles.catalog()?;
        let entry = ProfileIndex::find_entry(&catalog, &id).cloned();
        let kind = entry.as_ref().map(|entry| entry.kind);
        match kind {
            Some(ProfileKind::Definition) => {
                let definition = ctx.profiles.definition(&id)?;
                let mut settings = if include_settings {
                    Some(truncate_settings(&definition.defaults(), settings_limit))
                } else {
                    None
                };
                if let Some(settings) = &mut settings {
                    if definition.setting_count() > settings_limit {
                        settings.insert(
                            "_truncated".to_string(),
                            json!(format!(
                                "showing {settings_limit} of {} settings",
                                definition.setting_count()
                            )),
                        );
                    }
                }
                Ok(json_result(
                    &json!({
                        "entry": entry,
                        "kind": "definition",
                        "name": definition.name,
                        "inherits": definition.inherits,
                        "lineage": definition.lineage,
                        "path": definition.path,
                        "metadata": definition.metadata,
                        "setting_count": definition.setting_count(),
                        "warnings": definition.warnings,
                        "settings": settings,
                    }),
                    None,
                ))
            }
            Some(ProfileKind::Material) => {
                let material = ctx.profiles.material(&id)?;
                Ok(json_result(
                    &json!({
                        "entry": entry,
                        "kind": "material",
                        "material": material,
                        "settings": if include_settings {
                            Some(truncate_settings(&material.mapped, settings_limit))
                        } else {
                            None
                        },
                    }),
                    None,
                ))
            }
            _ => {
                let instance = ctx.profiles.instance_any(&id)?;
                Ok(json_result(
                    &json!({
                        "entry": entry,
                        "kind": instance.kind.as_str(),
                        "instance": {
                            "name": instance.name,
                            "path": instance.path,
                            "definition": instance.definition,
                            "version": instance.version,
                            "metadata": instance.metadata,
                            "value_count": instance.values.len(),
                            "expression_count": instance.expressions.len(),
                        },
                        "values": if include_settings {
                            Some(truncate_settings(&instance.values, settings_limit))
                        } else {
                            None
                        },
                        "expressions": instance.expressions,
                    }),
                    None,
                ))
            }
        }
    })
}

fn refresh(ctx: &ToolContext, _args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let catalog = ctx.profiles.refresh()?;
        let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
        for entry in &catalog.entries {
            *by_kind.entry(entry.kind.as_str()).or_insert(0) += 1;
        }
        Ok(json_result(
            &json!({
                "profiles": catalog.entries.len(),
                "definitions": catalog.definitions.len(),
                "by_kind": by_kind,
                "scanned_dirs": catalog.scanned_dirs,
                "warnings": catalog.warnings,
            }),
            None,
        ))
    })
}

fn resolve(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let include_settings = args
            .get("include_settings")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let args = Args::parse(
            Some(args),
            &[
                "definition",
                "quality",
                "quality_changes",
                "material",
                "variant",
                "intent",
                "profiles",
                "overrides",
                "extruders",
                "include_settings",
            ],
        )?;
        let resolved = resolve_profile(ctx, &args)?;
        let violations = crate::profiles::temperature_violations(
            &resolved.resolution.values,
            &ctx.config.policy,
        );
        let settings = include_settings.then(|| resolved.resolution.values.clone());
        let explicit = resolved
            .resolution
            .explicit
            .iter()
            .filter_map(|key| {
                resolved
                    .resolution
                    .values
                    .get(key)
                    .map(|value| (key.clone(), value.clone()))
            })
            .collect::<Map<_, _>>();
        Ok(json_result(
            &json!({
                "definition": definition_summary(&resolved.definition),
                "layers": resolved.resolution.layers,
                "explicit_settings": explicit,
                "settings_count": resolved.resolution.values.len(),
                "engine_values_count": resolved.resolution.engine_values().len(),
                "extruders": resolved.extruders.keys().collect::<Vec<_>>(),
                "warnings": resolved.resolution.warnings,
                "temperature_violations": violations,
                "settings": settings,
            }),
            None,
        ))
    })
}

fn verify_profile(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &[
                "definition",
                "quality",
                "quality_changes",
                "material",
                "variant",
                "intent",
                "profiles",
                "overrides",
                "extruders",
            ],
        )?;
        let resolved = resolve_profile(ctx, &args)?;
        let report = verify::verify(
            &resolved.definition,
            &resolved.resolution,
            &ctx.config.policy,
        );
        Ok(json_result(&report, None))
    })
}

fn create(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(
            Some(args),
            &["definition", "name", "values", "setting_version"],
        )?;
        let definition_name = args.required_string("definition")?;
        let definition = ctx.profiles.definition(&definition_name)?;
        let name = validate_name(&args.required_string("name")?, "name")?;
        let values = args.required_object("values")?;
        if values.is_empty() {
            return Err(Error::InvalidArguments(
                "'values' must contain at least one setting".to_string(),
            ));
        }
        let setting_version = args.optional_string("setting_version")?;
        let file_name = custom_profile_file_name(&name)?;
        let text = custom_profile_text(
            &definition.name,
            &name,
            ProfileKind::QualityChanges,
            setting_version.as_deref(),
            &values,
        );
        let path = ctx.store.write_custom_profile(&file_name, &text)?;
        let record = ctx.store.import_file(
            &file_name,
            ArtifactKind::Profile,
            "cfg",
            &path,
            Some("tool:cura_profile_create".to_string()),
            json!({
                "definition": definition.name,
                "name": name,
                "settings": values,
            }),
        )?;
        // Make the new profile visible immediately, not only after a restart.
        let _ = ctx.profiles.refresh();
        Ok(json_result(
            &json!({
                "artifact": record_json(ctx, &record),
                "path": path,
                "definition": definition.name,
                "name": name,
                "file_name": file_name,
                "note": "Refresh the profile index if a client does not see the new profile yet.",
            }),
            None,
        ))
    })
}

pub(crate) fn resolve_profile(ctx: &ToolContext, args: &Args) -> Result<Resolved> {
    let definition_id = args.required_string("definition")?;
    let definition = ctx.profiles.definition(&definition_id)?;
    let mut layers: Vec<ProfileLayer> = Vec::new();

    if let Some(variant) = args.optional_string("variant")? {
        layers.push(instance_layer(ctx, &variant)?);
    }
    if let Some(material) = args.optional_string("material")? {
        layers.push(material_layer(ctx, &material)?);
    }
    if let Some(quality) = args.optional_string("quality")? {
        layers.push(instance_layer(ctx, &quality)?);
    }
    if let Some(intent) = args.optional_string("intent")? {
        layers.push(instance_layer(ctx, &intent)?);
    }
    if let Some(changes) = args.optional_string_list("quality_changes")? {
        for change in changes {
            layers.push(instance_layer(ctx, &change)?);
        }
    }
    if let Some(profiles) = args.optional_string_list("profiles")? {
        for profile in profiles {
            layers.push(instance_layer(ctx, &profile)?);
        }
    }

    let overrides = args.optional_object("overrides")?.unwrap_or_default();
    let extruders = args
        .optional_extruder_settings("extruders")?
        .unwrap_or_default();
    let resolution = crate::profiles::resolve(&definition, &layers, &overrides);
    let extruder_layers = crate::profiles::extruder_map(&extruders, &Settings::new());
    Ok(Resolved {
        definition,
        resolution,
        extruders: extruder_layers,
    })
}

fn instance_layer(ctx: &ToolContext, id: &str) -> Result<ProfileLayer> {
    let profile = ctx.profiles.instance_any(id)?;
    let source = format!("{} ({})", profile.name, profile.kind.as_str());
    Ok(ProfileLayer::from_values(source, profile.values))
}

fn material_layer(ctx: &ToolContext, id: &str) -> Result<ProfileLayer> {
    if let Ok(material) = ctx.profiles.material(id) {
        let source = format!("{} (material)", material.name);
        return Ok(ProfileLayer::from_values(source, material.mapped));
    }
    let profile = ctx.profiles.instance_any(id)?;
    if profile.kind != ProfileKind::Material {
        return Err(Error::InvalidArguments(format!(
            "'{id}' is not a material profile (found {})",
            profile.kind.as_str()
        )));
    }
    let source = format!("{} (material)", profile.name);
    Ok(ProfileLayer::from_values(source, profile.values))
}

fn definition_summary(definition: &Definition) -> Value {
    json!({
        "name": definition.name,
        "path": definition.path,
        "inherits": definition.inherits,
        "lineage": definition.lineage,
        "setting_count": definition.setting_count(),
        "warnings": definition.warnings,
    })
}

fn truncate_settings(settings: &Settings, limit: usize) -> Settings {
    settings
        .iter()
        .take(limit)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn parse_kind(value: &str) -> Result<ProfileKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "definition" => Ok(ProfileKind::Definition),
        "quality" => Ok(ProfileKind::Quality),
        "quality_changes" | "quality-changes" => Ok(ProfileKind::QualityChanges),
        "material" => Ok(ProfileKind::Material),
        "variant" => Ok(ProfileKind::Variant),
        "intent" => Ok(ProfileKind::Intent),
        "extruder" => Ok(ProfileKind::Extruder),
        "user" => Ok(ProfileKind::User),
        "custom" => Ok(ProfileKind::Custom),
        "unknown" => Ok(ProfileKind::Unknown),
        other => Err(Error::InvalidArguments(format!(
            "unknown profile kind '{other}'"
        ))),
    }
}

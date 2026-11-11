//! Local introspection and approval tools.

use serde_json::{json, Value};

use crate::{
    error::Error,
    safety::{Capability, Risk},
    tools::{
        args::Args, empty_schema, json_result, no_summary, schema_object, Gate, ToolContext,
        ToolFuture, ToolSpec,
    },
};

/// Tool specs.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "cura_server_info",
            title: "Server information",
            description: "Report mcp-cura version, workspace paths, transport, policy summary, \
                          approval mode, and engine configuration. Secrets are omitted.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: empty_schema,
            handler: server_info,
        },
        ToolSpec {
            name: "cura_engine_info",
            title: "CuraEngine information",
            description: "Probe the configured CuraEngine binary or Docker image and report \
                          availability, version, resolved path, search paths, and settings mode.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: empty_schema,
            handler: engine_info,
        },
        ToolSpec {
            name: "cura_policy_info",
            title: "Safety policy",
            description: "Report the active local safety policy, limits, and approval mode. Use \
                          this before attempting a write, process, export, or delete action.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: empty_schema,
            handler: policy_info,
        },
        ToolSpec {
            name: "cura_pending_approvals",
            title: "Pending approvals",
            description: "List prepared high-risk actions waiting for confirmation. Use \
                          cura_confirm_action with an approval id to execute one.",
            allowed_args: &["all"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "all": {
                            "type": "boolean",
                            "default": false,
                            "description": "Include consumed, denied, and expired records."
                        }
                    }),
                    &[],
                )
            },
            handler: pending_approvals,
        },
        ToolSpec {
            name: "cura_confirm_action",
            title: "Confirm prepared action",
            description: "Execute a previously prepared high-risk action. The stored tool and \
                          arguments are used; supplying different arguments here does nothing. \
                          Requires acknowledge=true and, depending on approval mode, a \
                          single-use confirmation phrase and/or human out-of-band approval.",
            allowed_args: &["approval_id", "confirmation_phrase", "acknowledge"],
            capability: Capability::Read,
            risk: Risk::High,
            gate: Gate::Confirm,
            summarize: no_summary,
            schema: || {
                schema_object(
                    json!({
                        "approval_id": {
                            "type": "string",
                            "description": "Approval id from the prepare response (apr_...)."
                        },
                        "confirmation_phrase": {
                            "type": "string",
                            "description": "Single-use phrase when the prompt includes one."
                        },
                        "acknowledge": {
                            "type": "boolean",
                            "description": "Must be true after reading the action summary."
                        }
                    }),
                    &["approval_id", "acknowledge"],
                )
            },
            handler: confirm_placeholder,
        },
    ]
}

fn server_info(ctx: &ToolContext, _args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let open = ctx
            .approvals
            .list_open()
            .map(|records| records.len())
            .unwrap_or(0);
        let value = json!({
            "name": "mcp-cura",
            "version": crate::VERSION,
            "config": ctx.config.public_view(),
            "engine": ctx.engine.config(),
            "workspace_root": ctx.store.root(),
            "approval_mode": ctx.approvals.mode(),
            "pending_approvals": open,
            "process_slots": ctx.process_slots.available_permits(),
        });
        Ok(json_result(&value, None))
    })
}

fn engine_info(ctx: &ToolContext, _args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let info = ctx.engine.probe().await;
        Ok(json_result(&info, None))
    })
}

fn policy_info(ctx: &ToolContext, _args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let open = ctx.approvals.list_open().unwrap_or_default();
        let value = json!({
            "policy": ctx.policy.summary(),
            "approval": {
                "mode": ctx.approvals.mode(),
                "requires_phrase": ctx.approvals.requires_phrase(),
                "requires_human": ctx.approvals.requires_human(),
                "phrase_in_response": ctx.approvals.phrase_in_response(),
                "open_approvals": open.len(),
            },
            "notes": [
                "Read-only tools are always allowed.",
                "Slicing, geometry processing, and imports create local workspace data only.",
                "Artifact export and deletion are approval-gated when enabled.",
                "This server never sends jobs to a printer."
            ]
        });
        Ok(json_result(&value, None))
    })
}

fn pending_approvals(ctx: &ToolContext, args: Value) -> ToolFuture<'_> {
    Box::pin(async move {
        let args = Args::parse(Some(args), &["all"])?;
        let all = args.optional_bool("all")?.unwrap_or(false);
        let records = if all {
            ctx.approvals.list()?
        } else {
            ctx.approvals.list_open()?
        };
        let approvals = records
            .iter()
            .map(|record| {
                json!({
                    "approval_id": record.id,
                    "tool": record.tool,
                    "summary": record.summary,
                    "risk": record.risk.as_str(),
                    "status": record.status,
                    "created_at": record.created_at,
                    "expires_at": record.expires_at,
                    "expires_in_secs": record.remaining_secs(),
                    "human_approved": record.human_approved_at.is_some(),
                    "result": record.result,
                })
            })
            .collect::<Vec<_>>();
        Ok(json_result(
            &json!({
                "count": approvals.len(),
                "mode": ctx.approvals.mode(),
                "approvals": approvals,
            }),
            None,
        ))
    })
}

fn confirm_placeholder(_ctx: &ToolContext, _args: Value) -> ToolFuture<'_> {
    Box::pin(async {
        Err(Error::Approval(
            "cura_confirm_action is handled by the registry".to_string(),
        ))
    })
}

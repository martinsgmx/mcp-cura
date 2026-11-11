//! MCP server handler: tools, resources, and prompts.
//!
//! The handler is intentionally thin. Authorization, approvals, and audit
//! logging live in [`crate::tools::ToolRegistry`], so resource and prompt reads
//! cannot bypass them either.

use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::{
    mcp_server::ServerHandler,
    schema::{
        schema_utils::CallToolError, CallToolRequestParams, CallToolResult, ContentBlock,
        GetPromptRequestParams, GetPromptResult, Implementation, InitializeResult,
        ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams, Prompt,
        PromptArgument, PromptMessage, ProtocolVersion, ReadResourceRequestParams,
        ReadResourceResult, Role, RpcError, ServerCapabilities, ServerCapabilitiesPrompts,
        ServerCapabilitiesResources, ServerCapabilitiesTools, TextContent, TextResourceContents,
    },
    McpServer,
};
use serde_json::{Map, Value};

use crate::{
    error::Error,
    tools::{text_result, tool_error, ToolContext, ToolRegistry},
};

/// Build the MCP `InitializeResult` advertised to clients.
#[must_use]
pub fn server_details() -> InitializeResult {
    InitializeResult {
        server_info: Implementation {
            name: "mcp-cura".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("mcp-cura".into()),
            description: Some(
                "Structured CuraEngine slicing: mesh inspection and preparation, profile \
                 resolution and verification, background slice jobs, and G-code review."
                    .into(),
            ),
            icons: Vec::new(),
            website_url: Some("https://github.com/martinsgmx/mcp-cura".into()),
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools {
                list_changed: Some(false),
            }),
            resources: Some(ServerCapabilitiesResources {
                list_changed: Some(false),
                subscribe: Some(false),
            }),
            prompts: Some(ServerCapabilitiesPrompts {
                list_changed: Some(false),
            }),
            completions: Some(Map::new()),
            ..Default::default()
        },
        meta: None,
        instructions: Some(instructions()),
        protocol_version: ProtocolVersion::V2025_11_25.into(),
    }
}

fn instructions() -> String {
    [
        "You are connected to mcp-cura, a local CuraEngine slicing workspace.",
        "",
        "Typical workflow:",
        "1. cura_artifact_import a mesh, then cura_mesh_inspect to check bounds, watertightness, and triangle count.",
        "2. cura_profile_list / cura_profile_get to find a machine definition and quality profile.",
        "3. cura_profile_verify or cura_profile_resolve to check the settings before spending time on a slice.",
        "4. cura_slice starts a background job; poll cura_slice_status or call cura_slice_wait.",
        "5. cura_gcode_inspect / cura_gcode_verify review the result; cura_artifact_export copies it out.",
        "",
        "Safety rules:",
        "- Import paths are limited to configured roots; export and deletion require approval.",
        "- Resolved profiles must stay within the configured temperature limits.",
        "- G-code is never executed and no printer is contacted by this server.",
        "- When a tool returns an approval prompt, call cura_confirm_action with the approval id.",
        "- Never claim a slice succeeded unless the job state is completed.",
        "",
        "Prefer artifacts over paths: every mesh, profile, and G-code output has a stable art_ id. \
         Re-importing identical bytes returns the same id.",
    ]
    .join("\n")
}

/// MCP server handler.
pub struct CuraServerHandler {
    registry: Arc<ToolRegistry>,
    context: Arc<ToolContext>,
}

impl CuraServerHandler {
    /// Create a handler around a registry and its shared context.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>, context: Arc<ToolContext>) -> Self {
        Self { registry, context }
    }

    async fn resource_text(&self, tool: &str) -> Result<String, RpcError> {
        let result = self
            .registry
            .call(&self.context, tool, None)
            .await
            .map_err(|err| RpcError::internal_error().with_message(err.public_message()))?;
        if result.is_error == Some(true) {
            let message = result
                .content
                .iter()
                .find_map(|block| match block {
                    ContentBlock::TextContent(text) => Some(text.text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "resource read failed".to_string());
            return Err(RpcError::internal_error().with_message(message));
        }
        let text = result
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::TextContent(text) => Some(text.text.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                RpcError::internal_error().with_message("resource returned no text content")
            })?;
        Ok(text)
    }
}

#[async_trait]
impl ServerHandler for CuraServerHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: self.registry.list_tools(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        if self.registry.spec(&params.name).is_none() {
            return Err(CallToolError::unknown_tool(params.name));
        }
        let arguments = params.arguments.map(Value::Object);
        match self
            .registry
            .call(&self.context, &params.name, arguments)
            .await
        {
            Ok(result) => Ok(result),
            Err(err) => Ok(tool_error(&err.public_message())),
        }
    }

    async fn handle_list_resources_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListResourcesResult, RpcError> {
        Ok(ListResourcesResult {
            resources: resource_catalog(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_read_resource_request(
        &self,
        params: ReadResourceRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ReadResourceResult, RpcError> {
        let tool = match params.uri.as_str() {
            "cura://server" => "cura_server_info",
            "cura://engine" => "cura_engine_info",
            "cura://policy" => "cura_policy_info",
            "cura://profiles" => "cura_profile_list",
            "cura://artifacts" => "cura_artifact_list",
            "cura://approvals" => "cura_pending_approvals",
            other => {
                return Err(RpcError::invalid_params()
                    .with_message(format!("unknown resource URI '{other}'")));
            }
        };

        let text = self.resource_text(tool).await?;
        Ok(ReadResourceResult {
            contents: vec![TextResourceContents::new(text, params.uri)
                .with_mime_type("application/json")
                .into()],
            meta: None,
        })
    }

    async fn handle_list_prompts_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListPromptsResult, RpcError> {
        Ok(ListPromptsResult {
            prompts: prompt_catalog(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_get_prompt_request(
        &self,
        params: GetPromptRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<GetPromptResult, RpcError> {
        let arguments = params.arguments.unwrap_or_default();
        let (description, text) = match params.name.as_str() {
            "slice_model" => {
                let model = arguments
                    .get("model_artifact_id")
                    .map_or("the imported model", String::as_str)
                    .to_string();
                (
                    "Prepare and slice a model with a verified Cura profile.",
                    format!(
                        "Slice {model}. First inspect the mesh with cura_mesh_inspect and fix \
                         obvious problems (not watertight, wrong units, below the bed). Then list \
                         profiles, pick a definition and quality, and call cura_profile_verify. If \
                         verification has errors, resolve them or explain them before slicing. \
                         Start the slice with cura_slice, poll cura_slice_status until it is \
                         terminal, then run cura_gcode_verify and report time, filament, layers, \
                         and any warnings. Do not export the file unless asked."
                    ),
                )
            }
            "profile_audit" => {
                let definition = arguments
                    .get("definition")
                    .map_or("the selected definition", String::as_str)
                    .to_string();
                (
                    "Audit a machine and quality profile before printing.",
                    format!(
                        "Audit {definition}. Resolve the profile with cura_profile_resolve and \
                         verify it with cura_profile_verify. Report machine dimensions, nozzle \
                         size, layer height, line width, speeds, temperatures, and every error or \
                         warning. Call out any setting that exceeds the server policy. Do not \
                         change files or start a slice."
                    ),
                )
            }
            "gcode_review" => {
                let artifact = arguments
                    .get("artifact_id")
                    .map_or("the sliced G-code artifact", String::as_str)
                    .to_string();
                (
                    "Review sliced G-code before printing.",
                    format!(
                        "Review {artifact} with cura_gcode_inspect and cura_gcode_verify. Report \
                         estimated time, filament, layer count, Z range, maximum hotend and bed \
                         temperatures, dangerous commands, and missing shutdown commands. If the \
                         file needs a shutdown sequence, use cura_gcode_prepare and review the new \
                         artifact."
                    ),
                )
            }
            other => {
                return Err(
                    RpcError::invalid_params().with_message(format!("unknown prompt '{other}'"))
                );
            }
        };

        Ok(GetPromptResult {
            description: Some(description.to_string()),
            messages: vec![PromptMessage {
                role: Role::User,
                content: ContentBlock::TextContent(TextContent::from(text)),
            }],
            meta: None,
        })
    }
}

fn resource_catalog() -> Vec<rust_mcp_sdk::schema::Resource> {
    [
        (
            "cura://server",
            "Server information",
            "Version, workspace, policy, and engine configuration.",
        ),
        (
            "cura://engine",
            "CuraEngine status",
            "Engine availability, version, runner, and search paths.",
        ),
        (
            "cura://policy",
            "Safety policy",
            "Local capabilities, limits, and approval mode.",
        ),
        (
            "cura://profiles",
            "Profile catalog",
            "Discovered Cura definitions and profiles.",
        ),
        (
            "cura://artifacts",
            "Artifact catalog",
            "Recently stored meshes, profiles, and G-code.",
        ),
        (
            "cura://approvals",
            "Pending approvals",
            "High-risk actions waiting for confirmation.",
        ),
    ]
    .into_iter()
    .map(|(uri, name, description)| rust_mcp_sdk::schema::Resource {
        uri: uri.to_string(),
        name: name.to_string(),
        title: Some(name.to_string()),
        description: Some(description.to_string()),
        mime_type: Some("application/json".to_string()),
        size: None,
        annotations: None,
        icons: Vec::new(),
        meta: None,
    })
    .collect()
}

fn prompt_catalog() -> Vec<Prompt> {
    vec![
        Prompt {
            name: "slice_model".to_string(),
            title: Some("Slice a model".to_string()),
            description: Some(
                "Inspect, verify, slice, and review a mesh using the safe artifact workflow."
                    .to_string(),
            ),
            arguments: vec![PromptArgument {
                name: "model_artifact_id".to_string(),
                title: Some("Model artifact".to_string()),
                description: Some("Mesh artifact id (art_...).".to_string()),
                required: Some(false),
            }],
            icons: Vec::new(),
            meta: None,
        },
        Prompt {
            name: "profile_audit".to_string(),
            title: Some("Audit a profile".to_string()),
            description: Some("Read-only machine and quality profile review.".to_string()),
            arguments: vec![PromptArgument {
                name: "definition".to_string(),
                title: Some("Definition".to_string()),
                description: Some("Definition id, name, or path.".to_string()),
                required: Some(false),
            }],
            icons: Vec::new(),
            meta: None,
        },
        Prompt {
            name: "gcode_review".to_string(),
            title: Some("Review G-code".to_string()),
            description: Some("Read-only review of a sliced G-code artifact.".to_string()),
            arguments: vec![PromptArgument {
                name: "artifact_id".to_string(),
                title: Some("G-code artifact".to_string()),
                description: Some("G-code artifact id (art_...).".to_string()),
                required: Some(false),
            }],
            icons: Vec::new(),
            meta: None,
        },
    ]
}

/// Convenience conversion used by doctor output.
#[must_use]
pub fn error_text(err: &Error) -> String {
    err.public_message()
}

/// Plain text helper used by tests.
#[must_use]
pub fn text(value: impl Into<String>) -> CallToolResult {
    text_result(value)
}

//! Local policy engine.
//!
//! Every tool declares a [`Capability`] and a [`Risk`]. The policy decides
//! whether the action runs immediately, needs a single-use approval, or is
//! denied outright. No tool may bypass this layer.

use serde::{Deserialize, Serialize};

pub use crate::config::PolicyConfig;

/// What class of effect a tool has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Pure read of the workspace, profiles, or G-code.
    Read,
    /// Creates artifacts, job directories, or custom profiles.
    WorkspaceWrite,
    /// Spawns `CuraEngine` or performs CPU-heavy mesh processing.
    Process,
    /// Copies an artifact outside the managed workspace.
    Export,
    /// Deletes workspace data.
    Destructive,
}

impl Capability {
    /// Stable label used in audit records and tool metadata.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::WorkspaceWrite => "workspace_write",
            Self::Process => "process",
            Self::Export => "export",
            Self::Destructive => "destructive",
        }
    }
}

/// How dangerous an action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Read-only.
    Read,
    /// Reversible, low-impact workspace change.
    Low,
    /// Potentially expensive or surprising operation.
    Medium,
    /// Overwrites or removes user-visible data.
    High,
    /// Affects data outside the managed workspace.
    Critical,
}

impl Risk {
    /// Human label used in approval prompts and audit records.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Result of a policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Execute immediately.
    Allow,
    /// Create an approval request and do not execute yet.
    NeedsApproval,
    /// Refuse with a human-readable reason.
    Deny(String),
}

/// Policy evaluator.
#[derive(Debug, Clone)]
pub struct Policy {
    config: PolicyConfig,
}

impl Policy {
    /// Build a policy from configuration.
    #[must_use]
    pub fn new(config: PolicyConfig) -> Self {
        Self { config }
    }

    /// Access the underlying configuration.
    #[must_use]
    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// Evaluate a capability/risk pair.
    #[must_use]
    pub fn evaluate(&self, capability: Capability, risk: Risk) -> Decision {
        if capability == Capability::Read || risk == Risk::Read {
            return Decision::Allow;
        }

        if self.config.read_only {
            return Decision::Deny(
                "server is in read-only mode (MCP_CURA_READ_ONLY=true)".to_string(),
            );
        }

        match capability {
            Capability::Read => Decision::Allow,
            Capability::WorkspaceWrite => {
                if !self.config.allow_workspace_writes {
                    return Decision::Deny(
                        "workspace writes are disabled (MCP_CURA_ALLOW_WORKSPACE_WRITES=false)"
                            .to_string(),
                    );
                }
                if risk >= Risk::High {
                    Decision::NeedsApproval
                } else {
                    Decision::Allow
                }
            }
            Capability::Process => {
                if self.config.allow_slicing {
                    Decision::Allow
                } else {
                    Decision::Deny(
                        "slicing and mesh processing are disabled \
                         (MCP_CURA_ALLOW_SLICING=false)"
                            .to_string(),
                    )
                }
            }
            Capability::Export => {
                if !self.config.allow_export {
                    return Decision::Deny(
                        "artifact export is disabled (MCP_CURA_ALLOW_EXPORT=false)".to_string(),
                    );
                }
                if risk >= Risk::High {
                    Decision::NeedsApproval
                } else {
                    Decision::Allow
                }
            }
            Capability::Destructive => {
                if self.config.allow_delete {
                    Decision::NeedsApproval
                } else {
                    Decision::Deny(
                        "artifact deletion is disabled (MCP_CURA_ALLOW_DELETE=false)".to_string(),
                    )
                }
            }
        }
    }

    /// Secret-free policy summary for the `cura_policy_info` tool.
    #[must_use]
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "read_only": self.config.read_only,
            "allow_workspace_writes": self.config.allow_workspace_writes,
            "allow_slicing": self.config.allow_slicing,
            "allow_export": self.config.allow_export,
            "allow_delete": self.config.allow_delete,
            "max_hotend_temp": self.config.max_hotend_temp,
            "max_bed_temp": self.config.max_bed_temp,
            "max_triangles": self.config.max_triangles,
            "max_voxel_cells": self.config.max_voxel_cells,
            "max_concurrent_jobs": self.config.max_concurrent_jobs,
            "max_slice_secs": self.config.max_slice_secs,
            "approval_required_for": ["destructive", "high-risk workspace writes", "overwrite exports"],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy::new(PolicyConfig::default())
    }

    #[test]
    fn reads_are_always_allowed() {
        assert_eq!(
            policy().evaluate(Capability::Read, Risk::Read),
            Decision::Allow
        );
    }

    #[test]
    fn destructive_needs_approval() {
        assert_eq!(
            policy().evaluate(Capability::Destructive, Risk::High),
            Decision::NeedsApproval
        );
    }

    #[test]
    fn read_only_denies_writes_and_processing() {
        let config = PolicyConfig {
            read_only: true,
            ..PolicyConfig::default()
        };
        let policy = Policy::new(config);
        assert!(matches!(
            policy.evaluate(Capability::WorkspaceWrite, Risk::Low),
            Decision::Deny(_)
        ));
        assert!(matches!(
            policy.evaluate(Capability::Process, Risk::Medium),
            Decision::Deny(_)
        ));
        assert_eq!(
            policy.evaluate(Capability::Read, Risk::Read),
            Decision::Allow
        );
    }

    #[test]
    fn disabled_capabilities_are_denied() {
        let config = PolicyConfig {
            allow_slicing: false,
            allow_export: false,
            allow_delete: false,
            ..PolicyConfig::default()
        };
        let policy = Policy::new(config);
        assert!(matches!(
            policy.evaluate(Capability::Process, Risk::Medium),
            Decision::Deny(_)
        ));
        assert!(matches!(
            policy.evaluate(Capability::Export, Risk::Low),
            Decision::Deny(_)
        ));
        assert!(matches!(
            policy.evaluate(Capability::Destructive, Risk::High),
            Decision::Deny(_)
        ));
    }
}

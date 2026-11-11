//! Safety layer: local policy, secret redaction, and approvals.
//!
//! The types here are deliberately independent of MCP transport details. Every
//! tool call passes through [`policy::Policy`] and, for high-risk actions, the
//! file-backed [`approval::ApprovalStore`] before any side effect happens.

pub mod approval;
pub mod policy;
pub mod redact;

pub use approval::{ApprovalPrompt, ApprovalRecord, ApprovalStatus, ApprovalStore};
pub use policy::{Capability, Decision, Policy, Risk};

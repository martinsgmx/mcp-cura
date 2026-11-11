//! `mcp-cura`: a structured Model Context Protocol server for Cura.
//!
//! The crate is split into narrow layers:
//!
//! * [`config`] loads TOML and environment settings.
//! * [`workspace`] owns the content-addressed artifact store and job dirs.
//! * [`geometry`] loads, inspects, and transforms meshes.
//! * [`profiles`] parses and resolves Cura definitions, instances, and materials.
//! * [`engine`] probes and invokes `CuraEngine` (locally or through Docker).
//! * [`gcode`] analyses and prepares generated G-code.
//! * [`slicing`] runs slice jobs in the background.
//! * [`safety`] is the policy and approval layer.
//! * [`tools`] exposes the MCP tool registry.
//! * [`server`] and [`transport`] speak MCP over stdio or HTTP.
//!
//! Nothing in this crate executes G-code or talks to a printer. The output is an
//! artifact that a human or a separate printer-control tool can inspect first.

pub mod audit;
pub mod config;
pub mod engine;
pub mod error;
pub mod gcode;
pub mod geometry;
pub mod profiles;
pub mod safety;
pub mod slicing;
pub mod tools;
pub mod workspace;

pub mod server;
pub mod transport;

/// Crate version, shown in server info.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

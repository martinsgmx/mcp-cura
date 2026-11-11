//! Error types used across the crate.
//!
//! Errors are split between operational failures (binary missing, malformed
//! STL, engine failure) and policy failures (an action is disabled or needs
//! approval). [`Error::public_message`] produces a short, secret-free message
//! suitable for an MCP client.

use thiserror::Error;

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid or unsafe configuration.
    #[error("configuration error: {0}")]
    Config(String),

    /// The engine could not be discovered or executed.
    #[error("CuraEngine error: {0}")]
    Engine(String),

    /// Mesh loading, analysis, or manipulation failed.
    #[error("mesh error: {0}")]
    Mesh(String),

    /// Profile parsing, resolution, or validation failed.
    #[error("profile error: {0}")]
    Profile(String),

    /// Artifact store failure.
    #[error("artifact error: {0}")]
    Artifact(String),

    /// Slice job failure.
    #[error("job error: {0}")]
    Job(String),

    /// G-code parsing or validation failure.
    #[error("G-code error: {0}")]
    Gcode(String),

    /// CPU-heavy geometry processing failed or was unavailable.
    #[error("process error: {0}")]
    Process(String),

    /// The request was refused by the local policy engine.
    #[error("policy denied: {0}")]
    Policy(String),

    /// A tool was called with missing, malformed, or unknown arguments.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    /// An approval could not be created, found, or verified.
    #[error("approval error: {0}")]
    Approval(String),

    /// Filesystem failure.
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// TOML configuration parsing failure.
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),

    /// INI profile parsing failure.
    #[error("INI error: {0}")]
    Ini(#[from] ini::Error),

    /// XML material profile parsing failure.
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),

    /// URL parsing failure.
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
}

impl Error {
    /// Return a short, secret-free description suitable for an MCP client.
    #[must_use]
    pub fn public_message(&self) -> String {
        match self {
            Self::Config(message)
            | Self::Engine(message)
            | Self::Mesh(message)
            | Self::Profile(message)
            | Self::Artifact(message)
            | Self::Job(message)
            | Self::Gcode(message)
            | Self::Process(message)
            | Self::Policy(message)
            | Self::InvalidArguments(message)
            | Self::Approval(message) => message.clone(),
            Self::Io(err) => format!("filesystem error: {err}"),
            Self::Json(err) => format!("JSON error: {err}"),
            Self::Toml(err) => format!("TOML error: {err}"),
            Self::Ini(err) => format!("INI error: {err}"),
            Self::Xml(err) => format!("XML error: {err}"),
            Self::Url(err) => format!("URL error: {err}"),
        }
    }
}

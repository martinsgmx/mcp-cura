//! Managed workspace and content-addressed artifact store.
//!
//! Layout under `workspace.root`:
//!
//! ```text
//! blobs/<sha256>.<format>   immutable artifact bytes
//! meta/<artifact_id>.json   artifact metadata
//! jobs/<job_id>/            slice job inputs, logs, and outputs
//! profiles/                 custom `.inst.cfg` profiles written by tools
//! tmp/                      atomic-write staging
//! ```
//!
//! Artifact ids are derived from the SHA-256 of the content, so importing the
//! same bytes twice returns the same id. Metadata is written with an atomic
//! rename; blobs are immutable once published.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{Error, Result};

pub use crate::config::WorkspaceConfig;

/// Broad category of an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Mesh,
    Gcode,
    Profile,
    Log,
    Job,
    Other,
}

impl ArtifactKind {
    /// Stable label used in filters and output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Gcode => "gcode",
            Self::Profile => "profile",
            Self::Log => "log",
            Self::Job => "job",
            Self::Other => "other",
        }
    }
}

impl std::str::FromStr for ArtifactKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mesh" | "model" | "stl" => Ok(Self::Mesh),
            "gcode" | "g-code" | "gc" => Ok(Self::Gcode),
            "profile" | "config" => Ok(Self::Profile),
            "log" => Ok(Self::Log),
            "job" => Ok(Self::Job),
            "other" | "file" | "artifact" => Ok(Self::Other),
            other => Err(format!(
                "unknown artifact kind '{other}' (expected mesh, gcode, profile, log, job or other)"
            )),
        }
    }
}

/// Metadata for one stored artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// Stable content-derived id (`art_<sha256 prefix>`).
    pub id: String,
    /// Full SHA-256 of the content.
    pub sha256: String,
    /// Human-readable original name.
    pub name: String,
    pub kind: ArtifactKind,
    /// File format/extension, for example `stl` or `gcode`.
    pub format: String,
    pub bytes: u64,
    pub created_at: u64,
    /// Where the bytes came from (path, job, or tool).
    #[serde(default)]
    pub source: Option<String>,
    /// Tool-specific structured metadata.
    #[serde(default)]
    pub metadata: Value,
}

impl ArtifactRecord {
    /// Whether this is a mesh artifact.
    #[must_use]
    pub fn is_mesh(&self) -> bool {
        self.kind == ArtifactKind::Mesh
    }
}

/// Content-addressed artifact store.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    blobs: PathBuf,
    meta: PathBuf,
    jobs: PathBuf,
    profiles: PathBuf,
    tmp: PathBuf,
    config: WorkspaceConfig,
}

impl ArtifactStore {
    /// Open (and create) the store under `config.root`.
    pub fn open(config: &WorkspaceConfig) -> Result<Self> {
        let root = PathBuf::from(&config.root);
        let store = Self {
            blobs: root.join("blobs"),
            meta: root.join("meta"),
            jobs: root.join("jobs"),
            profiles: root.join("profiles"),
            tmp: root.join("tmp"),
            root,
            config: config.clone(),
        };
        for dir in [
            &store.root,
            &store.blobs,
            &store.meta,
            &store.jobs,
            &store.profiles,
            &store.tmp,
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(store)
    }

    /// Workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Job directory root.
    #[must_use]
    pub fn jobs_dir(&self) -> &Path {
        &self.jobs
    }

    /// Custom profile directory.
    #[must_use]
    pub fn profiles_dir(&self) -> &Path {
        &self.profiles
    }

    /// Temporary staging directory.
    #[must_use]
    pub fn tmp_dir(&self) -> &Path {
        &self.tmp
    }

    /// Store bytes as an artifact, deduplicating identical content.
    pub fn import_bytes(
        &self,
        name: &str,
        kind: ArtifactKind,
        format: &str,
        bytes: &[u8],
        source: Option<String>,
        metadata: Value,
    ) -> Result<ArtifactRecord> {
        if bytes.is_empty() {
            return Err(Error::Artifact("cannot store an empty artifact".into()));
        }
        if bytes.len() > self.config.max_artifact_bytes {
            return Err(Error::Artifact(format!(
                "artifact is {} bytes, above the {} byte limit",
                bytes.len(),
                self.config.max_artifact_bytes
            )));
        }

        let name = sanitize_name(name, format);
        let format = normalize_format(format, &name);
        let sha256 = sha256_hex(bytes);
        let id = format!("art_{}", &sha256[..32]);

        if let Ok(existing) = self.get(&id) {
            return Ok(existing);
        }

        if self.list(None, None, usize::MAX)?.len() >= self.config.max_artifacts {
            return Err(Error::Artifact(format!(
                "artifact store is full ({} artifacts); delete unused artifacts first",
                self.config.max_artifacts
            )));
        }

        let blob = self.blobs.join(format!("{sha256}.{format}"));
        if !blob.exists() {
            let temp = self
                .tmp
                .join(format!(".{}.{}.tmp", sha256, Uuid::new_v4().simple()));
            {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            fs::rename(&temp, &blob)?;
        }

        let record = ArtifactRecord {
            id,
            sha256,
            name,
            kind,
            format,
            bytes: bytes.len() as u64,
            created_at: unix_now(),
            source,
            metadata,
        };
        self.write_record(&record)?;
        Ok(record)
    }

    /// Read a file and store it.
    pub fn import_file(
        &self,
        name: &str,
        kind: ArtifactKind,
        format: &str,
        path: &Path,
        source: Option<String>,
        metadata: Value,
    ) -> Result<ArtifactRecord> {
        let size = fs::metadata(path)?.len();
        if size > self.config.max_import_bytes as u64 {
            return Err(Error::Artifact(format!(
                "file is {size} bytes, above the {} byte import limit",
                self.config.max_import_bytes
            )));
        }
        let bytes = fs::read(path)?;
        self.import_bytes(name, kind, format, &bytes, source, metadata)
    }

    /// Load metadata by id.
    pub fn get(&self, id: &str) -> Result<ArtifactRecord> {
        validate_artifact_id(id)?;
        let path = self.meta.join(format!("{id}.json"));
        let text = fs::read_to_string(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Error::Artifact(format!("artifact '{id}' was not found"))
            } else {
                Error::Io(err)
            }
        })?;
        serde_json::from_str(&text).map_err(Error::Json)
    }

    /// Path to the immutable blob for a record.
    #[must_use]
    pub fn blob_path(&self, record: &ArtifactRecord) -> PathBuf {
        self.blobs
            .join(format!("{}.{}", record.sha256, record.format))
    }

    /// Read artifact bytes.
    pub fn read_bytes(&self, id: &str) -> Result<Vec<u8>> {
        let record = self.get(id)?;
        let path = self.blob_path(&record);
        let bytes = fs::read(&path)?;
        if bytes.len() as u64 != record.bytes {
            return Err(Error::Artifact(format!(
                "artifact '{id}' is corrupt: expected {} bytes, found {}",
                record.bytes,
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    /// Read a text artifact with a size cap.
    pub fn read_text(&self, id: &str, max_bytes: usize) -> Result<String> {
        let record = self.get(id)?;
        if record.bytes > max_bytes as u64 {
            return Err(Error::Artifact(format!(
                "artifact '{id}' is too large to read as text ({} bytes)",
                record.bytes
            )));
        }
        let bytes = self.read_bytes(id)?;
        String::from_utf8(bytes)
            .map_err(|_| Error::Artifact(format!("artifact '{id}' is not valid UTF-8 text")))
    }

    /// List artifacts, newest first.
    pub fn list(
        &self,
        kind: Option<ArtifactKind>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ArtifactRecord>> {
        let query = query.map(str::to_ascii_lowercase);
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.meta)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<ArtifactRecord>(&text) else {
                continue;
            };
            if kind.is_some_and(|kind| record.kind != kind) {
                continue;
            }
            if let Some(query) = &query {
                let haystack = format!(
                    "{} {} {} {}",
                    record.id, record.name, record.format, record.sha256
                )
                .to_ascii_lowercase();
                if !haystack.contains(query) {
                    continue;
                }
            }
            records.push(record);
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        records.truncate(limit);
        Ok(records)
    }

    /// Delete an artifact and its blob.
    pub fn delete(&self, id: &str) -> Result<ArtifactRecord> {
        let record = self.get(id)?;
        let meta = self.meta.join(format!("{id}.json"));
        fs::remove_file(meta)?;
        // Ids are content-addressed, so no other record can reference this blob.
        let blob = self.blob_path(&record);
        if blob.exists() {
            fs::remove_file(blob)?;
        }
        Ok(record)
    }

    /// Remove orphan blobs and stale temporary files. Returns counts.
    pub fn gc(&self) -> Result<(usize, usize)> {
        let mut referenced = std::collections::HashSet::new();
        for record in self.list(None, None, usize::MAX)? {
            referenced.insert(format!("{}.{}", record.sha256, record.format));
        }
        let mut removed_blobs = 0;
        for entry in fs::read_dir(&self.blobs)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !referenced.contains(&name) && fs::remove_file(entry.path()).is_ok() {
                removed_blobs += 1;
            }
        }
        let mut removed_tmp = 0;
        for entry in fs::read_dir(&self.tmp)? {
            let entry = entry?;
            if fs::remove_file(entry.path()).is_ok() {
                removed_tmp += 1;
            }
        }
        Ok((removed_blobs, removed_tmp))
    }

    /// Create a unique job directory.
    pub fn create_job_dir(&self, job_id: &str) -> Result<PathBuf> {
        validate_job_id(job_id)?;
        let path = self.jobs.join(job_id);
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Path to a job directory.
    #[must_use]
    pub fn job_dir(&self, job_id: &str) -> PathBuf {
        self.jobs.join(job_id)
    }

    /// Write a custom profile file under the workspace.
    pub fn write_custom_profile(&self, file_name: &str, contents: &str) -> Result<PathBuf> {
        let name = sanitize_name(file_name, "cfg");
        if !name.ends_with(".inst.cfg") {
            return Err(Error::Artifact(
                "custom profile names must end with .inst.cfg".into(),
            ));
        }
        let path = self.profiles.join(name);
        let temp = self
            .tmp
            .join(format!(".profile.{}.tmp", Uuid::new_v4().simple()));
        fs::write(&temp, contents)?;
        fs::rename(temp, &path)?;
        Ok(path)
    }

    fn write_record(&self, record: &ArtifactRecord) -> Result<()> {
        let path = self.meta.join(format!("{}.json", record.id));
        let temp = self
            .tmp
            .join(format!(".meta.{}.tmp", Uuid::new_v4().simple()));
        fs::write(&temp, serde_json::to_vec_pretty(record)?)?;
        fs::rename(temp, path)?;
        Ok(())
    }
}

/// Validate a content-addressed artifact id.
pub fn validate_artifact_id(id: &str) -> Result<()> {
    let Some(hex) = id.strip_prefix("art_") else {
        return Err(Error::Artifact(format!(
            "'{id}' is not an artifact id (expected art_...)"
        )));
    };
    if hex.len() != 32 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(Error::Artifact(format!(
            "'{id}' is not a valid artifact id"
        )));
    }
    Ok(())
}

/// Validate a job id.
pub fn validate_job_id(id: &str) -> Result<()> {
    let Some(hex) = id.strip_prefix("job_") else {
        return Err(Error::Job(format!(
            "'{id}' is not a job id (expected job_...)"
        )));
    };
    if hex.is_empty()
        || hex.len() > 40
        || !hex
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(Error::Job(format!("'{id}' is not a valid job id")));
    }
    Ok(())
}

/// Hash bytes with SHA-256.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn sanitize_name(name: &str, format: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    let cleaned: String = base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        format!("artifact.{}", normalize_format(format, "bin"))
    } else {
        trimmed.to_string()
    }
}

fn normalize_format(format: &str, name: &str) -> String {
    let candidate = if format.trim().is_empty() {
        Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("bin")
    } else {
        format.trim()
    };
    let cleaned: String = candidate
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    if cleaned.is_empty() {
        "bin".to_string()
    } else {
        cleaned
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig {
            root: dir.path().to_string_lossy().to_string(),
            max_artifacts: 10,
            ..WorkspaceConfig::default()
        };
        let store = ArtifactStore::open(&config).unwrap();
        (dir, store)
    }

    #[test]
    fn content_addressing_deduplicates() {
        let (_dir, store) = store();
        let first = store
            .import_bytes(
                "cube.stl",
                ArtifactKind::Mesh,
                "stl",
                b"solid cube",
                None,
                Value::Null,
            )
            .unwrap();
        let second = store
            .import_bytes(
                "copy.stl",
                ArtifactKind::Mesh,
                "stl",
                b"solid cube",
                None,
                Value::Null,
            )
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(store.list(None, None, 10).unwrap().len(), 1);
        assert_eq!(store.read_bytes(&first.id).unwrap(), b"solid cube");
    }

    #[test]
    fn rejects_oversized_artifacts() {
        let (_dir, mut store) = store();
        store.config.max_artifact_bytes = 4;
        assert!(store
            .import_bytes(
                "x.gcode",
                ArtifactKind::Gcode,
                "gcode",
                b"12345",
                None,
                Value::Null
            )
            .is_err());
    }

    #[test]
    fn delete_removes_blob() {
        let (_dir, store) = store();
        let record = store
            .import_bytes(
                "x.gcode",
                ArtifactKind::Gcode,
                "gcode",
                b"G28\n",
                None,
                Value::Null,
            )
            .unwrap();
        let blob = store.blob_path(&record);
        assert!(blob.exists());
        store.delete(&record.id).unwrap();
        assert!(!blob.exists());
        assert!(store.get(&record.id).is_err());
    }

    #[test]
    fn rejects_bad_ids() {
        let (_dir, store) = store();
        assert!(store.get("../meta/x").is_err());
        assert!(store.get("art_xyz").is_err());
        assert!(validate_job_id("job_../../x").is_err());
    }

    #[test]
    fn gc_removes_orphans() {
        let (_dir, store) = store();
        fs::write(store.blobs.join("orphan.stl"), b"x").unwrap();
        let (blobs, _tmp) = store.gc().unwrap();
        assert_eq!(blobs, 1);
    }
}

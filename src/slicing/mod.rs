//! Background slice jobs.
//!
//! A slice can take minutes, so it runs in a background task while the MCP
//! session stays responsive. Job state is persisted as `job.json` inside the
//! workspace job directory, which means `status`, `wait`, and `list` work even
//! across a server restart. Concurrent jobs are bounded by a semaphore; a
//! cancellation token lets a running engine process be killed.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    engine::{Engine, SlicePlan},
    error::{Error, Result},
    gcode::{self, GcodeStats},
    workspace::{ArtifactKind, ArtifactStore},
};

/// Lifecycle state of a slice job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    /// Whether the job has reached a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Stable label for tool output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Engine stdout/stderr captured for a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLogs {
    pub stdout: String,
    pub stderr: String,
}

/// Persisted record for one slice job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub name: String,
    pub state: JobState,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub model_artifacts: Vec<String>,
    pub definition: Option<String>,
    pub profile_summary: Value,
    pub gcode_artifact: Option<String>,
    pub gcode_bytes: Option<u64>,
    pub gcode_stats: Option<GcodeStats>,
    pub logs: Option<JobLogs>,
    pub error: Option<String>,
    /// Free-form request details, useful for debugging without storing secrets.
    pub request: Value,
}

/// A job submitted to [`JobManager`].
#[derive(Debug, Clone)]
pub struct SliceJobRequest {
    pub name: String,
    pub plan: SlicePlan,
    pub model_artifacts: Vec<String>,
    pub definition: Option<String>,
    pub profile_summary: Value,
    pub request: Value,
}

/// Manager for background slice jobs.
#[derive(Clone)]
pub struct JobManager {
    store: Arc<ArtifactStore>,
    engine: Arc<Engine>,
    semaphore: Arc<Semaphore>,
    keep_jobs: usize,
    tokens: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl JobManager {
    /// Create a manager. `max_concurrent` must be at least one.
    #[must_use]
    pub fn new(
        store: Arc<ArtifactStore>,
        engine: Arc<Engine>,
        max_concurrent: usize,
        keep_jobs: usize,
    ) -> Self {
        Self {
            store,
            engine,
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            keep_jobs,
            tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Submit a job and return its initial queued record.
    pub fn start(&self, request: SliceJobRequest) -> Result<JobRecord> {
        let id = format!("job_{}", uuid::Uuid::new_v4().simple());
        let job_dir = self.store.create_job_dir(&id)?;
        let now = unix_now();
        let record = JobRecord {
            id: id.clone(),
            name: request.name.clone(),
            state: JobState::Queued,
            created_at: now,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            model_artifacts: request.model_artifacts.clone(),
            definition: request.definition.clone(),
            profile_summary: request.profile_summary.clone(),
            gcode_artifact: None,
            gcode_bytes: None,
            gcode_stats: None,
            logs: None,
            error: None,
            request: request.request.clone(),
        };
        write_record(&job_dir, &record)?;

        let manager = self.clone();
        tokio::spawn(async move {
            manager.run(id, request.plan).await;
        });
        Ok(record)
    }

    async fn run(&self, id: String, plan: SlicePlan) {
        let Ok(permit) = self.semaphore.clone().acquire_owned().await else {
            return;
        };

        let job_dir = self.store.job_dir(&id);
        let Ok(mut record) = read_record(&job_dir) else {
            return;
        };
        if record.state == JobState::Cancelled {
            drop(permit);
            return;
        }

        let token = CancellationToken::new();
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(id.clone(), token.clone());
        }

        record.state = JobState::Running;
        record.started_at = Some(unix_now());
        if write_record(&job_dir, &record).is_err() {
            return;
        }

        let started = std::time::Instant::now();
        let outcome = self
            .engine
            .slice_cancellable(&plan, &job_dir, Some(&token))
            .await;
        let duration = started.elapsed();

        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(&id);
        }
        drop(permit);

        match outcome {
            Ok(outcome) => {
                let gcode_path = Path::new(&outcome.gcode_path);
                let bytes = std::fs::metadata(gcode_path).map(|meta| meta.len()).ok();
                let stats = analyze_file(gcode_path);
                let artifact = self
                    .store
                    .import_file(
                        &format!("{id}.gcode"),
                        ArtifactKind::Gcode,
                        "gcode",
                        gcode_path,
                        Some(format!("slice job {id}")),
                        json!({
                            "job_id": id,
                            "name": record.name,
                            "definition": record.definition,
                            "duration_ms": duration.as_millis() as u64,
                        }),
                    )
                    .map(|record| record.id);
                match artifact {
                    Ok(artifact) => {
                        record.state = JobState::Completed;
                        record.gcode_artifact = Some(artifact);
                        record.gcode_bytes = bytes;
                        record.gcode_stats = stats;
                    }
                    Err(err) => {
                        record.state = JobState::Failed;
                        record.error = Some(err.public_message());
                    }
                }
                record.logs = Some(JobLogs {
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                });
            }
            Err(err) => {
                record.state = if token.is_cancelled() {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                };
                record.error = Some(err.public_message());
            }
        }

        record.finished_at = Some(unix_now());
        record.duration_ms = Some(duration.as_millis() as u64);
        let _ = write_record(&job_dir, &record);
        self.prune().ok();
    }

    /// Load a job record.
    pub fn status(&self, id: &str) -> Result<JobRecord> {
        crate::workspace::validate_job_id(id)?;
        read_record(&self.store.job_dir(id))
    }

    /// Wait until a job finishes or `timeout` elapses.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Result<JobRecord> {
        let started = std::time::Instant::now();
        loop {
            let record = self.status(id)?;
            if record.state.is_terminal() || started.elapsed() >= timeout {
                return Ok(record);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Request cancellation of a queued or running job.
    pub fn cancel(&self, id: &str) -> Result<JobRecord> {
        let mut record = self.status(id)?;
        if record.state.is_terminal() {
            return Err(Error::Job(format!(
                "job '{id}' has already finished ({})",
                record.state.as_str()
            )));
        }
        if let Ok(tokens) = self.tokens.lock() {
            if let Some(token) = tokens.get(id) {
                token.cancel();
            }
        }
        record.state = JobState::Cancelled;
        record.finished_at = Some(unix_now());
        record.error = Some("cancelled by request".to_string());
        write_record(&self.store.job_dir(id), &record)?;
        Ok(record)
    }

    /// List jobs, newest first.
    pub fn list(&self, limit: usize) -> Result<Vec<JobRecord>> {
        let mut records = Vec::new();
        for entry in std::fs::read_dir(self.store.jobs_dir())? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Ok(record) = read_record(&entry.path()) {
                records.push(record);
            }
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        records.truncate(limit);
        Ok(records)
    }

    /// Remove old terminal jobs beyond `keep_jobs`, leaving the newest intact.
    fn prune(&self) -> Result<()> {
        if self.keep_jobs == 0 {
            return Ok(());
        }
        let mut jobs = self.list(usize::MAX)?;
        jobs.retain(|job| job.state.is_terminal());
        let keep = self.keep_jobs.max(1);
        for job in jobs.into_iter().skip(keep) {
            let dir = self.store.job_dir(&job.id);
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }
}

fn analyze_file(path: &Path) -> Option<GcodeStats> {
    const MAX_ANALYZE_BYTES: u64 = 96 * 1024 * 1024;
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_ANALYZE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(gcode::analyze(&text))
}

fn job_file(job_dir: &Path) -> std::path::PathBuf {
    job_dir.join("job.json")
}

fn write_record(job_dir: &Path, record: &JobRecord) -> Result<()> {
    std::fs::create_dir_all(job_dir)?;
    let text = serde_json::to_vec_pretty(record)?;
    std::fs::write(job_file(job_dir), text)?;
    Ok(())
}

fn read_record(job_dir: &Path) -> Result<JobRecord> {
    let text = std::fs::read_to_string(job_file(job_dir)).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::Job(format!(
                "job '{}' was not found",
                job_dir.file_name().unwrap_or_default().to_string_lossy()
            ))
        } else {
            Error::Io(err)
        }
    })?;
    serde_json::from_str(&text).map_err(Error::Json)
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
    use crate::{
        config::{EngineConfig, EngineMode},
        workspace::WorkspaceConfig,
    };

    fn manager() -> (tempfile::TempDir, JobManager) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            ArtifactStore::open(&WorkspaceConfig {
                root: dir.path().to_string_lossy().to_string(),
                ..WorkspaceConfig::default()
            })
            .unwrap(),
        );
        let engine = Arc::new(Engine::new(EngineConfig::default(), Vec::new()));
        (dir, JobManager::new(store, engine, 1, 10))
    }

    #[test]
    fn rejects_unknown_jobs() {
        let (_dir, manager) = manager();
        assert!(manager.status("job_does-not-exist").is_err());
        assert!(manager.status("../etc/passwd").is_err());
    }

    #[tokio::test]
    async fn cancellation_before_start_is_terminal() {
        let (_dir, manager) = manager();
        let plan = SlicePlan::new(EngineMode::ResolvedJson, Duration::from_secs(1));
        let record = manager
            .start(SliceJobRequest {
                name: "test".into(),
                plan,
                model_artifacts: Vec::new(),
                definition: None,
                profile_summary: Value::Null,
                request: Value::Null,
            })
            .unwrap();
        // The spawned task may already have picked the job up; cancellation is
        // valid either way while it is not terminal.
        if !manager.status(&record.id).unwrap().state.is_terminal() {
            let cancelled = manager.cancel(&record.id).unwrap();
            assert_eq!(cancelled.state, JobState::Cancelled);
        }
    }
}

//! Job manager: each job runs in-process on its own OS thread with a
//! dedicated multi-threaded tokio runtime, executing the CLI's command
//! orchestration directly (no subprocess). One running job at a time
//! (single-job mutex). Job state is shared with the executor thread via
//! [`JobShared`], which doubles as the [`JobProgress`] sink the CLI commands
//! emit events/logs and cooperative cancellation through.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use mev_scout_cli::JobProgress;
use serde::Serialize;

pub use mev_scout_cli::ProgressEvent;

use crate::exec;

/// Serializable job metadata returned by all `/api/jobs*` endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub job_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub status: JobStatus,
    pub exit_code: Option<i32>,
    /// Sniffed from the job's `Run ID: …` log line, as soon as it appears.
    pub run_id: Option<String>,
    /// Kept for HTTP/UI compatibility (was the spawned child's PID; in-process
    /// jobs have none).
    pub pid: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub log_path: String,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Finished,
    Failed,
    Killed,
}

/// Runtime state shared between the `JobManager` (reads) and the job's
/// executor thread (writes). Provides cooperative cancellation: commands
/// abort on the next progress tick once `cancel` is set.
#[derive(Debug)]
pub struct JobShared {
    cancel: AtomicBool,
    progress: Mutex<Option<ProgressEvent>>,
    log: Mutex<Vec<String>>,
    run_id: Mutex<Option<String>>,
    exit_code: Mutex<Option<i32>>,
    finished_at: Mutex<Option<DateTime<Utc>>>,
    exited: AtomicBool,
    killed: AtomicBool,
    log_file: Mutex<std::fs::File>,
}

impl JobShared {
    fn new(log_file: std::fs::File) -> Arc<JobShared> {
        Arc::new(JobShared {
            cancel: AtomicBool::new(false),
            progress: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            run_id: Mutex::new(None),
            exit_code: Mutex::new(None),
            finished_at: Mutex::new(None),
            exited: AtomicBool::new(false),
            killed: AtomicBool::new(false),
            log_file: Mutex::new(log_file),
        })
    }

    /// Append one line to the in-memory ring buffer + the durable log file.
    fn append_log(&self, line: &str) {
        {
            let mut f = match self.log_file.lock() {
                Ok(f) => f,
                Err(p) => p.into_inner(),
            };
            use std::io::Write;
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
        {
            const MAX_LOG_LINES: usize = 2000;
            let mut ring = self.log.lock().unwrap();
            if ring.len() > MAX_LOG_LINES {
                let excess = ring.len() - MAX_LOG_LINES;
                ring.drain(0..excess);
            }
            ring.push(line.to_string());
        }
        if let Some(idx) = line.find("Run ID: ") {
            let rest = line[idx + "Run ID: ".len()..].split_whitespace().next().unwrap_or("");
            if rest.starts_with("run_") || rest.starts_with("live_") {
                let mut rid = self.run_id.lock().unwrap();
                if rid.is_none() {
                    *rid = Some(rest.to_string());
                }
            }
        }
    }

    /// Done: persist exit outcomes so `snapshot` can resolve the status.
    pub(crate) fn finish(&self, exit_code: Option<i32>, killed: bool) {
        *self.exit_code.lock().unwrap() = exit_code;
        if killed {
            self.killed.store(true, Ordering::Relaxed);
        }
        {
            let mut fin = self.finished_at.lock().unwrap();
            if fin.is_none() {
                *fin = Some(Utc::now());
            }
        }
        self.exited.store(true, Ordering::Relaxed);
    }

    fn running(&self) -> bool {
        !self.exited.load(Ordering::Relaxed)
    }

    fn snapshot(&self, base: &JobInfo) -> JobInfo {
        let run_id = self.run_id.lock().unwrap().clone();
        let exit_code = *self.exit_code.lock().unwrap();
        let finished_at = *self.finished_at.lock().unwrap();
        let mut info = base.clone();
        info.run_id = run_id;
        info.exit_code = exit_code;
        info.finished_at = finished_at;
        info.status = if !self.running() {
            if self.killed.load(Ordering::Relaxed) {
                JobStatus::Killed
            } else if exit_code == Some(0) {
                JobStatus::Finished
            } else {
                JobStatus::Failed
            }
        } else {
            JobStatus::Running
        };
        info
    }
}

impl JobProgress for JobShared {
    fn emit(&self, evt: ProgressEvent) {
        *self.progress.lock().unwrap() = Some(evt.clone());
        // Keep the log endpoint parity with the old subprocess model (raw
        // NDJSON lines arrived in the child's stdout → log file).
        if let Ok(line) = serde_json::to_string(&evt) {
            self.append_log(&line);
        }
    }

    fn log(&self, line: &str) {
        self.append_log(line);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// One job: immutable creation-time info + the shared runtime.
#[derive(Clone)]
pub struct Job {
    pub info: JobInfo,
    shared: Arc<JobShared>,
}

/// Job registry. One running job at a time; history kept in memory only.
pub struct JobManager {
    jobs: Vec<Job>,
    log_dir: PathBuf,
}

impl JobManager {
    pub fn new(data_dir: &Path) -> Self {
        let log_dir = data_dir.join("logs");
        std::fs::create_dir_all(&log_dir).ok();
        JobManager {
            jobs: Vec::new(),
            log_dir,
        }
    }

    /// True when a job is currently running (drives 409s and chain-switch locks).
    pub async fn has_running(&self) -> bool {
        self.jobs.iter().any(|j| j.shared.running())
    }

    /// Spawn a new job. Caller must verify: no job running, command
    /// allowlisted, args sane. `config_path` is loaded by the job itself so
    /// UI edits apply to spawned runs (same semantics as the old
    /// `--config <path>` argv).
    pub async fn spawn(
        &mut self,
        config_path: &Path,
        command: String,
        args: Vec<String>,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<String> {
        if self.has_running().await {
            anyhow::bail!("a job is already running");
        }
        let now = chrono::Utc::now();
        let job_id = format!("job_{}", now.timestamp_millis());
        let log_path = self.log_dir.join(format!("{job_id}.log"));
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let shared = JobShared::new(log_file);

        // Multi-word allowlisted commands (e.g. `explorer index`) split into
        // separate argv tokens (the CLI clap parser expects that shape).
        let command_words: Vec<String> = command.split_whitespace().map(String::from).collect();

        let info = JobInfo {
            job_id: job_id.clone(),
            command: command.clone(),
            args: args.clone(),
            status: JobStatus::Running,
            exit_code: None,
            run_id: None,
            pid: None,
            created_at: now,
            finished_at: None,
            log_path: log_path.to_string_lossy().into_owned(),
            timeout_secs,
        };
        self.jobs.push(Job {
            info,
            shared: shared.clone(),
        });

        // Executor: one OS thread per job, each with its own multi-threaded
        // tokio runtime (mirrors the CLI's `#[tokio::main]` so
        // `block_in_place`-style replay code works identically in-process).
        let cfg = config_path.to_string_lossy().into_owned();
        let th_shared = shared.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("mev-scout-job")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    th_shared.log(&format!("failed to build job runtime: {e}"));
                    th_shared.finish(Some(1), false);
                    return;
                }
            };
            rt.block_on(exec::run_job(&cfg, &command_words, &args, &th_shared));
        });

        // Timeout watchdog: marks the job cancelled (cooperative) once the
        // deadline elapses, whatever the command is doing.
        if let Some(secs) = timeout_secs {
            let wd = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                wd.cancel.store(true, Ordering::Relaxed);
            });
        }

        Ok(job_id)
    }

    /// Stop a running job by requesting cooperative cancellation.
    pub async fn stop(&mut self, job_id: &str) -> anyhow::Result<()> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.info.job_id == job_id)
            .ok_or_else(|| anyhow::anyhow!("unknown job '{job_id}'"))?;
        if !job.shared.running() {
            anyhow::bail!("job '{job_id}' is not running");
        }
        job.shared.cancel.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Request cancellation for every running job (graceful shutdown).
    pub async fn kill_all(&mut self) {
        for job in &self.jobs {
            if job.shared.running() {
                job.shared.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    fn get(&self, job_id: &str) -> anyhow::Result<&Job> {
        self.jobs
            .iter()
            .find(|j| j.info.job_id == job_id)
            .ok_or_else(|| anyhow::anyhow!("unknown job '{job_id}'"))
    }

    /// Snapshot of one job's info (merges runtime updates).
    pub async fn job_info(&self, job_id: &str) -> anyhow::Result<JobInfo> {
        let job = self.get(job_id)?;
        Ok(job.shared.snapshot(&job.info))
    }

    /// Snapshot list of all jobs (running + finished), newest last.
    pub async fn list(&self) -> Vec<JobInfo> {
        self.jobs
            .iter()
            .map(|j| j.shared.snapshot(&j.info))
            .collect()
    }

    /// Latest emitted progress event for a job (None until its first event).
    pub async fn progress(&self, job_id: &str) -> anyhow::Result<Option<ProgressEvent>> {
        let job = self.get(job_id)?;
        Ok(job.shared.progress.lock().unwrap().clone())
    }

    /// Tail of a job's log (whole in-memory ring when `tail` is None).
    pub async fn log_tail(
        &self,
        job_id: &str,
        tail: Option<usize>,
    ) -> anyhow::Result<Vec<String>> {
        let job = self.get(job_id)?;
        let ring = job.shared.log.lock().unwrap();
        match tail {
            Some(n) => Ok(ring.iter().rev().take(n).rev().cloned().collect()),
            None => Ok(ring.clone()),
        }
    }
}
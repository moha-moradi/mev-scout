//! Job manager: spawn/kill/list CLI subprocess jobs, capture logs, parse
//! NDJSON progress events. One running job at a time (single-job mutex).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Serializable job metadata returned by all `/api/jobs*` endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub job_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub status: JobStatus,
    pub exit_code: Option<i32>,
    /// Parsed from the child's `Run ID: …` stdout line, as soon as it appears.
    pub run_id: Option<String>,
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

/// One NDJSON `--progress json` event parsed from the job log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ops: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl ProgressEvent {
    pub fn pct(&self) -> Option<f64> {
        match (self.done, self.total) {
            (Some(d), Some(t)) if t > 0 => Some(d as f64 / t as f64),
            _ => None,
        }
    }
}

/// Mutable runtime shared between the JobManager and the background
/// monitor tasks (which must never hold the manager mutex across awaits).
#[derive(Debug, Default)]
struct JobRuntime {
    pid: Option<u32>,
    run_id: Option<String>,
    progress: Option<ProgressEvent>,
    stop_requested: bool,
    exited: bool,
    exit_code: Option<i32>,
    finished_at: Option<DateTime<Utc>>,
}

/// One job: serializable info + the shared runtime the monitor updates.
#[derive(Clone)]
pub struct Job {
    pub info: JobInfo,
    runtime: Arc<Mutex<JobRuntime>>,
}

impl Job {
    /// True while the child process has not yet exited. Reads the runtime
    /// (updated by the exit watcher) rather than the creation-time
    /// `info.status`, so a finished job stops blocking new spawns.
    async fn running(&self) -> bool {
        let rt = self.runtime.lock().await;
        !rt.exited
    }
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
        for job in &self.jobs {
            if job.running().await {
                return true;
            }
        }
        false
    }

    /// Spawn a new job. Caller must verify: no job running, command
    /// allowlisted, args sane. `config_path` is passed to the child via
    /// `--config` so UI edits apply to spawned runs.
    pub async fn spawn(
        &mut self,
        binary: &Path,
        config_path: &Path,
        command: String,
        mut args: Vec<String>,
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
        std::fs::write(&log_path, "")?;

        // Child argv: binary --config <path> <command words...> <args...>
        // Multi-word allowlisted commands (e.g. `explorer index`) split
        // into separate argv tokens; clap rejects the single-token form.
        let command_words: Vec<String> = command.split_whitespace().map(String::from).collect();
        let mut full_args: Vec<String> = vec![
            "--config".to_string(),
            config_path.to_string_lossy().into_owned(),
        ];
        full_args.extend(command_words.iter().cloned());
        full_args.append(&mut args);

        let mut child = tokio::process::Command::new(binary)
            .args(&full_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let runtime = Arc::new(Mutex::new(JobRuntime {
            pid,
            ..Default::default()
        }));
        let info = JobInfo {
            job_id: job_id.clone(),
            command,
            args: full_args[2 + command_words.len()..].to_vec(),
            status: JobStatus::Running,
            exit_code: None,
            run_id: None,
            pid,
            created_at: now,
            finished_at: None,
            log_path: log_path.to_string_lossy().into_owned(),
            timeout_secs,
        };
        let job = Job {
            info,
            runtime: runtime.clone(),
        };
        self.jobs.push(job);

        // stdout/stderr line collectors: append to the log file, parse
        // `Run ID:` lines and NDJSON progress events along the way.
        {
            let rt = runtime.clone();
            let path = log_path.clone();
            tokio::spawn(async move {
                collect_stream(stdout, &path, rt).await;
            });
        }
        {
            let rt = runtime.clone();
            let path = log_path.clone();
            tokio::spawn(async move {
                collect_stream(stderr, &path, rt).await;
            });
        }

        // Exit watcher: resolves status (finished/failed/killed) + timeout.
        let rt = runtime.clone();
        let jobs_len = self.jobs.len();
        let _ = jobs_len;
        tokio::spawn(async move {
            let status = tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(timeout_secs.unwrap_or(u64::MAX / 4))) => {
                    if timeout_secs.is_some() {
                        if let Some(pid) = rt.lock().await.pid {
                            kill_process_tree(pid).await;
                        }
                        let mut g = rt.lock().await;
                        g.stop_requested = true;
                        JobStatus::Killed
                    } else {
                        // u64::MAX sleep — unreachable; treat as wait.
                        wait_child(&mut child, &rt).await
                    }
                }
                code = child.wait() => {
                    let code = code.ok().and_then(|c| c.code());
                    let mut g = rt.lock().await;
                    g.exited = true;
                    g.exit_code = code;
                    let killed = g.stop_requested;
                    g.finished_at = Some(chrono::Utc::now());
                    drop(g);
                    if killed {
                        JobStatus::Killed
                    } else if code == Some(0) {
                        JobStatus::Finished
                    } else {
                        JobStatus::Failed
                    }
                }
            };
            let mut g = rt.lock().await;
            g.exited = true;
            g.finished_at.get_or_insert_with(chrono::Utc::now);
            drop(g);
            let _ = status;
        });
        Ok(job_id)
    }

    /// Stop a running job (kill the whole process tree).
    pub async fn stop(&mut self, job_id: &str) -> anyhow::Result<()> {
        let job = self
            .jobs
            .iter()
            .find(|j| j.info.job_id == job_id)
            .ok_or_else(|| anyhow::anyhow!("unknown job '{job_id}'"))?;
        if !job.running().await {
            anyhow::bail!("job '{job_id}' is not running");
        }
        let pid = job.runtime.lock().await.pid;
        job.runtime.lock().await.stop_requested = true;
        if let Some(pid) = pid {
            kill_process_tree(pid).await;
        }
        Ok(())
    }

    /// Kill every running job (graceful shutdown).
    pub async fn kill_all(&mut self) {
        for job in &self.jobs {
            if job.running().await {
                let mut g = job.runtime.lock().await;
                g.stop_requested = true;
                if let Some(pid) = g.pid {
                    kill_process_tree(pid).await;
                }
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
        Ok(self.snapshot(job).await)
    }

    async fn snapshot(&self, job: &Job) -> JobInfo {
        let rt = job.runtime.lock().await;
        let mut info = job.info.clone();
        info.run_id = rt.run_id.clone();
        info.exit_code = rt.exit_code;
        info.finished_at = rt.finished_at;
        info.pid = rt.pid;
        if rt.exited {
            let killed = rt.stop_requested;
            info.status = if killed {
                JobStatus::Killed
            } else if rt.exit_code == Some(0) {
                JobStatus::Finished
            } else {
                JobStatus::Failed
            };
        }
        info
    }

    /// Snapshot list of all jobs (running + finished), newest last.
    pub async fn list(&self) -> Vec<JobInfo> {
        let mut out = Vec::new();
        for job in &self.jobs {
            out.push(self.snapshot(job).await);
        }
        out
    }

    /// Latest parsed progress event for a job (None without `--progress json`).
    pub async fn progress(&self, job_id: &str) -> anyhow::Result<Option<ProgressEvent>> {
        let job = self.get(job_id)?;
        Ok(job.runtime.lock().await.progress.clone())
    }

    /// Tail of a job's log file (whole file when `tail` is None).
    pub async fn log_tail(
        &self,
        job_id: &str,
        tail: Option<usize>,
    ) -> anyhow::Result<Vec<String>> {
        let job = self.get(job_id)?;
        let content = tokio::fs::read_to_string(&job.info.log_path).await?;
        let lines: Vec<String> = content.lines().map(str::to_string).collect();
        match tail {
            Some(n) => Ok(lines.into_iter().rev().take(n).rev().collect()),
            None => Ok(lines),
        }
    }
}

async fn wait_child(child: &mut tokio::process::Child, rt: &Arc<Mutex<JobRuntime>>) -> JobStatus {
    let code = child.wait().await.ok().and_then(|c| c.code());
    let mut g = rt.lock().await;
    g.exited = true;
    g.exit_code = code;
    g.finished_at = Some(chrono::Utc::now());
    let killed = g.stop_requested;
    drop(g);
    if killed {
        JobStatus::Killed
    } else if code == Some(0) {
        JobStatus::Finished
    } else {
        JobStatus::Failed
    }
}

/// Read a child stream line by line: append to the job log, extract the
/// `Run ID:` link and NDJSON progress events.
async fn collect_stream<S>(stream: S, log_path: &PathBuf, runtime: Arc<Mutex<JobRuntime>>)
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .await;
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if let Ok(f) = file.as_mut() {
                    use tokio::io::AsyncWriteExt;
                    let _ = f.write_all(line.as_bytes()).await;
                }
                // Run-ID link: parse as soon as the line appears so the
                // live monitor can start streaming opportunities.
                if let Some(idx) = line.find("Run ID: ") {
                    let rest = line[idx + "Run ID: ".len()..].trim();
                    let candidate = rest.split_whitespace().next().unwrap_or("");
                    if candidate.starts_with("run_") || candidate.starts_with("live_") {
                        let mut g = runtime.lock().await;
                        if g.run_id.is_none() {
                            g.run_id = Some(candidate.to_string());
                        }
                    }
                }
                // NDJSON progress events (`--progress json`).
                if line.starts_with('{') {
                    if let Ok(evt) = serde_json::from_str::<ProgressEvent>(line.trim()) {
                        if evt.stage.is_empty() {
                            continue;
                        }
                        let mut g = runtime.lock().await;
                        g.progress = Some(evt);
                    }
                }
            }
            Err(_) => break,
        }
    }
}

/// Kill a process and all its descendants. `Child::kill()` on Windows only
/// kills the direct child, so use `taskkill /T /F` there.
#[cfg(target_os = "windows")]
pub async fn kill_process_tree(pid: u32) {
    let _ = tokio::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output()
        .await;
}

#[cfg(not(target_os = "windows"))]
pub async fn kill_process_tree(pid: u32) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    );
}

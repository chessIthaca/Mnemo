// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `job_start` / `job_wait` / `job_tail` / `job_kill` — long commands without
//! polling (plan 5b85ab61 step 2).
//!
//! A worker used to detach a long build into a background process and poll it
//! with `sleep` and log reads: one model round trip per poll, and the sleep
//! always either too short or too long. The four job tools replace that loop:
//!
//! - `job_start` runs a command as a session-owned JOB and returns its id at
//!   once. A job has NO silence limit and a three-hour hard cap, so a cold
//!   `cargo test --workspace` can run to completion in the background.
//! - `job_wait` blocks SERVER-SIDE until the job exits, until a NEW output line
//!   matches a pattern, or until its own timeout — one call, no polling.
//! - `job_tail` shows the last lines of the run so far.
//! - `job_kill` kills the job's whole process tree.
//!
//! # Ownership
//!
//! The tree-ownership rule (plan bfe2b09f) is unchanged: nothing a call starts
//! outlives what owns it. A job is owned by this session's [`JobRegistry`]
//! instead of by a single call. Dropping the registry — the session's agent
//! loop is dropped on stop, merge or app exit — aborts every job, and each
//! aborted job drops the `TreeKillGuard` that owns its process tree. A job is
//! never a way to leave a dev server running after the session.
//!
//! # The log
//!
//! stdout and stderr go to ONE file,
//! `<temp>/mnemo-jobs/<registry uuid>/<job id>.log`, never inside the worktree.
//! Every record is one line, tagged with the stream it came from:
//!
//! ```text
//! [out] Compiling mnemo v0.2.0
//! [err] error[E0308]: mismatched types
//! ```
//!
//! The tag is how a query rebuilds the two streams for the command-aware
//! [`filter_shell_output`] and how `job_tail` shows untagged raw lines.
//!
//! # Never a recorded check
//!
//! A job run is never recorded as a check: it is not measured against the
//! worktree and does not go through the recorded-check path. `job_start` says
//! so when the command is one of the session's check commands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use super::sandbox::Sandbox;
use super::shell::{
    command_is_core_git_op, combine_streams, resolve_cwd, run_command, tail_of, validated_check_command,
    JobLogTap, RunLimits, RunOutcome,
};
use super::shell_filter::filter_shell_output;
use crate::config::ShellFilterConfig;
use crate::provider::ToolSchema;
use crate::tool::agent::read_files::invalid_args_error;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolOutputStream, ToolResult};

/// The hard cap for a job (plan 5b85ab61): three hours covers a cold
/// full-workspace build, its test suite and a release run with room to spare.
///
/// There is deliberately NO silence limit: a job is watched with `job_wait`, so
/// a long quiet stretch (linking, a single slow test) is not a reason to kill
/// it — that limit belongs to a call that must come back.
const JOB_TOTAL: Duration = Duration::from_secs(3 * 60 * 60);

/// At most this many jobs run at once per session. Past it the model waits for
/// or kills one instead of piling up builds that compete for the same cores.
const MAX_RUNNING_JOBS: usize = 4;

/// `job_wait`'s default blocking time, in seconds: long enough for a cold build
/// to finish.
const DEFAULT_WAIT_S: u64 = 600;

/// `job_wait`'s maximum blocking time, in seconds: one call never occupies the
/// session for more than half an hour without a decision.
const MAX_WAIT_S: u64 = 1800;

/// `job_tail`'s default line count.
const DEFAULT_TAIL_LINES: usize = 60;

/// `job_tail`'s maximum line count.
const MAX_TAIL_LINES: usize = 400;

/// Display budget for a job's filtered tail. The END is kept — how far the run
/// got is what the model needs.
const TAIL_BYTES: usize = 8 * 1024;

/// Above this the log is read from its last bytes only, so one query cannot
/// pull a multi-hundred-megabyte log into memory.
const LOG_READ_TAIL: u64 = 8 * 1024 * 1024;

/// Raw-line read budget for `job_tail`.
const RAW_READ_TAIL: u64 = 512 * 1024;

/// The note that prefixes a log read cut at [`LOG_READ_TAIL`].
const LOG_EARLIER_OMITTED: &str = "[... earlier log omitted ...]\n";

/// The note that prefixes a tail cut at [`TAIL_BYTES`].
const TAIL_EARLIER_OMITTED: &str = "[... earlier output omitted ...]\n";

/// Stream tags in the job log (see the module docs).
const STDOUT_TAG: &str = "[out] ";
const STDERR_TAG: &str = "[err] ";

/// The note `job_start` appends when the command is one of the session's check
/// commands: such a run cannot certify the tip, so the model is pointed at the
/// path that does record one.
const CHECK_NOT_RECORDED_NOTE: &str =
    "this run is not recorded as a check; run it with shell from the checkout root to record it";

/// How long `job_kill` waits for the log writer to close the file after an
/// abort, so the reported tail includes what the readers had captured.
const LOG_CLOSE_GRACE: Duration = Duration::from_secs(1);

/// Poll cadence while `job_wait` watches for a NEW matching line. 100 ms is
/// invisible to the model and cheap server-side — the cursor reads only the
/// bytes appended since the previous pass.
const PATTERN_POLL: Duration = Duration::from_millis(100);

/// Poll cadence while `job_wait` watches a job with NO pattern: a finished job
/// is signalled, so this is only the lost-wakeup backstop.
const STATUS_POLL: Duration = Duration::from_secs(1);

/// How a job ended — or that it is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JobStatus {
    Running,
    Exited(i32),
    /// Stopped on purpose; the reason names the cause.
    Killed(&'static str),
    /// The command never started.
    SpawnFailed(String),
}

impl JobStatus {
    /// The model-facing state string.
    fn text(&self) -> String {
        match self {
            Self::Running => "still running".to_string(),
            Self::Exited(code) => format!("exited {code}"),
            Self::Killed(reason) => format!("killed ({reason})"),
            Self::SpawnFailed(message) => format!("failed to start ({message})"),
        }
    }
}

/// One job's shared state: the registry, the run task and the tools all hold a
/// clone of the same `Arc`.
///
/// The run task is the only writer (`finish`, `log_writer_finished`); the tools
/// only read it, so a query never blocks a run for longer than a lock scope.
struct JobState {
    id: String,
    /// The command as the model wrote it — the filter classifies on it, so it
    /// must be the ORIGINAL string (not the PowerShell translation).
    command: String,
    started: Instant,
    log_path: PathBuf,
    progress: Mutex<JobProgress>,
    /// Notified when the run ends, so `job_wait` needs no polling to see it.
    done: tokio::sync::Notify,
    /// Set by the log writer once the file is closed.
    log_closed: AtomicBool,
    log_closed_notify: tokio::sync::Notify,
}

/// The mutable half of [`JobState`].
struct JobProgress {
    status: JobStatus,
    /// The run time when the job ended; `None` while it runs.
    elapsed: Option<Duration>,
}

impl JobState {
    /// A job that has not started producing anything yet.
    fn new(id: String, command: String, log_path: PathBuf) -> Self {
        Self {
            id,
            command,
            started: Instant::now(),
            log_path,
            progress: Mutex::new(JobProgress {
                status: JobStatus::Running,
                elapsed: None,
            }),
            done: tokio::sync::Notify::new(),
            log_closed: AtomicBool::new(false),
            log_closed_notify: tokio::sync::Notify::new(),
        }
    }

    fn status(&self) -> JobStatus {
        self.progress
            .lock()
            .expect("job progress lock poisoned")
            .status
            .clone()
    }

    fn state_text(&self) -> String {
        self.status().text()
    }

    fn is_finished(&self) -> bool {
        !matches!(self.status(), JobStatus::Running)
    }

    /// The run time: the final duration once ended, the time so far while it
    /// runs.
    fn elapsed(&self) -> Duration {
        self.progress
            .lock()
            .expect("job progress lock poisoned")
            .elapsed
            .unwrap_or_else(|| self.started.elapsed())
    }

    /// Record the end of the run (first writer wins, so an interrupted task and
    /// the kill path cannot fight over the state).
    fn finish(&self, status: JobStatus) {
        {
            let mut progress = self.progress.lock().expect("job progress lock poisoned");
            if !matches!(progress.status, JobStatus::Running) {
                return;
            }
            progress.elapsed = Some(self.started.elapsed());
            progress.status = status;
        }
        self.done.notify_waiters();
    }

    /// The log writer closed the file (or could not open it).
    fn log_writer_finished(&self) {
        self.log_closed.store(true, Ordering::Relaxed);
        self.log_closed_notify.notify_waiters();
    }

    /// Wait (bounded) until the log writer closed the file.
    async fn wait_log_closed(&self, limit: Duration) {
        let deadline = tokio::time::Instant::now() + limit;
        while !self.log_closed.load(Ordering::Relaxed) {
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            // A lost wakeup is covered by the loop's next check and the
            // deadline; the short sleep bounds the retry rate.
            let _ = tokio::time::timeout(Duration::from_millis(10), self.log_closed_notify.notified())
                .await;
        }
    }
}

/// One job's registry entry.
struct JobEntry {
    /// The task awaiting [`run_command`]. Aborting it drops the run future, and
    /// with it the tree guard that owns the process tree.
    task: Option<tokio::task::JoinHandle<()>>,
    state: Arc<JobState>,
}

/// The session's job registry — ONE per agent session's tool set.
///
/// Every job's process tree is owned by this registry: dropping it aborts every
/// running job (which drops the run futures, and with them the tree guards) and
/// removes the session's log folder.
pub struct JobRegistry {
    sandbox: Sandbox,
    /// Shared with the shell tool: a job's tail must be filtered exactly like a
    /// shell result, and a Settings save must reach both.
    filter_config: Arc<RwLock<ShellFilterConfig>>,
    inner: Mutex<RegistryState>,
}

/// The mutable half of [`JobRegistry`].
struct RegistryState {
    /// Job ids are `j1`, `j2`, ... per session.
    next_id: u64,
    /// Created on the first `job_start`, so a session that never starts a job
    /// leaves no folder behind.
    dir: Option<PathBuf>,
    jobs: BTreeMap<String, JobEntry>,
}

impl JobRegistry {
    /// A registry confined to `sandbox`, sharing `filter_config` with the shell
    /// tool.
    pub fn new(sandbox: Sandbox, filter_config: Arc<RwLock<ShellFilterConfig>>) -> Self {
        Self {
            sandbox,
            filter_config,
            inner: Mutex::new(RegistryState {
                next_id: 0,
                dir: None,
                jobs: BTreeMap::new(),
            }),
        }
    }

    /// Resolve and sandbox-validate the working directory — the same contract
    /// as `shell`'s (see [`resolve_cwd`]).
    fn cwd(&self, cwd: Option<&str>) -> Result<PathBuf, String> {
        resolve_cwd(&self.sandbox, cwd)
    }

    /// Start a job for `command` in `cwd` and return its state. The command
    /// begins running immediately; the caller never waits for it.
    async fn start(&self, command: &str, cwd: PathBuf) -> Result<Arc<JobState>, String> {
        let (id, log_path) = {
            let mut inner = self.inner.lock().expect("job registry lock poisoned");
            let running: Vec<String> = inner
                .jobs
                .iter()
                .filter(|(_, entry)| !entry.state.is_finished())
                .map(|(id, _)| id.clone())
                .collect();
            if running.len() >= MAX_RUNNING_JOBS {
                return Err(format!(
                    "job limit reached: {MAX_RUNNING_JOBS} jobs are already running ({}). Wait \
                     for one with job_wait, or stop one with job_kill.",
                    running.join(", ")
                ));
            }
            if inner.dir.is_none() {
                let dir = std::env::temp_dir()
                    .join("mnemo-jobs")
                    .join(uuid::Uuid::new_v4().to_string());
                std::fs::create_dir_all(&dir).map_err(|error| {
                    format!(
                        "could not create the job log folder {}: {error}",
                        dir.display()
                    )
                })?;
                inner.dir = Some(dir);
            }
            inner.next_id += 1;
            let id = format!("j{}", inner.next_id);
            let log_path = inner
                .dir
                .as_ref()
                .expect("job log folder set above")
                .join(format!("{id}.log"));
            (id, log_path)
        };

        let state = Arc::new(JobState::new(id.clone(), command.to_string(), log_path.clone()));
        // ONE writer owns the log file: records are appended in arrival order,
        // and the file closes when every sender is gone.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = tokio::spawn(write_job_log(rx, log_path, state.clone()));
        let tap = JobLogTap::new(tx.clone());
        let task = tokio::spawn(run_job(
            state.clone(),
            command.to_string(),
            cwd,
            tx,
            writer,
            tap,
        ));

        let mut inner = self.inner.lock().expect("job registry lock poisoned");
        inner.jobs.insert(
            id,
            JobEntry {
                task: Some(task),
                state: state.clone(),
            },
        );
        Ok(state)
    }

    /// The state of one job, or a model-facing error naming the session's ids.
    fn lookup(&self, id: &str) -> Result<Arc<JobState>, String> {
        let inner = self.inner.lock().expect("job registry lock poisoned");
        match inner.jobs.get(id) {
            Some(entry) => Ok(entry.state.clone()),
            None => Err(unknown_job(id, &inner.jobs)),
        }
    }

    /// Kill one job's whole tree and wait for it to be gone.
    async fn kill(&self, id: &str) -> Result<Arc<JobState>, String> {
        let found = {
            let mut inner = self.inner.lock().expect("job registry lock poisoned");
            inner
                .jobs
                .get_mut(id)
                .map(|entry| (entry.task.take(), entry.state.clone()))
        };
        let Some((task, state)) = found else {
            let inner = self.inner.lock().expect("job registry lock poisoned");
            return Err(unknown_job(id, &inner.jobs));
        };
        if let Some(task) = task {
            // The abort drops the run future — and with it the tree guard, so
            // every process the command started dies here.
            task.abort();
            let _ = task.await;
        }
        state.finish(JobStatus::Killed("job_kill"));
        state.wait_log_closed(LOG_CLOSE_GRACE).await;
        Ok(state)
    }

    /// The model-facing tail of a job: the command-aware filter over the log,
    /// keeping the END inside [`TAIL_BYTES`].
    fn filtered_tail(&self, state: &JobState) -> String {
        let (log, omitted) = read_log_tail(&state.log_path, LOG_READ_TAIL);
        let (stdout, stderr) = partition_log(&log);
        let combined = {
            let config = self
                .filter_config
                .read()
                .expect("shell filter config lock poisoned");
            let (filtered_stdout, filtered_stderr) =
                filter_shell_output(&state.command, &stdout, &stderr, &config);
            combine_streams(&filtered_stdout, &filtered_stderr)
        };
        let mut text = String::new();
        if omitted {
            text.push_str(LOG_EARLIER_OMITTED);
        }
        if combined.len() > TAIL_BYTES {
            text.push_str(TAIL_EARLIER_OMITTED);
            text.push_str(tail_of(&combined, TAIL_BYTES));
        } else {
            text.push_str(&combined);
        }
        text
    }

    /// The last `lines` RAW (untagged, unfiltered) lines of a job's log, plus
    /// whether earlier lines were skipped.
    fn raw_tail(state: &JobState, lines: usize) -> (String, bool) {
        let (log, omitted) = read_log_tail(&state.log_path, RAW_READ_TAIL);
        let records: Vec<&str> = log
            .split_inclusive('\n')
            .map(|line| {
                line.strip_prefix(STDERR_TAG)
                    .or_else(|| line.strip_prefix(STDOUT_TAG))
                    .unwrap_or(line)
                    .trim_end_matches('\n')
            })
            .collect();
        let start = records.len().saturating_sub(lines);
        (records[start..].join("\n"), omitted || start > 0)
    }
}

/// The model-facing error for an id this session never handed out.
fn unknown_job(id: &str, jobs: &BTreeMap<String, JobEntry>) -> String {
    if jobs.is_empty() {
        return format!("unknown job id `{id}`: this session has no jobs");
    }
    let ids: Vec<&str> = jobs.keys().map(String::as_str).collect();
    format!(
        "unknown job id `{id}`: this session's jobs are {}",
        ids.join(", ")
    )
}

impl Drop for JobRegistry {
    fn drop(&mut self) {
        // Never panic in a drop: a poisoned lock means the registry is already
        // unwinding.
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        for entry in inner.jobs.values_mut() {
            if let Some(task) = entry.task.take() {
                // The abort kills the job's tree when the run future is
                // dropped. Fire-and-forget: a drop cannot await.
                task.abort();
            }
        }
        let Some(dir) = inner.dir.take() else {
            return;
        };
        drop(inner);
        // The aborts are asynchronous: the run futures, the readers and the log
        // file close a moment later, and Windows refuses to delete an open
        // file. Remove the folder from a short-lived thread that retries.
        std::thread::spawn(move || {
            for _ in 0..40 {
                if std::fs::remove_dir_all(&dir).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
}

/// Run one job to completion and record how it ended.
///
/// The run goes through the SAME runner `shell` uses, with `silence: None` and
/// the [`JOB_TOTAL`] cap — the deadline loop is not duplicated here.
async fn run_job(
    state: Arc<JobState>,
    command: String,
    cwd: PathBuf,
    tx: tokio::sync::mpsc::UnboundedSender<(ToolOutputStream, String)>,
    writer: tokio::task::JoinHandle<()>,
    tap: JobLogTap,
) {
    let limits = RunLimits {
        silence: None,
        total: JOB_TOTAL,
    };
    let outcome = run_command(&command, &cwd, limits, None, Some(tap)).await;
    // No more senders from this task: the writer drains what the readers sent
    // and closes the file, so a query never races the last lines.
    drop(tx);
    let _ = writer.await;
    let status = match outcome {
        RunOutcome::Exited { code, .. } => JobStatus::Exited(code),
        // A job has no silence limit, so this cannot fire; mapping it keeps the
        // state honest if that ever changes.
        RunOutcome::SilenceTimeout { .. } => JobStatus::Killed("silence timeout"),
        RunOutcome::TotalTimeout { .. } => JobStatus::Killed("hard cap"),
        RunOutcome::SpawnFailed(message) => JobStatus::SpawnFailed(message),
    };
    state.finish(status);
}

/// Append every line the run's readers hand over to the job log.
///
/// One record per line: `<tag><line>\n`. A trailing partial line (the run ended
/// mid-line) gets the newline added so the file stays line-oriented, and a
/// Windows CRLF is trimmed for the same reason.
async fn write_job_log(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<(ToolOutputStream, String)>,
    path: PathBuf,
    state: Arc<JobState>,
) {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(&path).await.ok();
    while let Some((stream, line)) = rx.recv().await {
        let Some(handle) = file.as_mut() else {
            break;
        };
        let tag = match stream {
            ToolOutputStream::Stdout => STDOUT_TAG,
            ToolOutputStream::Stderr => STDERR_TAG,
        };
        let body = line.trim_end_matches(['\n', '\r']);
        let record = format!("{tag}{body}\n");
        if handle.write_all(record.as_bytes()).await.is_err() {
            break;
        }
    }
    if let Some(mut handle) = file {
        let _ = handle.flush().await;
    }
    state.log_writer_finished();
}

/// Read a log's end: the whole file when it fits in `cap`, otherwise its last
/// `cap` bytes starting at a line boundary. Returns the text and whether
/// earlier lines were left out.
fn read_log_tail(path: &Path, cap: u64) -> (String, bool) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(metadata) = std::fs::metadata(path) else {
        return (String::new(), false);
    };
    let len = metadata.len();
    let Ok(mut file) = std::fs::File::open(path) else {
        return (String::new(), false);
    };
    let skipped = len > cap;
    if skipped && file.seek(SeekFrom::Start(len - cap)).is_err() {
        return (String::new(), false);
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return (String::new(), false);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if skipped {
        // The read began mid-record: drop that partial line.
        let start = text.find('\n').map_or(0, |index| index + 1);
        return (text[start..].to_string(), true);
    }
    (text, false)
}

/// Split a job log back into its two streams, dropping the stream tags.
fn partition_log(log: &str) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    for line in log.split_inclusive('\n') {
        if let Some(body) = line.strip_prefix(STDERR_TAG) {
            stderr.push_str(body);
        } else if let Some(body) = line.strip_prefix(STDOUT_TAG) {
            stdout.push_str(body);
        } else {
            // A record from a read that began mid-line: no tag to honor.
            stdout.push_str(line);
        }
    }
    (stdout, stderr)
}

/// Human-readable run time for a job report.
fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs >= 60.0 {
        format!("{}m{:04.1}s", (secs / 60.0) as u64, secs % 60.0)
    } else if secs >= 1.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

/// The shared shape of every job report: the state line, the run time, then a
/// headed tail. `state` is the caller's label, so `job_wait` can report
/// `matched: <line>` instead of the job's running state.
fn job_report(state: &JobState, label: &str, heading: &str, tail: &str) -> String {
    let mut text = format!(
        "state: {label}\nelapsed: {}\n",
        format_elapsed(state.elapsed())
    );
    if tail.trim().is_empty() {
        text.push_str("(no output)\n");
    } else {
        text.push_str(heading);
        text.push('\n');
        text.push_str(tail);
        if !tail.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

/// A forward-only reader over a job log, for `job_wait`'s pattern watch.
///
/// It is created at the log's CURRENT end, so only lines printed after the call
/// began can match — that is the whole point of "a NEW output line". Reads are
/// incremental: each pass reads only the bytes appended since the last one.
struct LogCursor {
    path: PathBuf,
    pos: u64,
    partial: Vec<u8>,
}

impl LogCursor {
    fn new(path: &Path) -> Self {
        let pos = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Self {
            path: path.to_path_buf(),
            pos,
            partial: Vec::new(),
        }
    }

    /// The first newly completed line matching `pattern`, if any.
    fn matching_line(&mut self, pattern: &Regex) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(&self.path).ok()?;
        file.seek(SeekFrom::Start(self.pos)).ok()?;
        let mut bytes = Vec::new();
        let read = file.read_to_end(&mut bytes).ok()?;
        if read == 0 {
            return None;
        }
        self.pos += read as u64;
        self.partial.extend_from_slice(&bytes);
        let mut matched = None;
        while let Some(index) = self.partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=index).collect();
            let text = String::from_utf8_lossy(&line);
            let body = text.trim_end_matches(['\n', '\r']);
            let body = body
                .strip_prefix(STDERR_TAG)
                .or_else(|| body.strip_prefix(STDOUT_TAG))
                .unwrap_or(body);
            if matched.is_none() && pattern.is_match(body) {
                matched = Some(body.to_string());
            }
        }
        matched
    }
}

/// How a `job_wait` ended.
enum WaitOutcome {
    Finished,
    Matched(String),
    TimedOut,
}

/// Watch a job until it ends, a NEW line matches `pattern`, or `timeout`
/// passes — whichever comes first.
async fn wait_for_job(state: &JobState, pattern: Option<&Regex>, timeout: Duration) -> WaitOutcome {
    let deadline = tokio::time::Instant::now() + timeout;
    let Some(pattern) = pattern else {
        loop {
            if state.is_finished() {
                return WaitOutcome::Finished;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return WaitOutcome::TimedOut;
            }
            let slice = (deadline - now).min(STATUS_POLL);
            let _ = tokio::time::timeout(slice, state.done.notified()).await;
        }
    };
    let mut cursor = LogCursor::new(&state.log_path);
    loop {
        // The match check leads the finish check: a line printed just before
        // the run ended is exactly what the caller is waiting for.
        if let Some(line) = cursor.matching_line(pattern) {
            return WaitOutcome::Matched(line);
        }
        if state.is_finished() {
            return WaitOutcome::Finished;
        }
        if tokio::time::Instant::now() >= deadline {
            return WaitOutcome::TimedOut;
        }
        tokio::time::sleep(PATTERN_POLL).await;
    }
}

/// Arguments for `job_start`.
#[derive(Debug, Deserialize)]
struct JobStartArgs {
    command: String,
    /// Optional working directory (relative to project root, sandboxed).
    #[serde(default)]
    cwd: Option<String>,
    /// A short human-readable label of what this job does (e.g. "building the
    /// workspace"). Required like `shell`'s: it was the first field dropped when
    /// a call went malformed.
    ///
    /// Consumed by the frontend from the raw tool-call args (Mnemo2's
    /// `argLabel` helper), so it is kept only so serde parses/validates it.
    #[expect(dead_code, reason = "consumed by the frontend from raw tool-call args")]
    purpose: String,
}

/// `job_start` — begin a background job and return its id at once.
pub struct JobStartTool {
    jobs: Arc<JobRegistry>,
    /// The same shared core-operation list `ShellTool` holds, so a job-invoked
    /// `git push` raises the always-on prompt too.
    core_operations: Arc<RwLock<Vec<String>>>,
    /// The session's exact check commands (for the "not a check" note).
    check_commands: Vec<String>,
}

impl JobStartTool {
    /// A starter bound to the session's registry and core-operation list.
    pub fn new(jobs: Arc<JobRegistry>, core_operations: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            jobs,
            core_operations,
            check_commands: Vec::new(),
        }
    }

    /// The session's check commands, validated for exact membership — the same
    /// list (and the same validation) `shell` measures.
    pub fn with_check_commands(mut self, commands: Vec<String>) -> Self {
        self.check_commands = commands
            .into_iter()
            .filter_map(|command| validated_check_command(&command).map(str::to_string))
            .collect();
        self
    }

    /// Whether `command` IS one of the session's check commands — an exact,
    /// validated match, so `cd sub; cargo test` is not `cargo test`.
    fn is_check_command(&self, command: &str) -> bool {
        validated_check_command(command)
            .is_some_and(|command| self.check_commands.iter().any(|allowed| allowed == command))
    }
}

#[async_trait]
impl Tool for JobStartTool {
    fn name(&self) -> &str {
        "job_start"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    /// `git merge`/`git push` (and whatever `[git] core_operations` lists) must
    /// raise the interactive prompt even in Autonomous mode — a job must not
    /// become a way around the gate `shell` and `git` enforce.
    fn never_auto_for(&self, args: &serde_json::Value) -> bool {
        let Some(command) = args.get("command").and_then(|value| value.as_str()) else {
            return false;
        };
        let ops = self
            .core_operations
            .read()
            .expect("core_operations lock poisoned");
        command_is_core_git_op(command, &ops)
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_start",
            format!(
                "Start a command as a background JOB; returns its id at once. Then block on it \
                 with job_wait — never with sleep. No silence limit, a {} hour cap, same \
                 command/cwd/approval rules as shell. Not recorded as a check.",
                JOB_TOTAL.as_secs() / 3600
            ),
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to run in the background (same rules as shell)."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "Short label of what the job does, for the UI."
                    },
                    "cwd": {
                        "type": ["string", "null"],
                        "description": "Working directory relative to the project root (sandboxed). Default: the root."
                    }
                },
                "required": ["command", "purpose"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: JobStartArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolResult::error(invalid_args_error(
                    "job_start",
                    &error,
                    &args,
                    &tool_contract::recovery_hint("command + purpose"),
                ))
            }
        };
        let cwd = match self.jobs.cwd(args.cwd.as_deref()) {
            Ok(path) => path,
            Err(message) => return ToolResult::error(message),
        };
        let state = match self.jobs.start(&args.command, cwd).await {
            Ok(state) => state,
            Err(message) => return ToolResult::error(message),
        };
        let mut output = format!("job {} started: {}\n", state.id, state.command);
        if self.is_check_command(&state.command) {
            output.push_str(CHECK_NOT_RECORDED_NOTE);
            output.push('\n');
        }
        ToolResult::success(output).with_data(json!({ "job_id": state.id }))
    }
}

/// Arguments for `job_wait`.
#[derive(Debug, Deserialize)]
struct JobWaitArgs {
    job_id: String,
    /// Stop early when a NEW output line matches this regex.
    #[serde(default)]
    pattern: Option<String>,
    /// Seconds to wait before reporting "still running".
    #[serde(default)]
    timeout_s: Option<u64>,
}

/// `job_wait` — block server-side until the job exits, a line matches, or the
/// timeout passes.
pub struct JobWaitTool {
    jobs: Arc<JobRegistry>,
}

impl JobWaitTool {
    /// A waiter bound to the session's registry.
    pub fn new(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl Tool for JobWaitTool {
    fn name(&self) -> &str {
        "job_wait"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_wait",
            format!(
                "Block until the job exits, a NEW line matches `pattern`, or `timeout_s` \
                 (default {DEFAULT_WAIT_S}, max {MAX_WAIT_S}) passes. Returns the state \
                 (`exited <code>` / `matched` with the line / `still running`), the run time \
                 and the filtered tail.",
            ),
            json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "The id job_start returned (j1, j2, ...)."
                    },
                    "pattern": {
                        "type": ["string", "null"],
                        "description": "Regex: return as soon as a NEW output line matches it."
                    },
                    "timeout_s": {
                        "type": ["integer", "null"],
                        "description": "Seconds to wait before reporting the job as still running."
                    }
                },
                "required": ["job_id"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: JobWaitArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolResult::error(invalid_args_error(
                    "job_wait",
                    &error,
                    &args,
                    &tool_contract::recovery_hint("job_id"),
                ))
            }
        };
        let state = match self.jobs.lookup(&args.job_id) {
            Ok(state) => state,
            Err(message) => return ToolResult::error(message),
        };
        let pattern = match args
            .pattern
            .as_deref()
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .map(Regex::new)
            .transpose()
        {
            Ok(pattern) => pattern,
            Err(error) => return ToolResult::error(format!("invalid pattern regex: {error}")),
        };
        let timeout = Duration::from_secs(args.timeout_s.unwrap_or(DEFAULT_WAIT_S).clamp(1, MAX_WAIT_S));
        let outcome = wait_for_job(&state, pattern.as_ref(), timeout).await;
        let matched = match &outcome {
            WaitOutcome::Matched(line) => Some(line.clone()),
            WaitOutcome::Finished | WaitOutcome::TimedOut => None,
        };
        let tail = self.jobs.filtered_tail(&state);
        let label = match &matched {
            Some(line) => format!("matched: {line}"),
            None => state.state_text(),
        };
        let text = job_report(&state, &label, "tail (filtered):", &tail);
        let mut result_data = json!({
            "job_id": state.id,
            "state": state.state_text(),
            "elapsed_ms": state.elapsed().as_millis() as u64,
        });
        if let Some(line) = matched {
            result_data["matched"] = json!(line);
        }
        if let JobStatus::Exited(code) = state.status() {
            result_data["exit_code"] = json!(code);
        }
        // A killed or never-started job is a failure; an exit code is data.
        let success = !matches!(
            state.status(),
            JobStatus::Killed(_) | JobStatus::SpawnFailed(_)
        );
        ToolResult {
            success,
            output: text,
            data: Some(result_data),
        }
    }
}

/// Arguments for `job_tail`.
#[derive(Debug, Deserialize)]
struct JobTailArgs {
    job_id: String,
    /// How many raw lines to show (default 60, max 400).
    #[serde(default)]
    lines: Option<usize>,
}

/// `job_tail` — the last raw lines of a job, plus its state.
pub struct JobTailTool {
    jobs: Arc<JobRegistry>,
}

impl JobTailTool {
    /// A tail reader bound to the session's registry.
    pub fn new(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl Tool for JobTailTool {
    fn name(&self) -> &str {
        "job_tail"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_tail",
            format!(
                "The last `lines` (default {DEFAULT_TAIL_LINES}, max {MAX_TAIL_LINES}) raw \
                 lines of a job and its state.",
            ),
            json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "The id job_start returned."
                    },
                    "lines": {
                        "type": ["integer", "null"],
                        "description": "How many trailing lines to show."
                    }
                },
                "required": ["job_id"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: JobTailArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolResult::error(invalid_args_error(
                    "job_tail",
                    &error,
                    &args,
                    &tool_contract::recovery_hint("job_id"),
                ))
            }
        };
        let state = match self.jobs.lookup(&args.job_id) {
            Ok(state) => state,
            Err(message) => return ToolResult::error(message),
        };
        let lines = args
            .lines
            .unwrap_or(DEFAULT_TAIL_LINES)
            .clamp(1, MAX_TAIL_LINES);
        let (tail, skipped) = JobRegistry::raw_tail(&state, lines);
        let mut text = format!(
            "state: {}\nelapsed: {}\n",
            state.state_text(),
            format_elapsed(state.elapsed())
        );
        if skipped {
            text.push_str(LOG_EARLIER_OMITTED);
        }
        if tail.is_empty() {
            text.push_str("(no output)\n");
        } else {
            text.push_str(&tail);
            text.push('\n');
        }
        ToolResult::success(text).with_data(json!({
            "job_id": state.id,
            "state": state.state_text(),
            "elapsed_ms": state.elapsed().as_millis() as u64,
        }))
    }
}

/// Arguments for `job_kill`.
#[derive(Debug, Deserialize)]
struct JobKillArgs {
    job_id: String,
}

/// `job_kill` — kill a job's whole process tree.
pub struct JobKillTool {
    jobs: Arc<JobRegistry>,
}

impl JobKillTool {
    /// A killer bound to the session's registry.
    pub fn new(jobs: Arc<JobRegistry>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl Tool for JobKillTool {
    fn name(&self) -> &str {
        "job_kill"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_kill",
            "Kill a job's whole process tree, wait for it to be gone, then return the final \
             state and tail.",
            json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "The id job_start returned."
                    }
                },
                "required": ["job_id"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: JobKillArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolResult::error(invalid_args_error(
                    "job_kill",
                    &error,
                    &args,
                    &tool_contract::recovery_hint("job_id"),
                ))
            }
        };
        let state = match self.jobs.kill(&args.job_id).await {
            Ok(state) => state,
            Err(message) => return ToolResult::error(message),
        };
        let tail = self.jobs.filtered_tail(&state);
        let text = job_report(&state, &state.state_text(), "tail (filtered):", &tail);
        ToolResult::success(text).with_data(json!({
            "job_id": state.id,
            "state": state.state_text(),
            "elapsed_ms": state.elapsed().as_millis() as u64,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;

    /// A registry confined to `dir`, with the default shell filter.
    fn registry(dir: &TempDir) -> Arc<JobRegistry> {
        let sandbox = Sandbox::new(dir.path().to_path_buf()).expect("sandbox");
        Arc::new(JobRegistry::new(
            sandbox,
            Arc::new(RwLock::new(ShellFilterConfig::default())),
        ))
    }

    fn starter(jobs: &Arc<JobRegistry>) -> JobStartTool {
        JobStartTool::new(Arc::clone(jobs), Arc::new(RwLock::new(Vec::new())))
    }

    /// A sleep in the platform shell's own syntax — `shell.rs` owns that split
    /// (and the multi-platform policy keeps a platform switch out of this file).
    fn sleep_command(duration: Duration) -> String {
        crate::tool::agent::shell::test_sleep(duration)
    }

    fn job_id(result: &ToolResult) -> String {
        result
            .data
            .as_ref()
            .and_then(|data| data.get("job_id"))
            .and_then(Value::as_str)
            .expect("job_start returns a job id")
            .to_string()
    }

    /// Start `command` and return its id, asserting the start succeeded.
    async fn start(tool: &JobStartTool, command: &str) -> String {
        let result = tool
            .execute(json!({ "command": command, "purpose": "test job" }))
            .await;
        assert!(result.success, "job_start failed: {}", result.output);
        job_id(&result)
    }

    #[tokio::test]
    async fn job_wait_returns_exit_code_and_tail() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let id = start(
            &starter(&jobs),
            "echo one; echo error: boom; echo three; exit 2",
        )
        .await;
        let wait = JobWaitTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id, "timeout_s": 60 }))
            .await;
        assert!(wait.success, "job_wait failed: {}", wait.output);
        assert!(
            wait.output.contains("state: exited 2"),
            "the exit code is missing: {}",
            wait.output
        );
        assert!(
            wait.output.contains("boom"),
            "the tail lost the failing line: {}",
            wait.output
        );
        assert_eq!(
            wait.data.as_ref().expect("data").get("exit_code"),
            Some(&json!(2))
        );
    }

    #[tokio::test]
    async fn job_wait_pattern_returns_on_the_matching_line() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let command = format!(
            "{}; echo ready; {}",
            sleep_command(Duration::from_millis(500)),
            sleep_command(Duration::from_secs(30))
        );
        let id = start(&starter(&jobs), &command).await;
        let began = Instant::now();
        let wait = JobWaitTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id, "pattern": "ready", "timeout_s": 10 }))
            .await;
        assert!(wait.success, "job_wait failed: {}", wait.output);
        assert!(
            wait.output.contains("state: matched: ready"),
            "the matched line is missing: {}",
            wait.output
        );
        // Well under the job's 30-second sleep, so the return came from the
        // match and not from the exit. The bound is generous because a loaded
        // full-suite run can spend seconds starting powershell.exe before the
        // first line is printed at all.
        assert!(
            began.elapsed() < Duration::from_secs(15),
            "the pattern did not return early: {:?}",
            began.elapsed()
        );
        // The job is still sleeping its 30 s; the kill must be immediate, which
        // is only true if the whole tree died.
        let killing = Instant::now();
        let kill = JobKillTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id }))
            .await;
        assert!(kill.success, "job_kill failed: {}", kill.output);
        assert!(
            kill.output.contains("killed (job_kill)"),
            "the kill state is missing: {}",
            kill.output
        );
        assert!(
            killing.elapsed() < Duration::from_secs(5),
            "job_kill did not return promptly: {:?}",
            killing.elapsed()
        );
    }

    #[tokio::test]
    async fn job_wait_times_out_while_running() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let id = start(&starter(&jobs), &sleep_command(Duration::from_secs(10))).await;
        let began = Instant::now();
        let wait = JobWaitTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id, "timeout_s": 1 }))
            .await;
        assert!(wait.success, "job_wait failed: {}", wait.output);
        assert!(
            wait.output.contains("still running"),
            "the state is missing: {}",
            wait.output
        );
        assert!(
            began.elapsed() >= Duration::from_millis(900)
                && began.elapsed() < Duration::from_secs(6),
            "the wait did not honor timeout_s: {:?}",
            began.elapsed()
        );
    }

    #[tokio::test]
    async fn dropping_the_registry_kills_running_jobs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let jobs = registry(&dir);
        let command = crate::tool::agent::shell::test_heartbeat("beat.txt");
        let state = jobs.start(&command, root.clone()).await.expect("job start");
        let log_dir = state
            .log_path
            .parent()
            .expect("the log lives in a folder")
            .to_path_buf();
        assert!(log_dir.is_dir(), "the log folder was not created");
        let beat = root.join("beat.txt");
        let mut waited = Duration::ZERO;
        while !beat.exists() && waited < Duration::from_secs(30) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += Duration::from_millis(100);
        }
        assert!(beat.exists(), "the heartbeat never started");
        // Drop the registry — the job's only owner.
        drop(jobs);
        tokio::time::sleep(Duration::from_millis(900)).await;
        let after = std::fs::metadata(&beat).expect("heartbeat metadata").len();
        tokio::time::sleep(Duration::from_millis(700)).await;
        let later = std::fs::metadata(&beat).expect("heartbeat metadata").len();
        assert_eq!(
            after, later,
            "the heartbeat outlived the registry drop: the job's tree survived"
        );
        // The session's log folder goes too (the cleanup retries on Windows
        // while the aborted tasks close the file).
        let mut waited = Duration::ZERO;
        while log_dir.exists() && waited < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += Duration::from_millis(100);
        }
        assert!(
            !log_dir.exists(),
            "the log folder survived the registry drop: {}",
            log_dir.display()
        );
    }

    #[tokio::test]
    async fn fifth_job_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let jobs = registry(&dir);
        let mut ids = Vec::new();
        for _ in 0..MAX_RUNNING_JOBS {
            let state = jobs
                .start(&sleep_command(Duration::from_secs(30)), root.clone())
                .await
                .expect("a job within the limit");
            ids.push(state.id.clone());
        }
        let message = jobs
            .start(&sleep_command(Duration::from_secs(30)), root.clone())
            .await
            .err()
            .expect("the fifth job must be refused");
        assert!(
            message.contains("job limit reached"),
            "the refusal does not name the limit: {message}"
        );
        for id in &ids {
            assert!(
                message.contains(id.as_str()),
                "the refusal does not name the running {id}: {message}"
            );
        }
    }

    #[tokio::test]
    async fn job_start_validates_cwd_like_shell() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let tool = starter(&jobs);
        let outside = tool
            .execute(json!({ "command": "echo hi", "purpose": "test", "cwd": ".." }))
            .await;
        assert!(!outside.success, "an escaping cwd was accepted");
        assert!(
            outside.output.contains("cwd validation failed"),
            "the cwd error is missing: {}",
            outside.output
        );
        let missing = tool
            .execute(json!({ "command": "echo hi", "purpose": "test", "cwd": "no-such-dir" }))
            .await;
        assert!(!missing.success, "a missing cwd was accepted");
        assert!(
            missing.output.contains("cwd validation failed")
                || missing.output.contains("not a directory"),
            "the cwd error is missing: {}",
            missing.output
        );
        // No cwd: the project root, exactly like shell.
        let id = start(&tool, "echo hi").await;
        let wait = JobWaitTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id, "timeout_s": 60 }))
            .await;
        assert!(
            wait.output.contains("state: exited 0"),
            "a root-cwd job did not run: {}",
            wait.output
        );
    }

    #[tokio::test]
    async fn job_start_notes_a_check_command_run_is_not_recorded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let tool = starter(&jobs).with_check_commands(vec!["echo recorded".to_string()]);
        let noted = tool
            .execute(json!({ "command": "echo recorded", "purpose": "test" }))
            .await;
        assert!(noted.success, "job_start failed: {}", noted.output);
        assert!(
            noted.output.contains(CHECK_NOT_RECORDED_NOTE),
            "the check note is missing: {}",
            noted.output
        );
        let other = tool
            .execute(json!({ "command": "echo not-a-check", "purpose": "test" }))
            .await;
        assert!(other.success, "job_start failed: {}", other.output);
        assert!(
            !other.output.contains(CHECK_NOT_RECORDED_NOTE),
            "a non-check command got the check note: {}",
            other.output
        );
    }

    #[tokio::test]
    async fn unknown_job_id_lists_the_session_jobs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let empty = JobWaitTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": "j9" }))
            .await;
        assert!(!empty.success);
        assert!(
            empty.output.contains("this session has no jobs"),
            "{}",
            empty.output
        );
        let id = start(&starter(&jobs), "echo hi").await;
        let unknown = JobTailTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": "j42" }))
            .await;
        assert!(!unknown.success);
        assert!(
            unknown.output.contains(&id),
            "the error does not list the session's jobs: {}",
            unknown.output
        );
    }

    #[test]
    fn check_command_note_matches_only_the_exact_validated_command() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let tool = starter(&jobs).with_check_commands(vec![
            "cargo test --workspace".to_string(),
            "cargo fmt --check".to_string(),
        ]);
        assert!(tool.is_check_command("cargo test --workspace"));
        assert!(!tool.is_check_command("cargo test --workspace --release"));
        assert!(!tool.is_check_command("cd sub; cargo test --workspace"));
        assert!(!tool.is_check_command("cargo test"));
    }

    #[test]
    fn partition_log_restores_both_streams() {
        let log = "[out] Compiling mnemo v0.1.0\n[err] error: boom\n[out] warning: keep\n";
        let (stdout, stderr) = partition_log(log);
        assert_eq!(stdout, "Compiling mnemo v0.1.0\nwarning: keep\n");
        assert_eq!(stderr, "error: boom\n");
    }

    #[test]
    fn read_log_tail_keeps_the_end_and_flags_the_cut() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tail.log");
        let log: String = (0..200)
            .map(|index| format!("[out] line {index}\n"))
            .collect();
        std::fs::write(&path, &log).expect("write log");
        let (text, omitted) = read_log_tail(&path, 200);
        assert!(omitted, "a cut read must be flagged");
        assert!(text.ends_with("line 199\n"), "the end was lost: {text}");
        assert!(text.starts_with("[out]"), "the cut is not on a line boundary");
        let (whole, not_omitted) = read_log_tail(&path, 10_000);
        assert!(!not_omitted, "a whole read must not be flagged");
        assert_eq!(whole, log);
    }

    #[tokio::test]
    async fn job_tail_shows_untagged_raw_lines() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = registry(&dir);
        let id = start(&starter(&jobs), "echo alpha; echo beta").await;
        // Let the job finish so the log is closed.
        let _ = JobWaitTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id, "timeout_s": 60 }))
            .await;
        let tail = JobTailTool::new(Arc::clone(&jobs))
            .execute(json!({ "job_id": id }))
            .await;
        assert!(tail.success, "job_tail failed: {}", tail.output);
        assert!(
            tail.output.contains("alpha") && tail.output.contains("beta"),
            "the raw lines are missing: {}",
            tail.output
        );
        assert!(
            !tail.output.contains(STDOUT_TAG) && !tail.output.contains(STDERR_TAG),
            "the stream tags leaked into the raw tail: {}",
            tail.output
        );
    }
}

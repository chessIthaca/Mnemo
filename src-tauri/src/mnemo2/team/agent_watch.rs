// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Following a worker that runs as a Claude Code agent.
//!
//! The lead launches such an agent with its own Agent tool, so Mnemo has no
//! session to watch. The lead registers the agent (`team_agent_started`) and
//! Mnemo then reads the agent's transcript, the `agent-<id>.jsonl` file
//! Claude Code writes under `~/.claude/projects/<slug>/<session>/subagents/`,
//! for the three facts a worker's session would give it: when it last did
//! something, how many tool calls it made and how many tokens it used. The
//! `finished` flag of the `agent-<id>.meta.json` beside it says the agent has
//! ended. A watcher task turns silence and the end into one notice each.
//!
//! The transcript is read incrementally: each worker keeps the byte offset
//! after its last complete line, so a long transcript is never parsed twice.
//! Malformed lines are skipped. Nothing outside `~/.claude/projects` and the
//! file the lead named is ever read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use mnemo::app::worktrees::is_link_or_junction;

use crate::mnemo2::phase3;

use super::pump::save_workers;
use super::record::{
    append_inbox, InboxEntry, InboxKind, MessageSender, WorkerRecord, WorkerStatus,
};
use super::{BoardSignal, TeamRuntime};

/// How long a transcript may stay unchanged before the agent counts as
/// stalled. Fixed: there is no setting.
pub(super) const STALL_MS: u128 = 10 * 60 * 1000;

/// How often the watcher looks.
const WATCH_INTERVAL: Duration = Duration::from_secs(60);

/// What Mnemo knows about one registered agent, as `team_status` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AgentView {
    pub(super) id: String,
    pub(super) transcript: Option<String>,
    pub(super) last_activity_ms: Option<u128>,
    pub(super) tool_calls: u64,
    pub(super) tokens: u64,
    pub(super) finished: bool,
    pub(super) stalled: bool,
}

impl AgentView {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "transcript": self.transcript,
            "last_activity_ms": self.last_activity_ms,
            "tool_calls": self.tool_calls,
            "tokens": self.tokens,
            "finished": self.finished,
            "stalled": self.stalled,
        })
    }
}

/// The reading state of one registered agent.
#[derive(Default)]
struct Observed {
    /// The agent id this state belongs to; a new registration starts over.
    agent_id: String,
    /// The file the counters were read from.
    path: Option<PathBuf>,
    /// Bytes read from the file, including the unfinished `tail`.
    offset: u64,
    /// The unfinished last line, kept so nothing is read twice.
    tail: Vec<u8>,
    /// A line longer than the cap is being skipped up to its newline.
    skipping: bool,
    /// The longest line kept; 0 means [`TAIL_CAP`]. Tests set a small one.
    cap: usize,
    /// Length and hash of the file's first bytes when last read: a changed
    /// hash means the file was replaced.
    fingerprint: Option<(u64, u64)>,
    tool_calls: u64,
    tokens: u64,
    /// When Mnemo first saw this registration: the silence clock before a
    /// transcript exists.
    registered_ms: u128,
    /// A stall notice went out for the current silent spell.
    stall_noticed: bool,
    /// The agent finished and the lead was told.
    finish_noticed: bool,
}

#[derive(Default)]
struct Inner {
    states: HashMap<String, Observed>,
    /// Where the transcripts live; `~/.claude/projects` unless a test says
    /// otherwise.
    projects_root: Option<PathBuf>,
    /// The watcher task, while one runs.
    task: Option<JoinHandle<()>>,
}

/// Every registered agent's reading state, shared with the watcher task.
#[derive(Clone, Default)]
pub(super) struct AgentWatch {
    inner: Arc<StdMutex<Inner>>,
}

impl AgentWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Point the locator at another projects folder. Tests only.
    #[cfg(test)]
    pub(super) fn set_projects_root_for_test(&self, root: PathBuf) {
        self.lock().projects_root = Some(root);
    }

    /// Forget a worker's reading state (a new registration, or the worker
    /// is gone).
    pub(super) fn forget(&self, worker_id: &str) {
        self.lock().states.remove(worker_id);
    }

    /// Stop the watcher task.
    pub(super) fn stop(&self) {
        if let Some(task) = self.lock().task.take() {
            task.abort();
        }
    }

    /// Lower a worker's line cap. Tests only.
    #[cfg(test)]
    pub(super) fn set_line_cap_for_test(&self, worker_id: &str, cap: usize) {
        if let Some(observed) = self.lock().states.get_mut(worker_id) {
            observed.cap = cap;
        }
    }

    /// The bytes of an unfinished line kept for a worker. Tests only.
    #[cfg(test)]
    pub(super) fn tail_len_for_test(&self, worker_id: &str) -> Option<usize> {
        self.lock().states.get(worker_id).map(|o| o.tail.len())
    }

    /// Whether a watcher task is running. Tests only.
    #[cfg(test)]
    pub(super) fn running_for_test(&self) -> bool {
        self.lock().task.is_some()
    }

    fn projects_root(&self) -> Option<PathBuf> {
        if let Some(root) = self.lock().projects_root.clone() {
            return Some(root);
        }
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .filter(|home| !home.is_empty())?;
        Some(PathBuf::from(home).join(".claude").join("projects"))
    }
}

/// Whether `id` can be part of a transcript file name: Claude Code's agent
/// ids are short alphanumeric strings.
pub(super) fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The newest `<root>/<slug>/<session>/subagents/agent-<agent_id>.jsonl`.
fn locate(root: &Path, agent_id: &str) -> Option<PathBuf> {
    if !valid_agent_id(agent_id) {
        return None;
    }
    let file = format!("agent-{agent_id}.jsonl");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for slug in std::fs::read_dir(root).ok()?.flatten() {
        let Ok(sessions) = std::fs::read_dir(slug.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let candidate = session.path().join("subagents").join(&file);
            let Some(candidate) = confined(root, &candidate, agent_id) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&candidate) else {
                continue;
            };
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
                newest = Some((modified, candidate));
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// The path when it is an existing file named exactly
/// `agent-<agent_id>.jsonl` whose canonical form lies under `root` and
/// whose own components below the root are not symlinks or junctions.
fn confined(root: &Path, path: &Path, agent_id: &str) -> Option<PathBuf> {
    if !valid_agent_id(agent_id) {
        return None;
    }
    let expected = format!("agent-{agent_id}.jsonl");
    if path.file_name()?.to_str()? != expected {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    let canonical_root = std::fs::canonicalize(root).ok()?;
    if !canonical.starts_with(&canonical_root)
        || canonical.file_name()?.to_str()? != expected
        || !std::fs::metadata(&canonical).ok()?.is_file()
    {
        return None;
    }
    // Walk the given spelling upward from the file to the projects root:
    // the file and every folder strictly below the root must not be a link,
    // and the walk must reach the root.
    let mut current = path;
    loop {
        if current == root {
            return Some(canonical);
        }
        let meta = std::fs::symlink_metadata(current).ok()?;
        if is_link_or_junction(&meta) {
            return None;
        }
        if std::fs::canonicalize(current).ok()? == canonical_root {
            // Another spelling of the root itself.
            return Some(canonical);
        }
        current = current.parent()?;
    }
}

/// What a transcript search found.
pub(super) struct Found {
    pub(super) path: Option<String>,
    /// The lead named a path that was not accepted.
    pub(super) ignored_given: bool,
}

/// Find an agent's transcript: the file the lead named when it is confined
/// to `~/.claude/projects` and named for the agent, else the documented
/// location.
pub(super) fn find_transcript(
    watch: &AgentWatch,
    agent_id: &str,
    given: Option<&str>,
) -> Found {
    let given = given
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let root = watch.projects_root();
    let mut ignored_given = false;
    if let Some(path) = given {
        match root.as_deref().and_then(|root| confined(root, &path, agent_id)) {
            Some(path) => {
                return Found {
                    path: Some(path.to_string_lossy().into_owned()),
                    ignored_given: false,
                }
            }
            None => ignored_given = true,
        }
    }
    let path = root
        .and_then(|root| locate(&root, agent_id))
        .map(|path| path.to_string_lossy().into_owned());
    Found {
        path,
        ignored_given,
    }
}

fn epoch_ms(time: std::time::SystemTime) -> u128 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Count one transcript line into `observed`; anything malformed adds
/// nothing.
fn read_line(observed: &mut Observed, line: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let Some(message) = value.get("message") else {
        return;
    };
    let assistant = message.get("role").and_then(Value::as_str) == Some("assistant")
        || value.get("type").and_then(Value::as_str) == Some("assistant");
    if !assistant {
        return;
    }
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        let calls = blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .count();
        observed.tool_calls = observed.tool_calls.saturating_add(calls as u64);
    }
    if let Some(usage) = message.get("usage") {
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ] {
            let tokens = usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            observed.tokens = observed.tokens.saturating_add(tokens);
        }
    }
}

/// The longest unfinished line kept between looks; a longer line is skipped.
const TAIL_CAP: usize = 4 * 1024 * 1024;

/// How many leading bytes fingerprint a transcript file.
const FINGERPRINT_BYTES: u64 = 512;

/// How many bytes one read takes.
const CHUNK_BYTES: usize = 64 * 1024;

/// A hash of the file's first `len` bytes, `None` when they cannot be read.
fn fingerprint(file: &mut std::fs::File, len: u64) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::new();
    file.take(len).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 != len {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

impl Observed {
    /// Forget everything read: the file is new or was replaced.
    fn reset_reading(&mut self) {
        self.offset = 0;
        self.tool_calls = 0;
        self.tokens = 0;
        self.tail.clear();
        self.skipping = false;
        self.fingerprint = None;
    }

    /// The longest line that is parsed.
    fn line_cap(&self) -> usize {
        if self.cap == 0 {
            TAIL_CAP
        } else {
            self.cap
        }
    }

    /// Count the complete lines in `tail`, keeping the unfinished rest.
    fn drain_lines(&mut self) {
        let cap = self.line_cap();
        let mut start = 0;
        while let Some(found) = self.tail[start..].iter().position(|byte| *byte == 10) {
            let end = start + found;
            if self.skipping {
                self.skipping = false;
            } else if end - start > cap {
                // A line over the cap is skipped, though it is complete.
            } else if end > start {
                let line = self.tail[start..end].to_vec();
                read_line(self, &line);
            }
            start = end + 1;
        }
        self.tail.drain(..start);
        if self.skipping {
            self.tail.clear();
        } else if self.tail.len() > cap {
            // An unfinished line longer than the cap: skip it up to its newline.
            self.tail.clear();
            self.skipping = true;
        }
    }
}

/// Read what the file gained since the last look: only new bytes, in
/// chunks, and only complete lines are parsed.
fn read_new(observed: &mut Observed, path: &Path, length: u64) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    // Shrunk, or its first bytes changed: a different file now.
    let replaced = length < observed.offset
        || observed
            .fingerprint
            .is_some_and(|(len, hash)| fingerprint(&mut file, len) != Some(hash));
    if replaced {
        observed.reset_reading();
    }
    if length > observed.offset {
        if file.seek(SeekFrom::Start(observed.offset)).is_err() {
            return;
        }
        let mut remaining = length - observed.offset;
        let mut chunk = vec![0u8; CHUNK_BYTES];
        while remaining > 0 {
            let want = remaining.min(CHUNK_BYTES as u64) as usize;
            let Ok(got) = file.read(&mut chunk[..want]) else {
                break;
            };
            if got == 0 {
                break;
            }
            observed.tail.extend_from_slice(&chunk[..got]);
            observed.offset += got as u64;
            remaining -= got as u64;
            observed.drain_lines();
        }
    }
    let len = observed.offset.min(FINGERPRINT_BYTES);
    observed.fingerprint = fingerprint(&mut file, len).map(|hash| (len, hash));
}

/// Whether the agent's meta file says it ended.
fn meta_finished(transcript: &Path) -> bool {
    let meta = transcript.with_extension("meta.json");
    std::fs::read_to_string(meta)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("finished").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// What one look at a registered agent found.
pub(super) struct Looked {
    pub(super) view: AgentView,
    /// A transcript path found now that the record does not carry yet.
    pub(super) found: Option<String>,
}

/// Read a registered agent's transcript and report it. `None` when the
/// worker has no registered agent.
pub(super) fn look(watch: &AgentWatch, worker: &WorkerRecord, now: u128) -> Option<Looked> {
    let agent_id = worker.claude_agent_id.clone()?;
    // The recorded file, or a fresh search when it is missing or gone.
    let root = watch.projects_root();
    let recorded = worker
        .claude_agent_transcript
        .as_deref()
        .map(PathBuf::from)
        .and_then(|path| confined(root.as_deref()?, &path, &agent_id));
    let mut found = None;
    let path = match recorded {
        Some(path) => Some(path),
        None => {
            let path = find_transcript(watch, &agent_id, None).path.map(PathBuf::from);
            if let Some(path) = &path {
                found = Some(path.to_string_lossy().into_owned());
            }
            path
        }
    };
    let mut inner = watch.lock();
    let observed = inner.states.entry(worker.id.clone()).or_default();
    if observed.agent_id != agent_id {
        *observed = Observed {
            agent_id: agent_id.clone(),
            registered_ms: now,
            ..Observed::default()
        };
    }
    if observed.path != path {
        observed.path = path.clone();
        observed.reset_reading();
    }
    let mut last_activity = None;
    let mut finished = false;
    if let Some(path) = &path {
        if let Ok(meta) = std::fs::metadata(path) {
            last_activity = meta.modified().ok().map(epoch_ms);
            read_new(observed, path, meta.len());
        }
        finished = meta_finished(path);
    }
    let last = last_activity.unwrap_or(observed.registered_ms);
    let running = matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing);
    let stalled = running && !finished && now.saturating_sub(last) >= STALL_MS;
    Some(Looked {
        view: AgentView {
            id: agent_id,
            transcript: path.map(|path| path.to_string_lossy().into_owned()),
            last_activity_ms: last_activity,
            tool_calls: observed.tool_calls,
            tokens: observed.tokens,
            finished,
            stalled,
        },
        found,
    })
}

impl TeamRuntime {
    /// One look at a worker's registered agent, remembering a transcript
    /// path found late and the agent's last activity on the record.
    pub(super) async fn agent_view(&self, worker: &WorkerRecord) -> Option<AgentView> {
        refresh(&self.records, &self.agent_watch, worker).await
    }

    /// Start the watcher when a registered agent worker is Working or
    /// Fixing and no watcher runs.
    pub(super) async fn ensure_agent_watch(&self) {
        let wanted = self
            .records
            .lock()
            .await
            .iter()
            .any(watched);
        if !wanted {
            return;
        }
        let mut inner = self.agent_watch.lock();
        if inner.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let records = self.records.clone();
        let signals = self.signals.clone();
        let watch = self.agent_watch.clone();
        inner.task = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                if !tick(&records, &signals, &watch).await {
                    return;
                }
            }
        }));
    }

    /// One pass of the watcher. Tests call it instead of waiting a minute.
    #[cfg(test)]
    pub(super) async fn agent_watch_tick(&self) {
        tick(&self.records, &self.signals, &self.agent_watch).await;
    }
}

/// A registered agent worker whose agent still owes the lead something.
fn watched(worker: &WorkerRecord) -> bool {
    worker.claude_agent.is_some()
        && worker.claude_agent_id.is_some()
        && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
}

async fn refresh(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    watch: &AgentWatch,
    worker: &WorkerRecord,
) -> Option<AgentView> {
    let looked = look(watch, worker, phase3::now_ms())?;
    let found = looked.found.clone();
    let last = looked.view.last_activity_ms;
    if found.is_some() || last.is_some() {
        let mut records = records.lock().await;
        if let Some(record) = records.iter_mut().find(|record| record.id == worker.id) {
            // Only while it is still the same agent.
            if record.claude_agent_id == worker.claude_agent_id {
                if let Some(path) = found {
                    record.claude_agent_transcript = Some(path);
                }
                if let Some(last) = last {
                    record.last_activity_ms = record.last_activity_ms.max(last);
                }
            }
        }
    }
    Some(looked.view)
}

/// One pass: look at every watched worker, send the notices that are due.
/// Returns whether the watcher should keep running.
async fn tick(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    signals: &broadcast::Sender<BoardSignal>,
    watch: &AgentWatch,
) -> bool {
    let workers: Vec<WorkerRecord> = records
        .lock()
        .await
        .iter()
        .filter(|worker| watched(worker))
        .cloned()
        .collect();
    if workers.is_empty() {
        // Re-check under the task slot, so a registration racing the exit
        // either keeps this task or finds the slot free.
        let mut inner = watch.lock();
        let again = records.try_lock().map(|r| r.iter().any(watched)).unwrap_or(true);
        if again {
            return true;
        }
        inner.task = None;
        return false;
    }
    for worker in workers {
        let Some(view) = refresh(records, watch, &worker).await else {
            continue;
        };
        let now = phase3::now_ms();
        let mut notice = None;
        {
            let mut inner = watch.lock();
            let Some(observed) = inner.states.get_mut(&worker.id) else {
                continue;
            };
            if !view.finished {
                // A continued agent can finish again.
                observed.finish_noticed = false;
            }
            if view.finished {
                if !observed.finish_noticed {
                    observed.finish_noticed = true;
                    notice = Some((
                        format!("agent-finished:{}:{}:{}", worker.id, view.id, worker.turns),
                        format!(
                            "worker_id={id}\n{id}'s Claude Code agent finished; call team_agent_done with its report.",
                            id = worker.id
                        ),
                    ));
                }
            } else if view.stalled {
                if !observed.stall_noticed {
                    observed.stall_noticed = true;
                    let last = view.last_activity_ms.unwrap_or(observed.registered_ms);
                    let minutes = now.saturating_sub(last) / 60_000;
                    notice = Some((
                        format!("agent-stall:{}:{now}", worker.id),
                        format!(
                            "worker_id={id}\n{id}'s Claude Code agent has written nothing for {minutes} minutes; it may have stalled or stopped. Check it, then continue it with SendMessage or call team_agent_done.",
                            id = worker.id
                        ),
                    ));
                }
            } else {
                // Activity resumed: the next silence is a new episode.
                observed.stall_noticed = false;
            }
        }
        if let Some((key, text)) = notice {
            let _ = signals.send(BoardSignal::Notice { key, text });
        }
    }
    true
}

/// Makes every progress notice key unique, so two notes in one millisecond
/// both reach the lead.
static PROGRESS_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The clock progress notes are stamped with. Tests pin it (non-zero) to
/// post two notes in the same millisecond.
#[cfg(test)]
pub(super) static PROGRESS_CLOCK_FIXED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn progress_now() -> u128 {
    #[cfg(test)]
    {
        let fixed = PROGRESS_CLOCK_FIXED.load(std::sync::atomic::Ordering::SeqCst);
        if fixed != 0 {
            return u128::from(fixed);
        }
    }
    phase3::now_ms()
}

/// The longest progress note, in characters.
const PROGRESS_NOTE_CHARS: usize = 500;

/// The agent workers `team_agent_started` and `team_progress` accept.
fn agent_worker_in_work(worker: &WorkerRecord, tool: &str) -> Result<(), String> {
    if worker.claude_agent.is_none() {
        return Err(format!(
            "{} does not run as a Claude Code agent: {tool} only serves agent workers",
            worker.id
        ));
    }
    if !matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing) {
        return Err(format!(
            "{} is not Working or Fixing: {tool} needs a running agent worker",
            worker.id
        ));
    }
    Ok(())
}

impl TeamRuntime {
    /// `team_agent_started`: register the Claude Code agent the lead
    /// launched for a worker, so Mnemo can follow its transcript. Returns
    /// the worker and the sentence the lead reads.
    pub(super) async fn agent_started(
        &self,
        worker_id: &str,
        agent_id: &str,
        transcript_path: Option<&str>,
    ) -> Result<(WorkerRecord, String), String> {
        let agent_id = agent_id.trim();
        if !valid_agent_id(agent_id) {
            return Err(
                "team_agent_started requires the agent's id: letters, digits, '-' or '_'"
                    .to_string(),
            );
        }
        let worker = self.worker(worker_id).await?;
        agent_worker_in_work(&worker, "team_agent_started")?;
        let search = find_transcript(&self.agent_watch, agent_id, transcript_path);
        let transcript = search.path;
        {
            let mut records = self.records.lock().await;
            let Some(record) = records.iter_mut().find(|record| record.id == worker_id) else {
                return Err(format!("unknown worker '{worker_id}'"));
            };
            record.claude_agent_id = Some(agent_id.to_string());
            record.claude_agent_transcript = transcript.clone();
        }
        // A new registration reads its transcript from the start.
        self.agent_watch.forget(worker_id);
        save_workers(
            &self.records,
            &self.usage,
            &self.save_lock,
            &self.persistence,
        )
        .await?;
        self.ensure_agent_watch().await;
        let ignored = if search.ignored_given {
            "The transcript_path you gave was ignored: only agent-<agent_id>.jsonl under ~/.claude/projects, reached without links, is accepted. "
        } else {
            ""
        };
        let text = match &transcript {
            Some(path) => format!(
                "{ignored}Registered Claude Code agent {agent_id} for {worker_id}; Mnemo follows its transcript {path}."
            ),
            None => format!(
                "{ignored}Registered Claude Code agent {agent_id} for {worker_id}; its transcript was not found yet, so Mnemo keeps looking for it under ~/.claude/projects."
            ),
        };
        let worker = self.worker(worker_id).await?;
        Ok((worker, text))
    }

    /// `team_progress`: a one-line note from a worker's own Claude Code
    /// agent. It lands in the worker's log and reaches the lead as a notice.
    pub(super) async fn agent_progress(
        &self,
        worker_id: &str,
        note: &str,
    ) -> Result<WorkerRecord, String> {
        let note = note.trim();
        if note.is_empty() {
            return Err("team_progress requires a non-empty 'note'".to_string());
        }
        if note.chars().count() > PROGRESS_NOTE_CHARS {
            return Err(format!(
                "team_progress notes are at most {PROGRESS_NOTE_CHARS} characters: send one short line"
            ));
        }
        let worker = self.worker(worker_id).await?;
        agent_worker_in_work(&worker, "team_progress")?;
        let at_ms = progress_now();
        self.update_worker(worker_id, |worker| {
            append_inbox(
                worker,
                InboxEntry {
                    sender: MessageSender::ClaudeAgent,
                    kind: InboxKind::Progress,
                    text: note.to_string(),
                    at_ms,
                },
            );
        })
        .await;
        save_workers(
            &self.records,
            &self.usage,
            &self.save_lock,
            &self.persistence,
        )
        .await?;
        let _ = self.signals.send(BoardSignal::Notice {
            key: format!(
                "progress:{worker_id}:{at_ms}:{}",
                PROGRESS_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            text: format!("worker_id={worker_id}\n{worker_id} progress: {note}"),
        });
        self.worker(worker_id).await
    }
}

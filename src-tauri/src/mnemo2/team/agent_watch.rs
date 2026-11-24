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
use crate::mnemo2::runtime::TEAM_NOTICE_PREFIX;

use super::dispatch::BoxFuture;
use super::pump::save_workers;
use super::record::{
    append_inbox, HandedBack, InboxEntry, InboxKind, MessageSender, WorkerRecord, WorkerStatus,
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
    /// When Mnemo first saw this registration: the silence clock when
    /// neither a transcript nor the record says anything.
    registered_ms: u128,
    /// A stall notice went out for the current silent spell.
    stall_noticed: bool,
    /// The worker turn whose run's finish was handed to the runtime: each
    /// run (agent and turn) is finished once, however short it was.
    handled_turns: Option<u32>,
    /// The transcript's length when the current run was started by a
    /// continuation or fix round: a finish counts only once the transcript
    /// grew past it, so the previous run's finished flag is not taken for
    /// this run's.
    run_floor: Option<u64>,
    /// When the current run was started by a continuation or fix round: the
    /// silence clock starts there, not at the old transcript's last write.
    run_start_ms: u128,
}

/// An agent run Mnemo finished itself, as the completed record shows it:
/// `team_agent_done` on the same run then neither finishes it twice nor
/// notifies twice. In memory only: a restart forgets it.
#[derive(Clone, Debug, PartialEq)]
struct AutoFinished {
    agent_id: String,
    generation: u64,
    turns: u32,
}

/// A registered agent whose transcript says it finished while its worker
/// still works: the run the watcher asks the runtime to finish.
#[derive(Clone, Debug)]
pub(super) struct FinishedRun {
    pub(super) worker_id: String,
    pub(super) agent_id: String,
    /// The worker's turn count when the finish was seen: with the worker and
    /// the agent, the run's key.
    pub(super) turns: u32,
    /// The agent's transcript, for its last message.
    pub(super) transcript: Option<String>,
}

/// The report of an agent that ended without a final message.
pub(super) const NO_FINAL_MESSAGE: &str =
    "The Claude Code agent finished without a final message.";

#[derive(Default)]
struct Inner {
    states: HashMap<String, Observed>,
    /// The agent runs Mnemo finished itself, by worker id.
    auto_finished: HashMap<String, AutoFinished>,
    /// Tests only: run a watcher pass right after a continuation or fix
    /// round put its worker back to work, and keep the runs it found.
    #[cfg(test)]
    probe: Option<Vec<FinishedRun>>,
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

    /// Remember that Mnemo finished `worker`'s agent run itself; `worker` is
    /// the completed record.
    pub(super) fn mark_auto_finished(&self, worker: &WorkerRecord) {
        let Some(agent_id) = worker.claude_agent_id.clone() else {
            return;
        };
        self.lock().auto_finished.insert(
            worker.id.clone(),
            AutoFinished {
                agent_id,
                generation: worker.generation,
                turns: worker.turns,
            },
        );
    }

    /// Whether `worker` is still exactly the completed record of a run Mnemo
    /// finished itself.
    pub(super) fn auto_finished(&self, worker: &WorkerRecord) -> bool {
        worker.status == WorkerStatus::Completed
            && self.lock().auto_finished.get(&worker.id).is_some_and(|run| {
                Some(run.agent_id.as_str()) == worker.claude_agent_id.as_deref()
                    && run.generation == worker.generation
                    && run.turns == worker.turns
            })
    }

    /// Forget a run Mnemo finished itself (the lead marked it failed).
    pub(super) fn clear_auto_finished(&self, worker_id: &str) {
        self.lock().auto_finished.remove(worker_id);
    }

    /// Look at a finish again on the next pass: finishing it failed, or no
    /// runtime could take it.
    fn retry_finish(&self, run: &FinishedRun) {
        if let Some(observed) = self.lock().states.get_mut(&run.worker_id) {
            if observed.agent_id == run.agent_id && observed.handled_turns == Some(run.turns) {
                observed.handled_turns = None;
            }
        }
    }

    /// A registered agent worker went back to work (a continuation or a fix
    /// round): its next finish must come from the transcript growing past
    /// where it is now, and its silence counts from now.
    pub(super) fn note_run_start(&self, worker: &WorkerRecord, now: u128) {
        let Some(agent_id) = worker.claude_agent_id.clone() else {
            return;
        };
        let len = worker
            .claude_agent_transcript
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map_or(0, |meta| meta.len());
        let mut inner = self.lock();
        let observed = inner.states.entry(worker.id.clone()).or_default();
        if observed.agent_id != agent_id {
            *observed = Observed {
                agent_id,
                registered_ms: now,
                ..Observed::default()
            };
        }
        observed.run_floor = Some(len);
        observed.run_start_ms = now;
        observed.stall_noticed = false;
    }

    /// Stop the watcher task, and wait until the aborted task is dropped (it
    /// lets go of what it holds only when the runtime polls it again).
    pub(super) async fn stop(&self) {
        let task = self.lock().task.take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
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
        #[cfg(test)]
        if let Some(root) = TEST_PROJECTS_ROOT.with(|root| root.borrow().clone()) {
            return Some(root);
        }
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .filter(|home| !home.is_empty())?;
        Some(PathBuf::from(home).join(".claude").join("projects"))
    }
}

#[cfg(test)]
thread_local! {
    /// The projects folder every watch on this test thread uses unless it
    /// was given its own: lets a test reach `TeamRuntime::open`'s watch.
    pub(super) static TEST_PROJECTS_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
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
    // With a transcript, its last write (or the current run's start, when
    // later); without one, the record's last activity, which survives a
    // restart, so an agent whose transcript is gone can still stall.
    let last = match last_activity {
        Some(written) => written.max(observed.run_start_ms),
        None if worker.last_activity_ms > 0 => worker.last_activity_ms.max(observed.run_start_ms),
        None => observed.registered_ms.max(observed.run_start_ms),
    };
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

    /// Start the watcher when an agent worker is Working or Fixing (its
    /// agent registered or still awaited) and no watcher runs. Each pass
    /// also runs the hand-back check while no lead is attached.
    ///
    /// Boxed: the hand-back check may register a launched agent, which
    /// starts the watch again, so this future's type must not be inferred
    /// through the watch task's own.
    pub(super) fn ensure_agent_watch(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let wanted = self
                .records
                .lock()
                .await
                .iter()
                .any(keeps_watch);
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
            // The runtime, to finish a worker whose agent ended; set once the
            // runtime holds its own Arc (`attach_board`).
            let limits = self.usage_limits.clone();
            inner.task = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(WATCH_INTERVAL).await;
                    let (keep, finished, _stalled) = tick(&records, &signals, &watch).await;
                    match limits.team() {
                        Some(team) => team.after_agent_watch(&finished).await,
                        None => {
                            for run in &finished {
                                watch.retry_finish(run);
                            }
                        }
                    }
                    if !keep {
                        return;
                    }
                }
            }));
        })
    }

    /// Arm the run-start probe ([`TeamRuntime::probe_run_start`]). Tests only.
    #[cfg(test)]
    pub(super) fn arm_run_start_probe(&self) {
        self.agent_watch.lock().probe = Some(Vec::new());
    }

    /// The runs the armed probe's pass found ready to finish. Tests only.
    #[cfg(test)]
    pub(super) fn take_run_start_probe(&self) -> Vec<FinishedRun> {
        self.agent_watch.lock().probe.take().unwrap_or_default()
    }

    /// The seam right after a continuation or fix round put its worker back
    /// to work (the merge lock still held): when armed, one watcher pass runs
    /// here and the runs it would finish are kept for the test.
    #[cfg(test)]
    pub(super) async fn probe_run_start(&self) {
        if self.agent_watch.lock().probe.is_none() {
            return;
        }
        let (_, finished, _) = tick(&self.records, &self.signals, &self.agent_watch).await;
        if let Some(probe) = self.agent_watch.lock().probe.as_mut() {
            probe.extend(finished);
        }
    }

    /// One pass of the watcher. Tests call it instead of waiting a minute.
    #[cfg(test)]
    pub(super) async fn agent_watch_tick(&self) {
        let (_, finished, _) = tick(&self.records, &self.signals, &self.agent_watch).await;
        self.after_agent_watch(&finished).await;
    }

    /// What a watcher pass leaves to the runtime: finish the runs that
    /// ended, and, while no Claude Code lead is attached, run the hand-back
    /// check ([`TeamRuntime::hand_back_agent_workers`]): it hands back only
    /// once the lead has been away for the grace period (plan ee73bd2f
    /// decision 1), so a stall alone hands nothing back.
    async fn after_agent_watch(&self, finished: &[FinishedRun]) {
        for run in finished {
            self.finish_agent_run(run).await;
        }
        if !self.external_lead_attached() {
            if let Err(error) = self.hand_back_agent_workers().await {
                eprintln!("team: could not hand back agent workers: {error}");
            }
        }
    }

    /// Finish the worker of an agent run that ended, as `team_agent_done`
    /// would, with the agent's last message as its report. A failure is
    /// logged and tried again on the next pass.
    pub(super) async fn finish_agent_run(&self, run: &FinishedRun) {
        if let Err(error) = self.auto_finish_agent(run).await {
            eprintln!(
                "team: could not finish {} after its Claude Code agent ended: {error}",
                run.worker_id
            );
            self.agent_watch.retry_finish(run);
        }
    }
}

/// A registered agent worker whose agent still owes the lead something.
fn watched(worker: &WorkerRecord) -> bool {
    worker.claude_agent.is_some()
        && worker.claude_agent_id.is_some()
        && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
}

/// An agent worker at work, its agent registered or still awaited: the
/// watcher keeps running for it, so the hand-back check also reaches a
/// worker whose agent was never launched.
fn keeps_watch(worker: &WorkerRecord) -> bool {
    worker.claude_agent.is_some()
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
/// Returns whether the watcher should keep running, the agent runs that
/// ended, for the runtime to finish, and whether any agent is stalled.
async fn tick(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    signals: &broadcast::Sender<BoardSignal>,
    watch: &AgentWatch,
) -> (bool, Vec<FinishedRun>, bool) {
    let workers: Vec<WorkerRecord> = records
        .lock()
        .await
        .iter()
        .filter(|worker| watched(worker))
        .cloned()
        .collect();
    if workers.is_empty() {
        // Re-check under the task slot, so a registration racing the exit
        // either keeps this task or finds the slot free. An agent worker
        // still awaiting its agent keeps the task too: its hand-back check
        // runs after each pass.
        let mut inner = watch.lock();
        let again = records.try_lock().map(|r| r.iter().any(keeps_watch)).unwrap_or(true);
        if again {
            return (true, Vec::new(), false);
        }
        inner.task = None;
        return (false, Vec::new(), false);
    }
    let mut finished = Vec::new();
    let mut any_stalled = false;
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
            if view.finished {
                // Once per agent run (worker, agent, turn): the runtime
                // finishes the worker, and the lead gets its finished notice.
                // A continued run counts once its transcript grew past where
                // the run started, even if no pass saw it unfinished.
                let new_run = observed.handled_turns != Some(worker.turns);
                let grown = observed.run_floor.is_none_or(|floor| observed.offset > floor);
                if new_run && grown {
                    observed.handled_turns = Some(worker.turns);
                    finished.push(FinishedRun {
                        worker_id: worker.id.clone(),
                        agent_id: view.id.clone(),
                        turns: worker.turns,
                        transcript: view.transcript.clone(),
                    });
                }
            } else if view.stalled {
                any_stalled = true;
                if !observed.stall_noticed {
                    observed.stall_noticed = true;
                    let last = view.last_activity_ms.unwrap_or(observed.registered_ms);
                    let minutes = now.saturating_sub(last) / 60_000;
                    notice = Some((
                        format!("agent-stall:{}:{now}", worker.id),
                        format!(
                            "worker_id={id}\n{id}'s Claude Code agent has written nothing for {minutes} minutes; it may have stalled or stopped. Check it, then continue it with SendMessage, or mark the run failed with team_agent_done.",
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
    (true, finished, any_stalled)
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

/// Whether `worker` runs as a Claude Code agent right now: an agent worker in
/// Working or Fixing, launched or still waiting for its launch.
pub(crate) fn runs_as_agent(worker: &WorkerRecord) -> bool {
    worker.claude_agent.is_some()
        && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
}

/// An agent worker whose agent has not been launched yet: it runs as a
/// Claude Code agent, is Working or Fixing in a checkout, and has no agent
/// registered. The WorktreeCreate hook hands the next isolated agent to it,
/// so at most one may exist at a time (plan 4d920eec decision 4).
pub(super) fn waits_for_agent(worker: &WorkerRecord) -> bool {
    runs_as_agent(worker)
        && worker.claude_agent_id.is_none()
        && !worker.worktree.as_os_str().is_empty()
}

/// The phase of a Claude Code agent worker that `team_status` and the
/// window show (derived, never stored): `awaiting_launch` until its agent is
/// launched, then `running`, `stalled` or `finished` from the agent's
/// transcript while the worker still works. `None` for a worker that is not
/// an agent worker, or one that no longer works.
pub(super) fn agent_phase(worker: &WorkerRecord, view: Option<&AgentView>) -> Option<&'static str> {
    worker.claude_agent.as_ref()?;
    if waits_for_agent(worker) {
        return Some("awaiting_launch");
    }
    if worker.claude_agent_id.is_none()
        || !matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
    {
        return None;
    }
    let view = view?;
    Some(if view.finished {
        "finished"
    } else if view.stalled {
        "stalled"
    } else {
        "running"
    })
}

/// Why an agent worker was handed back to Mnemo.
pub(super) const HANDED_BACK_REASON: &str =
    "Its Claude Code lead was away longer than the hand-back grace period ([routing] agent_hand_back_minutes) while its agent was not launched or had stalled.";

/// Whether the lead has been away long enough for a hand-back (plan
/// ee73bd2f decision 1): `away_since_ms` is when the external lead left
/// (`None` while one is attached), `grace_ms` the grace period. A grace of 0
/// hands back as soon as the lead is gone.
pub(super) fn hand_back_grace_passed(away_since_ms: Option<u64>, now_ms: u64, grace_ms: u64) -> bool {
    away_since_ms.is_some_and(|since| now_ms.saturating_sub(since) >= grace_ms)
}

/// Whether an absent lead's agent worker is due for a hand-back: its agent
/// was never launched, or it stalled. A running agent may still write its
/// transcript, so it stays.
pub(super) fn hand_back_due(worker: &WorkerRecord, view: Option<&AgentView>) -> bool {
    matches!(
        agent_phase(worker, view),
        Some("awaiting_launch") | Some("stalled")
    )
}

/// Make an agent worker an ordinary interrupted Mnemo worker: it no longer
/// runs as a Claude Code agent, and says why. What it was is kept in
/// `handed_back`, so `team_agent_started` can restore it while no Mnemo
/// model has run on it. Record, branch, checkout, commits, findings and
/// review history stay.
pub(super) fn hand_back(worker: &mut WorkerRecord, at_ms: u64) {
    if let Some(agent) = worker.claude_agent.take() {
        worker.handed_back = Some(HandedBack {
            at_ms,
            claude_agent: agent,
            claude_agent_effort: worker.claude_agent_effort.clone(),
            status: worker.status,
        });
    }
    worker.claude_agent_effort = None;
    worker.claude_agent_id = None;
    worker.claude_agent_transcript = None;
    worker.compacting = false;
    worker.status = WorkerStatus::Interrupted;
    worker.error = Some(HANDED_BACK_REASON.to_string());
}

/// Whether a worker's checkout has uncommitted changes: tracked or
/// untracked, ignored files excluded (the pool check's rule). A checkout
/// Git cannot read counts as dirty, which only ever refuses a takeover; a
/// worker without a checkout has nothing to overwrite.
pub(super) async fn checkout_dirty(worktree: &Path) -> bool {
    if worktree.as_os_str().is_empty() || !worktree.is_dir() {
        return false;
    }
    match phase3::git_value(worktree, &["status", "--porcelain", "-uall"]).await {
        Ok(status) => !status.trim().is_empty(),
        Err(_) => true,
    }
}

/// The ways to continue a handed-back worker, as the notices name them.
fn handed_back_recovery(worker_id: &str) -> String {
    format!(
        "To continue it:\n- team_agent_started (worker_id={worker_id}, agent_id=<the agent's id>) reclaims it with an agent you launched for it;\n- team_resume (worker_id={worker_id}, tier=<a tier that runs as a Claude Code agent>) starts a fresh agent on it;\n- team_resume onto a Mnemo tier runs it on a Mnemo model, which is refused while its checkout has uncommitted changes unless you pass keep_changes: true."
    )
}

/// The notice the next lead gets for a handed-back worker: the worker, its
/// checkout, whether that holds uncommitted changes, and the recovery calls.
pub(super) fn handed_back_notice(worker: &WorkerRecord, dirty: bool) -> String {
    let id = &worker.id;
    let changes = if dirty {
        "has uncommitted changes"
    } else {
        "has no uncommitted changes"
    };
    format!(
        "{TEAM_NOTICE_PREFIX} worker_id={id}\n{id} was handed back from Claude Code: {HANDED_BACK_REASON}\nIts checkout {checkout} {changes}.\n{recovery}",
        checkout = worker.worktree.display(),
        recovery = handed_back_recovery(id),
    )
}

/// The refusal (and notice) when a Mnemo model would take over a
/// handed-back worker whose checkout has uncommitted changes (plan ee73bd2f
/// decision 3): a launched agent may still be writing there.
pub(super) fn handed_back_dirty_refusal(worker: &WorkerRecord) -> String {
    let id = &worker.id;
    format!(
        "worker_id={id}\n{id} was handed back from Claude Code and its checkout {checkout} has uncommitted changes, which its Claude Code agent may still be writing: Mnemo does not start a Mnemo model there, so {id} stays interrupted.\n{recovery}",
        checkout = worker.worktree.display(),
        recovery = handed_back_recovery(id),
    )
}

/// A launched agent's transcript found for an agent worker that awaits its
/// launch ([`find_launched_agent`]).
pub(super) struct Launched {
    pub(super) agent_id: String,
    pub(super) path: PathBuf,
    /// How many transcripts matched; the newest is the one named.
    pub(super) matches: usize,
}

/// The most bytes read from a transcript while looking for its first user
/// message.
const FIRST_MESSAGE_BYTES: u64 = 1024 * 1024;

/// Whether `path` exists and is no link or junction.
fn plain_entry(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| !is_link_or_junction(&meta))
}

/// The text of a user transcript entry's message.
fn user_message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Whether a user entry is Claude Code's own bookkeeping rather than the
/// prompt: an `isMeta` entry, or one whose text is a system caveat or a
/// command echo (a `<local-command-caveat>`, `<command-name>` or
/// `<system-reminder>` block).
fn meta_user_entry(value: &Value, text: &str) -> bool {
    if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let text = text.trim_start();
    text.is_empty()
        || text.starts_with("<local-command-caveat>")
        || text.starts_with("<local-command-stdout>")
        || text.starts_with("<command-name>")
        || text.starts_with("<system-reminder>")
        || text.starts_with("Caveat: The messages below were generated by the user")
}

/// The text of a transcript's first real user message (its prompt), skipping
/// meta and caveat entries ([`meta_user_entry`]), read from at most
/// [`FIRST_MESSAGE_BYTES`].
fn first_user_text(path: &Path) -> Option<String> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file.take(FIRST_MESSAGE_BYTES));
    for line in reader.split(b'\n') {
        let Ok(line) = line else {
            return None;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some(message) = value.get("message") else {
            continue;
        };
        let user = message.get("role").and_then(Value::as_str) == Some("user")
            || value.get("type").and_then(Value::as_str) == Some("user");
        if !user {
            continue;
        }
        let text = user_message_text(message);
        if meta_user_entry(&value, &text) {
            continue;
        }
        return Some(text);
    }
    None
}

/// Whether a prompt says the agent runs on `branch`: the phrase
/// `on branch <branch>` (case-insensitive), not followed by more of a
/// branch name. Only this phrase counts (plan ee73bd2f, step 1 audit): a
/// brief routinely names other workers' checkouts and branches ("never
/// touch …"), which must not match.
fn prompt_names_branch(text: &str, branch: &str) -> bool {
    let branch = branch.trim();
    if branch.is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let phrase = format!("on branch {}", branch.to_lowercase());
    let mut from = 0;
    while let Some(found) = text[from..].find(&phrase) {
        let end = from + found + phrase.len();
        let continues = text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '/'));
        if !continues {
            return true;
        }
        from = end;
    }
    false
}

/// Look for the agent the lead launched for `worker` (an agent worker that
/// awaits its launch) before handing it back (plan ee73bd2f decision 2):
/// `~/.claude/projects/<slug>/<session>/subagents/agent-<id>.jsonl` files,
/// reached without links and confined to the projects root as
/// [`find_transcript`] requires, written at or after `since_ms` (the
/// worker's dispatch or its last reroute), whose agent is not in `taken`
/// (registered to another worker), and whose first real user message says
/// it runs `on branch <the worker's branch>` ([`prompt_names_branch`]); a
/// prompt that merely mentions the checkout or the branch does not count.
/// The newest match wins.
pub(super) fn find_launched_agent(
    watch: &AgentWatch,
    worker: &WorkerRecord,
    since_ms: u128,
    taken: &[String],
) -> Option<Launched> {
    let root = watch.projects_root()?;
    if worker.branch.trim().is_empty() {
        return None;
    }
    let mut found: Vec<(u128, String, PathBuf)> = Vec::new();
    for slug in std::fs::read_dir(&root).ok()?.flatten() {
        if !plain_entry(&slug.path()) {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(slug.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let subagents = session.path().join("subagents");
            if !plain_entry(&session.path()) || !plain_entry(&subagents) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&subagents) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let Some(agent_id) = name
                    .to_str()
                    .and_then(|name| name.strip_prefix("agent-"))
                    .and_then(|name| name.strip_suffix(".jsonl"))
                else {
                    continue;
                };
                if !valid_agent_id(agent_id) || taken.iter().any(|id| id == agent_id) {
                    continue;
                }
                let Some(path) = confined(&root, &file.path(), agent_id) else {
                    continue;
                };
                let modified = std::fs::metadata(&path)
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .map_or(0, epoch_ms);
                if modified < since_ms {
                    continue;
                }
                let Some(text) = first_user_text(&path) else {
                    continue;
                };
                if prompt_names_branch(&text, &worker.branch) {
                    found.push((modified, agent_id.to_string(), path));
                }
            }
        }
    }
    let matches = found.len();
    let (_, agent_id, path) = found.into_iter().max_by_key(|(modified, _, _)| *modified)?;
    Some(Launched {
        agent_id,
        path,
        matches,
    })
}

/// The longest agent report quoted in a finished notice, in characters.
const NOTICE_REPORT_CHARS: usize = 4000;

/// An agent's report cut for a notice; `team_status` with detail has it all.
pub(super) fn clip_report(report: &str) -> String {
    if report.chars().count() <= NOTICE_REPORT_CHARS {
        return report.to_string();
    }
    let kept: String = report.chars().take(NOTICE_REPORT_CHARS).collect();
    format!("{kept}\n[cut: team_status with detail shows the whole report]")
}

/// An agent worker as the agent gate sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGateWorker {
    pub id: String,
    /// The settled tier it runs on; empty when none is recorded.
    pub tier: String,
    /// Its checkout path; empty before it has one.
    pub worktree: String,
    /// Its `wt/agent-*` branch.
    pub branch: String,
}

/// The agent workers the agent gate decides from
/// ([`TeamRuntime::agent_gate_view`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentGateView {
    /// The agent worker that waits for its agent to be launched.
    pub waiting: Option<AgentGateWorker>,
    /// Every agent worker in Working or Fixing, the waiting one included.
    pub running: Vec<AgentGateWorker>,
}

/// The most agent workers that run (Working or Fixing) at once (plan
/// 4d920eec decision 5). A constant: there is no setting.
pub(super) const AGENT_WORKER_CAP: usize = 3;

impl TeamRuntime {
    /// What the agent gate decides from, read under one records lock and
    /// nothing else (no snapshot, no transcripts), so the PreToolUse hook gets
    /// its answer well inside its timeout: the agent worker that waits for
    /// its agent to be launched, if one does, and every agent worker that runs
    /// (Working or Fixing, the waiting one included).
    pub async fn agent_gate_view(&self) -> AgentGateView {
        let records = self.records.lock().await;
        let gate_worker = |worker: &WorkerRecord| AgentGateWorker {
            id: worker.id.clone(),
            tier: worker.tier.clone().unwrap_or_default(),
            worktree: worker.worktree.to_string_lossy().into_owned(),
            branch: worker.branch.clone(),
        };
        AgentGateView {
            waiting: records
                .iter()
                .find(|worker| waits_for_agent(worker))
                .map(gate_worker),
            running: records
                .iter()
                .filter(|worker| runs_as_agent(worker))
                .map(gate_worker)
                .collect(),
        }
    }

    /// Why another agent worker may not start now, if it may not (plan
    /// 4d920eec decisions 4-5): one already waits for its agent, so the next
    /// isolated launch could be handed the wrong checkout, or
    /// [`AGENT_WORKER_CAP`] agent workers already run. `except` is the worker
    /// being started or moved, which never counts against itself.
    pub(super) async fn agent_launch_refusal(&self, except: Option<&str>) -> Option<String> {
        let records = self.records.lock().await;
        let others = || {
            records
                .iter()
                .filter(move |worker| Some(worker.id.as_str()) != except)
        };
        if let Some(waiting) = others().find(|worker| waits_for_agent(worker)) {
            return Some(format!(
                "{} is waiting for its agent: launch it (Agent with isolation \"worktree\") before dispatching another agent step.",
                waiting.id
            ));
        }
        let running: Vec<&str> = others()
            .filter(|worker| runs_as_agent(worker))
            .map(|worker| worker.id.as_str())
            .collect();
        (running.len() >= AGENT_WORKER_CAP).then(|| {
            format!(
                "{AGENT_WORKER_CAP} Claude Code agent workers are already running ({}); wait for one to finish (Mnemo finishes it when its agent ends) before starting another.",
                running.join(", ")
            )
        })
    }
}

/// A handed-back worker `team_agent_started` may reclaim: Interrupted, with
/// the hand-back still on record (no Mnemo model has run on it since).
fn reclaimable(worker: &WorkerRecord) -> bool {
    worker.claude_agent.is_none()
        && worker.handed_back.is_some()
        && worker.status == WorkerStatus::Interrupted
}

/// The agent workers `team_agent_started` and `team_progress` accept.
fn agent_worker_in_work(worker: &WorkerRecord, tool: &str) -> Result<(), String> {
    if worker.claude_agent.is_none() {
        return Err(format!(
            "{} does not run as a Claude Code agent (it never did, or a Mnemo model has run on it since it was handed back): {tool} only serves agent workers",
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
        // A handed-back worker no Mnemo model has run on since is reclaimed
        // (plan ee73bd2f decision 4): the lead launched its agent after all.
        // Under the merge lock, so no resume starts a Mnemo session on it
        // meanwhile; no other caller of this holds that lock for such a
        // worker (they register a worker that awaits its agent).
        let reclaim_guard = if reclaimable(&worker) {
            Some(self.merge_lock.lock().await)
        } else {
            agent_worker_in_work(&worker, "team_agent_started")?;
            None
        };
        let search = find_transcript(&self.agent_watch, agent_id, transcript_path);
        let transcript = search.path;
        let mut restored = false;
        {
            let mut records = self.records.lock().await;
            let Some(record) = records.iter_mut().find(|record| record.id == worker_id) else {
                return Err(format!("unknown worker '{worker_id}'"));
            };
            if reclaim_guard.is_some() {
                // Re-checked under the lock: a Mnemo session may have started.
                if !reclaimable(record) {
                    agent_worker_in_work(record, "team_agent_started")?;
                }
                if let Some(handed) = record.handed_back.take() {
                    record.claude_agent = Some(handed.claude_agent);
                    record.claude_agent_effort = handed.claude_agent_effort;
                    record.status = handed.status;
                    record.error = None;
                    restored = true;
                }
            }
            record.claude_agent_id = Some(agent_id.to_string());
            record.claude_agent_transcript = transcript.clone();
            // The launch is activity: an agent whose transcript never shows
            // up stalls ten minutes from now, also across a restart.
            record.last_activity_ms = record.last_activity_ms.max(phase3::now_ms());
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
        let registered = if restored {
            format!(
                "Restored {worker_id} as a Claude Code agent worker and registered agent {agent_id}"
            )
        } else {
            format!("Registered Claude Code agent {agent_id} for {worker_id}")
        };
        let text = match &transcript {
            Some(path) => format!("{ignored}{registered}; Mnemo follows its transcript {path}."),
            None => format!(
                "{ignored}{registered}; its transcript was not found yet, so Mnemo keeps looking for it under ~/.claude/projects."
            ),
        };
        drop(reclaim_guard);
        if restored {
            let _ = self.signals.send(BoardSignal::Changed);
        }
        let worker = self.worker(worker_id).await?;
        Ok((worker, text))
    }

    /// The worktree Claude Code's WorktreeCreate hook hands an agent launched
    /// with isolation "worktree" (plan 4d920eec decision 3): the pool
    /// checkout of the one agent worker that waits for its agent (Working or
    /// Fixing, with no agent registered yet), with the agent registered as
    /// `team_agent_started` would. `name` is the hook's worktree name,
    /// `agent-<agent id>`; `transcript_path` is the lead session's
    /// transcript, beside whose folder Claude Code writes the agent's
    /// (`<session>/subagents/agent-<id>.jsonl`).
    ///
    /// `Ok(Some((worktree, worker_id)))` hands out that checkout; `Ok(None)`
    /// means no agent worker waits, so the agent is not Mnemo work and the
    /// hook makes an ordinary worktree. More than one waiting worker is an
    /// error: the launch could belong to either, and dispatch never lets it
    /// happen.
    pub async fn claim_agent_checkout(
        &self,
        name: &str,
        transcript_path: Option<&str>,
    ) -> Result<Option<(PathBuf, String)>, String> {
        let _guard = self.merge_lock.lock().await;
        let name = name.trim();
        let agent_id = name.strip_prefix("agent-").unwrap_or(name);
        if !valid_agent_id(agent_id) {
            return Ok(None);
        }
        let waiting: Vec<String> = self
            .records
            .lock()
            .await
            .iter()
            .filter(|worker| waits_for_agent(worker))
            .map(|worker| worker.id.clone())
            .collect();
        let worker_id = match waiting.as_slice() {
            [] => return Ok(None),
            [one] => one.clone(),
            many => {
                return Err(format!(
                    "{} agent workers wait for their agent ({}); Mnemo cannot tell which one this agent runs",
                    many.len(),
                    many.join(", ")
                ));
            }
        };
        let guess = transcript_path.and_then(|path| {
            let path = Path::new(path);
            Some(
                path.parent()?
                    .join(path.file_stem()?)
                    .join("subagents")
                    .join(format!("agent-{agent_id}.jsonl")),
            )
        });
        let guess = guess.map(|path| path.to_string_lossy().into_owned());
        self.agent_started(&worker_id, agent_id, guess.as_deref())
            .await?;
        let worker = self.worker(&worker_id).await?;
        Ok(Some((worker.worktree, worker_id)))
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

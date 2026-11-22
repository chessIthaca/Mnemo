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
    /// Bytes consumed: the end of the last complete line read.
    offset: u64,
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
            let Ok(meta) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
                newest = Some((modified, candidate));
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// Find an agent's transcript: the file the lead named when it exists, else
/// the documented location.
pub(super) fn find_transcript(
    watch: &AgentWatch,
    agent_id: &str,
    given: Option<&str>,
) -> Option<String> {
    let given = given
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(path) = given {
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    let root = watch.projects_root()?;
    locate(&root, agent_id).map(|path| path.to_string_lossy().into_owned())
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

/// Read what the file gained since the last look.
fn read_new(observed: &mut Observed, path: &Path, length: u64) {
    use std::io::{Read, Seek, SeekFrom};
    if length < observed.offset {
        // The file shrank: it is a different file now.
        observed.offset = 0;
        observed.tool_calls = 0;
        observed.tokens = 0;
    }
    if length == observed.offset {
        return;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    if file.seek(SeekFrom::Start(observed.offset)).is_err() {
        return;
    }
    let mut bytes = Vec::new();
    if file.take(length - observed.offset).read_to_end(&mut bytes).is_err() {
        return;
    }
    // Only complete lines count: the writer may be mid-line.
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return;
    };
    for line in bytes[..end].split(|byte| *byte == b'\n') {
        if !line.is_empty() {
            read_line(observed, line);
        }
    }
    observed.offset += end as u64 + 1;
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
    let recorded = worker
        .claude_agent_transcript
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let mut found = None;
    let path = match recorded {
        Some(path) => Some(path),
        None => {
            let path = find_transcript(watch, &agent_id, None).map(PathBuf::from);
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
        observed.offset = 0;
        observed.tool_calls = 0;
        observed.tokens = 0;
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
                        format!("agent-finished:{}:{}", worker.id, view.id),
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
        let transcript = find_transcript(&self.agent_watch, agent_id, transcript_path);
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
        let text = match &transcript {
            Some(path) => format!(
                "Registered Claude Code agent {agent_id} for {worker_id}; Mnemo follows its transcript {path}."
            ),
            None => format!(
                "Registered Claude Code agent {agent_id} for {worker_id}; its transcript was not found yet, so Mnemo keeps looking for it under ~/.claude/projects."
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
        let at_ms = phase3::now_ms();
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
            key: format!("progress:{worker_id}:{at_ms}:{}", note.len()),
            text: format!("worker_id={worker_id}\n{worker_id} progress: {note}"),
        });
        self.worker(worker_id).await
    }
}

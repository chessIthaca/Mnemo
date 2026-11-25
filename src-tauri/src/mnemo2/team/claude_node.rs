// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Following a remote agent worker's Claude Code session from its own node.
//!
//! A Claude-agent-tier worker placed on a paired host (plan 5e64ccd5) runs as
//! a Claude Code session the LEAD starts over SSH in the node's warm pool
//! checkout; Mnemo never launches `claude` itself. That session writes an
//! ordinary Claude Code transcript, `<projects>/<slug>/<session>.jsonl` under
//! the node's own Claude projects folder, so the node can answer what the
//! local agent watch reads from a local transcript: when the session last did
//! something, how many tool calls and tokens it used, and whether its turn
//! ended. [`CLAUDE_WATCH`] is that answer; [`CLAUDE_STATUS`] says where the
//! folder is, so the hub can refuse a host whose sessions it could not follow.
//!
//! The session is found by its prompt, not by a launch token: the prompt's
//! first user message starts `Mnemo worker <id> on branch <branch>` (plan
//! 5e64ccd5, decision F2), which names the worker and keeps a node's unrelated
//! sessions out. The watch only reads the projects folder — it spawns no
//! process, touches no credentials file, and every transcript it accepts must
//! resolve inside that folder without links, as the local agent watch already
//! requires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use mnemo::provider::Role;
use mnemo::remote::node::{checkout_path, NodeCallCtx, NodeExtension};
use mnemo::remote::RemoteError;

use super::agent_log;
use super::agent_watch::{
    first_user_text, plain_entry, prompt_names_branch, read_new, valid_agent_id, Observed,
};
use super::proof::ProofNodeExtension;

/// The node method that reports the Claude projects folder it reads.
pub const CLAUDE_STATUS: &str = "claude.status";

/// The node method that reports one Claude Code session working in one of the
/// node's checkouts.
pub const CLAUDE_WATCH: &str = "claude.watch";

/// How long an assistant record without `end_turn` may stay unchanged before
/// its turn counts as ended anyway: a session that stopped or was killed
/// mid-turn still finishes its worker.
const QUIET_MS: u64 = 120_000;

/// How much of the last assistant text a finished turn reports.
const LAST_TEXT_CHARS: usize = 16_000;

/// `claude.status`'s answer: where this node's Claude Code transcripts live.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeStatusAnswer {
    /// The projects folder, `None` when the node has no home to name one in.
    pub projects_dir: Option<String>,
    /// Whether that folder exists.
    pub exists: bool,
}

/// `claude.watch`'s request: the session one remote agent worker runs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ClaudeWatch {
    /// The worker's checkout name under the node's `checkouts/`.
    pub checkout: String,
    /// The worker the session runs for.
    pub worker_id: String,
    /// The worker's `wt/agent-*` branch.
    pub branch: String,
    /// The dispatch or last-reroute time: a transcript written before it is
    /// not this worker's.
    pub since_ms: u64,
    /// The transcript's file stem, once the hub knows it: no search then.
    #[serde(default)]
    pub session: Option<String>,
}

/// `claude.watch`'s answer. Every field is always present: the hub records the
/// session, its activity and its turn from one answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeWatchAnswer {
    /// The transcript's file stem (the Claude Code session id), once found.
    pub session: Option<String>,
    /// The transcript's path on the node.
    pub transcript: Option<String>,
    /// The transcript's length in bytes.
    pub size: u64,
    /// When the transcript last changed, in Unix milliseconds.
    pub last_activity_ms: Option<u64>,
    /// Tool calls the session made, counted from the transcript.
    pub tool_calls: u64,
    /// Tokens the session used, counted from the transcript.
    pub tokens: u64,
    /// Whether the session's last word was the end of an assistant turn.
    pub turn_ended: bool,
    /// The turn's last assistant text; present only when it ended.
    pub last_text: Option<String>,
    /// How many transcripts matched the session's prompt.
    pub matches: u32,
}

/// The Claude Code projects folder this machine's sessions write to: the
/// `projects` folder under `$CLAUDE_CONFIG_DIR` when set, else
/// `$USERPROFILE/.claude/projects` or `$HOME/.claude/projects`. The environment
/// says which, never the build.
pub fn claude_projects_dir() -> Option<PathBuf> {
    if let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(config).join("projects"));
    }
    let home = std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|home| !home.is_empty()))?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

/// The node's look at Claude Code sessions ([`CLAUDE_STATUS`],
/// [`CLAUDE_WATCH`]).
#[derive(Default)]
pub struct ClaudeNodeExtension {
    /// The projects folder to read; `None` uses [`claude_projects_dir`].
    projects_root: Option<PathBuf>,
}

impl ClaudeNodeExtension {
    /// The extension reading `root` instead of this machine's folder. Tests
    /// and the hub's node tests use it.
    pub fn with_projects_root(root: PathBuf) -> Self {
        Self {
            projects_root: Some(root),
        }
    }

    /// The projects folder to read: the configured one, else this machine's.
    fn root(&self) -> Option<PathBuf> {
        self.projects_root.clone().or_else(claude_projects_dir)
    }
}

#[async_trait]
impl NodeExtension for ClaudeNodeExtension {
    /// Answer `claude.status` and `claude.watch`; other methods are not this
    /// extension's.
    async fn request(
        &self,
        method: &str,
        params: Value,
        ctx: &NodeCallCtx,
    ) -> Option<Result<Value, RemoteError>> {
        match method {
            CLAUDE_STATUS => Some(status(self.root())),
            CLAUDE_WATCH => Some(watch(self.root().as_deref(), params, ctx)),
            _ => None,
        }
    }
}

/// The extension the `mnemo2-node` binary serves: the proof runs a remote
/// worker needs on this host ([`ProofNodeExtension`]) and the Claude session
/// watch a remote agent worker is followed with.
#[derive(Default)]
pub struct MnemoNodeExtension {
    /// The Claude session watch.
    pub claude: ClaudeNodeExtension,
}

#[async_trait]
impl NodeExtension for MnemoNodeExtension {
    /// The proof runs first, exactly as a node without this extension served
    /// them; the Claude watch answers everything else it owns.
    async fn request(
        &self,
        method: &str,
        params: Value,
        ctx: &NodeCallCtx,
    ) -> Option<Result<Value, RemoteError>> {
        if let Some(answer) = ProofNodeExtension.request(method, params.clone(), ctx).await {
            return Some(answer);
        }
        self.claude.request(method, params, ctx).await
    }
}

/// `claude.status`'s answer for `root`.
fn status(root: Option<PathBuf>) -> Result<Value, RemoteError> {
    let answer = ClaudeStatusAnswer {
        exists: root.as_deref().is_some_and(Path::is_dir),
        projects_dir: root.map(|root| root.to_string_lossy().into_owned()),
    };
    serde_json::to_value(answer)
        .map_err(|error| RemoteError::internal(format!("{CLAUDE_STATUS}: {error}")))
}

/// Answer one `claude.watch` request: resolve the worker's checkout on this
/// node and read the session's transcript under `root`.
fn watch(root: Option<&Path>, params: Value, ctx: &NodeCallCtx) -> Result<Value, RemoteError> {
    let request: ClaudeWatch = serde_json::from_value(params)
        .map_err(|error| RemoteError::bad_params(format!("{CLAUDE_WATCH}: {error}")))?;
    let checkout = checkout_path(&ctx.home, &request.checkout)?;
    let answer = match root {
        Some(root) => watch_at(root, &checkout, &request, now_ms()),
        None => ClaudeWatchAnswer::default(),
    };
    serde_json::to_value(answer)
        .map_err(|error| RemoteError::internal(format!("{CLAUDE_WATCH}: {error}")))
}

/// The watch answer for one checkout, from the transcripts under `root`.
fn watch_at(
    root: &Path,
    checkout: &Path,
    request: &ClaudeWatch,
    now_ms: u64,
) -> ClaudeWatchAnswer {
    let named = request
        .session
        .as_deref()
        .map(str::trim)
        .filter(|session| !session.is_empty());
    let (path, matches) = match named {
        // A session the hub already accepted stays the worker's however old
        // its transcript is (a reroute moves `since_ms` past an idle file), so
        // neither `since_ms` nor the prompt marker is asked for again; the
        // stem, the confinement and the plain check still are.
        Some(session) => match find_session(root, checkout, session) {
            Some(path) => (Some(path), 1),
            None => (None, 0),
        },
        None => find_marked(root, checkout, request),
    };
    let Some(path) = path else {
        return ClaudeWatchAnswer {
            matches,
            ..ClaudeWatchAnswer::default()
        };
    };
    match file_answer(&path, now_ms) {
        Some(mut answer) => {
            answer.matches = matches;
            answer
        }
        None => ClaudeWatchAnswer {
            matches,
            ..ClaudeWatchAnswer::default()
        },
    }
}

/// Read one transcript's new lines and answer what it says about the session.
fn file_answer(path: &Path, now_ms: u64) -> Option<ClaudeWatchAnswer> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    let last_activity_ms = meta.modified().ok().map(modified_ms);
    let mut cache = cache().lock().unwrap_or_else(|poison| poison.into_inner());
    let observed = cache.entry(path.to_path_buf()).or_default();
    read_new(observed, path, size);
    let turn_ended = observed.last_role == Some(Role::Assistant)
        && !observed.last_tool_use
        && (observed.last_stop.as_deref() == Some("end_turn")
            || last_activity_ms
                .is_some_and(|modified| now_ms.saturating_sub(modified) >= QUIET_MS));
    let (tool_calls, tokens) = (observed.tool_calls, observed.tokens);
    drop(cache);
    let last_text = if turn_ended {
        agent_log::last_assistant_text_of(path).map(cut_last_text)
    } else {
        None
    };
    Some(ClaudeWatchAnswer {
        session: path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string),
        transcript: Some(path.to_string_lossy().into_owned()),
        size,
        last_activity_ms,
        tool_calls,
        tokens,
        turn_ended,
        last_text,
        matches: 0,
    })
}

/// Every watched transcript's reading state, keyed by the transcript's path.
fn cache() -> &'static Mutex<HashMap<PathBuf, Observed>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Observed>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The report a finished turn hands the hub: at most [`LAST_TEXT_CHARS`]
/// characters.
fn cut_last_text(text: String) -> String {
    if text.chars().count() <= LAST_TEXT_CHARS {
        return text;
    }
    text.chars().take(LAST_TEXT_CHARS).collect()
}

/// `time` as Unix milliseconds.
fn modified_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// The current time in Unix milliseconds.
fn now_ms() -> u64 {
    modified_ms(SystemTime::now())
}

/// The projects folder's name for one checkout path: every character outside
/// `[A-Za-z0-9]` becomes `-`, as Claude Code names its folders (`D:\Mnemo`
/// becomes `D--Mnemo`).
fn slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript of the session the hub already named: in the checkout's own
/// projects folder first, then in any other one (a checkout the node spells
/// differently than the path Claude Code slugged).
fn find_session(root: &Path, checkout: &Path, session: &str) -> Option<PathBuf> {
    if !valid_agent_id(session) {
        return None;
    }
    let file = format!("{session}.jsonl");
    let own = root.join(slug(checkout)).join(&file);
    if let Some(path) = confined_session(root, &own, session) {
        return Some(path);
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .find_map(|folder| confined_session(root, &folder.path().join(&file), session))
}

/// The newest session looking for `request`, and how many matched: the
/// transcript's first real user message carries the worker marker and names
/// the worker's branch.
fn find_marked(root: &Path, checkout: &Path, request: &ClaudeWatch) -> (Option<PathBuf>, u32) {
    let marker = format!("Mnemo worker {} ", request.worker_id);
    let mut matched: Vec<(u64, PathBuf)> = Vec::new();
    for candidate in candidates(root, checkout) {
        let Some(stem) = candidate.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(path) = confined_session(root, &candidate, stem) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let Some(modified) = meta.modified().ok().map(modified_ms) else {
            continue;
        };
        if modified < request.since_ms {
            continue;
        }
        let Some(text) = first_user_text(&path) else {
            continue;
        };
        if !text.contains(&marker) || !prompt_names_branch(&text, &request.branch) {
            continue;
        }
        matched.push((modified, path));
    }
    let matches = matched.len().min(u32::MAX as usize) as u32;
    let newest = matched
        .into_iter()
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path);
    (newest, matches)
}

/// The candidate transcripts for a worker whose session is not known: the
/// top-level `*.jsonl` files of the checkout's own projects folder, or — when
/// that folder holds none — of every projects folder, since the checkout may
/// be spelled differently than its folder is named.
fn candidates(root: &Path, checkout: &Path) -> Vec<PathBuf> {
    let files = session_files(&root.join(slug(checkout)));
    if !files.is_empty() {
        return files;
    }
    let Ok(folders) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    folders
        .flatten()
        .flat_map(|folder| session_files(&folder.path()))
        .collect()
}

/// The top-level `*.jsonl` files of one projects folder: sessions only, never
/// a nested `subagents/` transcript.
fn session_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect()
}

/// Whether `path` is exactly `<session>.jsonl`, an existing file whose
/// canonical form lies inside `root` and whose own components below the root
/// are no links or junctions.
fn confined_session(root: &Path, path: &Path, session: &str) -> Option<PathBuf> {
    if !valid_agent_id(session) {
        return None;
    }
    let expected = format!("{session}.jsonl");
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
    // Walk the given spelling upward from the file to the projects root: the
    // file and every folder strictly below the root must not be a link, and
    // the walk must reach the root.
    let mut current = path;
    loop {
        if current == root {
            return Some(canonical);
        }
        if !plain_entry(current) {
            return None;
        }
        if std::fs::canonicalize(current).ok()? == canonical_root {
            // Another spelling of the root itself.
            return Some(canonical);
        }
        current = current.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    const WORKER: &str = "abcd1234";
    const BRANCH: &str = "wt/agent-abcd1234";

    /// The checkout path a watch names; it never has to exist.
    fn checkout() -> PathBuf {
        PathBuf::from(r"C:\node\checkouts\abcd1234")
    }

    /// The request the hub sends for the test worker.
    fn request() -> ClaudeWatch {
        ClaudeWatch {
            checkout: "abcd1234".to_string(),
            worker_id: WORKER.to_string(),
            branch: BRANCH.to_string(),
            since_ms: 0,
            session: None,
        }
    }

    fn user_record(text: &str) -> String {
        json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
    }

    /// The session's first user record: the marker and the branch the node
    /// finds the worker by.
    fn prompt_record() -> String {
        user_record(&format!(
            "Mnemo worker {WORKER} on branch {BRANCH}\n\nDo the thing."
        ))
    }

    fn assistant_record(content: Value, stop: Option<&str>) -> String {
        let mut message = json!({"role": "assistant", "content": content});
        if let Some(stop) = stop {
            message["stop_reason"] = json!(stop);
        }
        json!({"type": "assistant", "message": message}).to_string()
    }

    fn text_block(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    fn tool_use_block() -> Value {
        json!({"type": "tool_use", "id": "call-1", "name": "Read", "input": {}})
    }

    /// Write one `session` transcript into `folder` under the projects root.
    fn write_transcript(root: &Path, folder: &str, session: &str, body: &str) -> PathBuf {
        let folder = root.join(folder);
        std::fs::create_dir_all(&folder).expect("the projects folder");
        let path = folder.join(format!("{session}.jsonl"));
        std::fs::write(&path, body).expect("the transcript");
        path
    }

    /// Back-date a transcript: it was last written `by` before now.
    fn age(path: &Path, by: Duration) {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("the transcript");
        file.set_modified(SystemTime::now() - by)
            .expect("set the transcript's time");
    }

    /// The projects folder's name replaces every character outside
    /// `[A-Za-z0-9]` with `-`, Claude Code's own naming.
    #[test]
    fn slug_replaces_every_non_alphanumeric() {
        assert_eq!(slug(Path::new(r"D:\Mnemo")), "D--Mnemo");
        assert_eq!(
            slug(Path::new(r"C:\Mnemo\.worktrees\agent-1")),
            "C--Mnemo--worktrees-agent-1"
        );
        assert_eq!(
            slug(Path::new("/Users/me/My Project")),
            "-Users-me-My-Project"
        );
    }

    /// A session in the checkout's own projects folder is found by its prompt
    /// marker, and its ended turn is answered with its last text.
    #[test]
    fn watch_finds_the_session_by_marker_in_the_slug_dir() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let body = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([text_block("Done.")]), Some("end_turn"))
        );
        let path = write_transcript(projects.path(), &slug(&checkout), "session-a", &body);
        // The answer names the transcript's canonical path, as the local
        // agent watch does.
        let canonical = std::fs::canonicalize(&path).expect("the canonical transcript");
        let answer = watch_at(projects.path(), &checkout, &request(), now_ms());
        assert_eq!(answer.session.as_deref(), Some("session-a"));
        assert_eq!(
            answer.transcript.as_deref(),
            Some(&*canonical.to_string_lossy())
        );
        assert_eq!(answer.matches, 1);
        assert!(answer.turn_ended);
        assert_eq!(answer.last_text.as_deref(), Some("Done."));
    }

    /// A transcript written before the dispatch and one without the worker's
    /// marker are not this worker's session.
    #[test]
    fn watch_ignores_older_and_unmarked_sessions() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let folder = slug(&checkout);
        let marked = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([text_block("Old.")]), Some("end_turn"))
        );
        let old = write_transcript(projects.path(), &folder, "session-old", &marked);
        age(&old, Duration::from_secs(600));
        let unmarked = format!(
            "{}\n{}\n",
            user_record("some other session"),
            assistant_record(json!([text_block("Hi.")]), Some("end_turn"))
        );
        write_transcript(projects.path(), &folder, "session-unmarked", &unmarked);
        let now = now_ms();
        let request = ClaudeWatch {
            since_ms: now - 1_000,
            ..request()
        };
        let answer = watch_at(projects.path(), &checkout, &request, now);
        assert_eq!(answer.session, None);
        assert_eq!(answer.transcript, None);
        assert_eq!(answer.matches, 0);
    }

    /// A subagent transcript sits one level deeper than a session and is never
    /// this watch's.
    #[test]
    fn watch_ignores_subagent_files() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let body = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([text_block("Done.")]), Some("end_turn"))
        );
        write_transcript(
            projects.path(),
            &format!("{}/sessions", slug(&checkout)),
            "agent-abcd1234",
            &body,
        );
        let answer = watch_at(projects.path(), &checkout, &request(), now_ms());
        assert_eq!(answer.session, None);
        assert_eq!(answer.matches, 0);
    }

    /// An assistant record without `end_turn` ends its turn only once the
    /// transcript stayed unchanged through the quiet window.
    #[test]
    fn turn_end_needs_end_turn_or_quiet() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let folder = slug(&checkout);
        let body = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([text_block("Working.")]), None)
        );
        write_transcript(projects.path(), &folder, "session-fresh", &body);
        let quiet = write_transcript(projects.path(), &folder, "session-quiet", &body);
        age(&quiet, Duration::from_secs(QUIET_MS / 1000 + 60));
        let now = now_ms();
        let fresh = ClaudeWatch {
            session: Some("session-fresh".to_string()),
            ..request()
        };
        assert!(
            !watch_at(projects.path(), &checkout, &fresh, now).turn_ended,
            "a fresh assistant record without end_turn is still working"
        );
        let quiet = ClaudeWatch {
            session: Some("session-quiet".to_string()),
            ..request()
        };
        assert!(
            watch_at(projects.path(), &checkout, &quiet, now).turn_ended,
            "silence past the quiet window ends the turn"
        );
    }

    /// A session whose last word is a tool call is still working, however long
    /// its transcript has been quiet.
    #[test]
    fn tool_use_last_is_not_turn_end() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let body = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([tool_use_block()]), Some("tool_use"))
        );
        let path = write_transcript(projects.path(), &slug(&checkout), "session-tool", &body);
        age(&path, Duration::from_secs(QUIET_MS / 1000 + 60));
        let request = ClaudeWatch {
            session: Some("session-tool".to_string()),
            ..request()
        };
        let answer = watch_at(projects.path(), &checkout, &request, now_ms());
        assert!(!answer.turn_ended);
        assert_eq!(answer.tool_calls, 1);
        assert_eq!(answer.last_text, None);
    }

    /// A continued session reports each of its turns once: the previous turn's
    /// `end_turn` does not end the new one, and a null `stop_reason` after the
    /// new user line does not either.
    #[test]
    fn continued_session_turn_runs_again() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let first = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([text_block("First answer.")]), Some("end_turn"))
        );
        let path = write_transcript(projects.path(), &slug(&checkout), "session-turn", &first);
        let request = ClaudeWatch {
            session: Some("session-turn".to_string()),
            ..request()
        };
        let now = now_ms();
        let answer = watch_at(projects.path(), &checkout, &request, now);
        assert!(answer.turn_ended);
        assert_eq!(answer.last_text.as_deref(), Some("First answer."));
        // The lead's next message, then an assistant record whose stop_reason
        // is still null: the continued turn runs.
        let mut body = std::fs::read_to_string(&path).expect("the transcript");
        body.push_str(&format!(
            "{}\n{}\n",
            user_record("Keep going."),
            assistant_record(json!([text_block("Working again.")]), None)
        ));
        std::fs::write(&path, &body).expect("the continued transcript");
        let answer = watch_at(projects.path(), &checkout, &request, now);
        assert!(
            !answer.turn_ended,
            "no end_turn and a fresh write: the continued turn still runs"
        );
        // Its real end.
        let mut body = std::fs::read_to_string(&path).expect("the transcript");
        body.push_str(&format!(
            "{}\n",
            assistant_record(json!([text_block("Second answer.")]), Some("end_turn"))
        ));
        std::fs::write(&path, &body).expect("the ended transcript");
        let answer = watch_at(projects.path(), &checkout, &request, now);
        assert!(answer.turn_ended);
        assert_eq!(answer.last_text.as_deref(), Some("Second answer."));
    }

    /// CRLF transcripts count and end like LF ones: a trailing `\r` is JSON
    /// whitespace.
    #[test]
    fn crlf_lines_count() {
        let projects = tempfile::tempdir().expect("a projects root");
        let checkout = checkout();
        let with_usage = json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [tool_use_block()],
                "usage": {"input_tokens": 10, "output_tokens": 5},
                "stop_reason": "tool_use"
            }
        })
        .to_string();
        let body = [
            prompt_record(),
            with_usage,
            assistant_record(json!([text_block("Done.")]), Some("end_turn")),
        ]
        .join("\r\n")
            + "\r\n";
        write_transcript(projects.path(), &slug(&checkout), "session-crlf", &body);
        let request = ClaudeWatch {
            session: Some("session-crlf".to_string()),
            ..request()
        };
        let answer = watch_at(projects.path(), &checkout, &request, now_ms());
        assert_eq!(answer.tool_calls, 1);
        assert_eq!(answer.tokens, 15);
        assert!(answer.turn_ended);
        assert_eq!(answer.last_text.as_deref(), Some("Done."));
    }

    /// The extension over the real link: `claude.status` names the folder it
    /// reads and `claude.watch` finds the session in the node's checkout.
    #[tokio::test(flavor = "multi_thread")]
    async fn watch_over_link_uses_extension() {
        use std::sync::Arc;

        use crate::mnemo2::team::check_run::remote_testkit::Rig;

        let projects = tempfile::tempdir().expect("a projects root");
        let extension = Arc::new(MnemoNodeExtension {
            claude: ClaudeNodeExtension::with_projects_root(projects.path().to_path_buf()),
        });
        let rig = Rig::start_with_extension(extension).await;
        let (record, remote, _tip) = rig.worker_on_node("abcd1234", false).await;
        let body = format!(
            "{}\n{}\n",
            prompt_record(),
            assistant_record(json!([text_block("Done on the node.")]), Some("end_turn"))
        );
        write_transcript(projects.path(), &slug(Path::new(&remote.path)), "session-link", &body);
        let status = rig
            .call_node(&remote, CLAUDE_STATUS, json!({}), Duration::from_secs(30))
            .await
            .expect("claude.status");
        assert_eq!(
            status["projects_dir"],
            json!(projects.path().to_string_lossy())
        );
        assert_eq!(status["exists"], json!(true));
        let answer = rig
            .call_node(
                &remote,
                CLAUDE_WATCH,
                json!({
                    "checkout": remote.checkout,
                    "worker_id": record.id,
                    "branch": record.branch,
                    "since_ms": 0,
                }),
                Duration::from_secs(30),
            )
            .await
            .expect("claude.watch");
        let answer: ClaudeWatchAnswer =
            serde_json::from_value(answer).expect("the watch answer");
        assert_eq!(answer.session.as_deref(), Some("session-link"));
        assert_eq!(answer.matches, 1);
        assert!(answer.turn_ended);
        assert_eq!(answer.last_text.as_deref(), Some("Done on the node."));
    }
}

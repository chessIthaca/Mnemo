// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Edit attempts and how they failed, read back from the persisted worker
//! transcripts (plan f89fd63d step 5).
//!
//! The Quality tab already answers whether merged work held up; this module adds
//! the tool-level half: how many edit calls each model and each worker made, how
//! many of them failed, and why — misshapen arguments, a read receipt the file
//! had moved away from, a remembered `old_string` that no longer matched, a
//! stale-read interception, or some other tool error.
//!
//! Nothing new is recorded at run time. Every tool call and its result already
//! live in `.coding/mnemo2/transcripts/<worker_id>.json`, so the numbers cover
//! past workers and survive a restart. A worker's model comes from the same
//! routing-log outcome rows the quality report counts; a worker whose transcript
//! is missing or unreadable is skipped and counted, never fatal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::Serialize;

/// The state-dir folder holding one transcript per worker.
const TRANSCRIPTS_DIR: &str = "transcripts";

/// The worker tools that edit files: every call of one is an edit attempt.
const EDIT_TOOLS: [&str; 4] = ["file_edit", "multi_edit", "file_write", "file_append"];

/// The path an edit whose arguments never parsed is filed under.
const UNPARSED_PATH: &str = "(unparsed)";

/// The stale-read gate's marker (see `src/agent/dispatch.rs`).
const BLOCKED_MARKER: &str = "EDIT INTERCEPTED";

/// Argument-shape markers: the call never ran because its arguments were
/// misshapen (step 4 of this plan adds the first of them).
const SHAPE_MARKERS: [&str; 4] = [
    "must be a JSON array, but it arrived as a string",
    "failed because an argument",
    "failed because its arguments",
    "failed because parameter",
];

/// Read-receipt markers: the cites did not line up with the read they name.
const RECEIPT_DRIFT_MARKERS: [&str; 4] = [
    "changed since read '",
    "unknown read receipt",
    "rewritten by an edit since that read",
    "reach past the file's current end",
];

/// `old_string` markers: the edit ran but found no anchor.
const MISMATCH_MARKERS: [&str; 2] = ["old_string not found", "near-miss:"];

/// How many files the top-files list keeps.
const MAX_TOP_FILES: usize = 10;

/// Why one edit-tool call failed, or that it succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditOutcome {
    /// The call succeeded.
    Ok,
    /// The call never ran: its arguments arrived misshapen.
    Shape,
    /// A read receipt no longer matched the file, or was unknown.
    ReceiptDrift,
    /// The `old_string` did not match the file's current text.
    Mismatch,
    /// The stale-read gate intercepted the call.
    Blocked,
    /// Any other tool error.
    Other,
}

/// The cause of one edit-tool result, in the plan's fixed order: a stale-read
/// interception first, then a misshapen argument, a drifted read receipt, a
/// non-matching `old_string`, another tool error, and finally success.
fn classify(content: &str) -> EditOutcome {
    let has = |markers: &[&str]| markers.iter().any(|marker| content.contains(marker));
    if content.contains(BLOCKED_MARKER) {
        EditOutcome::Blocked
    } else if has(&SHAPE_MARKERS) {
        EditOutcome::Shape
    } else if has(&RECEIPT_DRIFT_MARKERS) {
        EditOutcome::ReceiptDrift
    } else if has(&MISMATCH_MARKERS) {
        EditOutcome::Mismatch
    } else if content.starts_with("[tool error]") {
        EditOutcome::Other
    } else {
        EditOutcome::Ok
    }
}

/// One counted call, before aggregation: its cause and the files it named.
#[derive(Clone, Debug)]
struct CountedCall {
    /// How the call ended.
    outcome: EditOutcome,
    /// The files it named; `(unparsed)` when the arguments did not parse.
    paths: Vec<String>,
}

/// One model's or one worker's edit attempts, by cause.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcomeRow {
    /// The model spec string, or the worker id.
    pub key: String,
    /// Edit-tool calls counted.
    pub attempts: u32,
    /// Calls that succeeded.
    pub ok: u32,
    /// Calls whose arguments arrived misshapen.
    pub shape: u32,
    /// Calls refused because the cited lines had drifted from their read.
    pub receipt_drift: u32,
    /// Calls whose `old_string` did not match.
    pub mismatch: u32,
    /// Calls the stale-read gate intercepted.
    pub blocked: u32,
    /// Other tool errors.
    pub other: u32,
}

impl EditOutcomeRow {
    /// A row keyed by `key`, with every count at zero.
    fn new(key: &str) -> Self {
        Self {
            key: key.to_string(),
            ..Self::default()
        }
    }

    /// Count one call that ended as `outcome`.
    fn add(&mut self, outcome: EditOutcome) {
        self.attempts += 1;
        match outcome {
            EditOutcome::Ok => self.ok += 1,
            EditOutcome::Shape => self.shape += 1,
            EditOutcome::ReceiptDrift => self.receipt_drift += 1,
            EditOutcome::Mismatch => self.mismatch += 1,
            EditOutcome::Blocked => self.blocked += 1,
            EditOutcome::Other => self.other += 1,
        }
    }

    /// The calls that failed: every attempt but a success.
    pub fn failures(&self) -> u32 {
        self.attempts.saturating_sub(self.ok)
    }
}

/// One file's edit attempts, by cause — a row of the top-files list.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditFileRow {
    /// The path as the call's arguments named it.
    pub path: String,
    /// Calls counted for this file (a successful `multi_edit` counts once per
    /// file it names).
    pub attempts: u32,
    /// Calls counted for this file that failed.
    pub failures: u32,
    /// Failed calls whose arguments arrived misshapen.
    pub shape: u32,
    /// Failed calls whose read receipt had drifted.
    pub receipt_drift: u32,
    /// Failed calls whose `old_string` did not match.
    pub mismatch: u32,
    /// Failed calls the stale-read gate intercepted.
    pub blocked: u32,
    /// Failed calls with another tool error.
    pub other: u32,
}

impl EditFileRow {
    /// A row for `path`, with every count at zero.
    fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            ..Self::default()
        }
    }

    /// Count one call that ended as `outcome`.
    fn add(&mut self, outcome: EditOutcome) {
        self.attempts += 1;
        match outcome {
            EditOutcome::Ok => {}
            EditOutcome::Shape => {
                self.failures += 1;
                self.shape += 1;
            }
            EditOutcome::ReceiptDrift => {
                self.failures += 1;
                self.receipt_drift += 1;
            }
            EditOutcome::Mismatch => {
                self.failures += 1;
                self.mismatch += 1;
            }
            EditOutcome::Blocked => {
                self.failures += 1;
                self.blocked += 1;
            }
            EditOutcome::Other => {
                self.failures += 1;
                self.other += 1;
            }
        }
    }
}

/// The Quality tab's `Edits` section: who made edit calls, who struggled, and
/// which files struggled most.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcomes {
    /// One row per final model, most attempts first.
    pub by_model: Vec<EditOutcomeRow>,
    /// One row per worker, most attempts first.
    pub by_worker: Vec<EditOutcomeRow>,
    /// The files with the most failed calls, most failures first, at most
    /// [`MAX_TOP_FILES`] of them.
    pub top_files: Vec<EditFileRow>,
    /// Workers whose transcript was missing or unreadable.
    pub skipped: u32,
}

/// The transcript directory of the project at `root`
/// (`.coding/mnemo2/transcripts`).
pub fn transcripts_dir(root: &Path) -> PathBuf {
    root.join(".coding")
        .join(crate::mnemo2::project::STATE_SUBDIR)
        .join(TRANSCRIPTS_DIR)
}

/// Count the edit calls of the window's `workers` — `(worker_id, model)` pairs
/// — from their transcripts in `dir`.
///
/// A worker appears once even if the routing log gave it more than one outcome
/// row. Its transcript is parsed once per file state and then served from the
/// in-memory cache, so reopening the tab does not re-parse megabytes.
pub fn collect(dir: &Path, workers: &[(String, String)]) -> EditOutcomes {
    let mut by_model: BTreeMap<String, EditOutcomeRow> = BTreeMap::new();
    let mut by_worker: Vec<EditOutcomeRow> = Vec::new();
    let mut files: BTreeMap<String, EditFileRow> = BTreeMap::new();
    let mut skipped = 0u32;
    let mut seen: HashSet<&str> = HashSet::new();

    for (worker_id, model) in workers {
        if !seen.insert(worker_id.as_str()) {
            continue;
        }
        let path = dir.join(format!("{worker_id}.json"));
        let Some(calls) = transcript_calls(&path) else {
            skipped += 1;
            continue;
        };
        if calls.is_empty() {
            continue;
        }
        let mut worker_row = EditOutcomeRow::new(worker_id);
        for call in &calls {
            worker_row.add(call.outcome);
            by_model
                .entry(model.clone())
                .or_insert_with(|| EditOutcomeRow::new(model))
                .add(call.outcome);
            add_to_files(&mut files, call);
        }
        by_worker.push(worker_row);
    }

    EditOutcomes {
        by_model: busiest_first(by_model.into_values().collect()),
        by_worker: busiest_first(by_worker),
        top_files: worst_files(files),
        skipped,
    }
}

/// Rows by attempts, most first; ties by key so the output is stable.
fn busiest_first(mut rows: Vec<EditOutcomeRow>) -> Vec<EditOutcomeRow> {
    rows.sort_by(|a, b| b.attempts.cmp(&a.attempts).then_with(|| a.key.cmp(&b.key)));
    rows
}

/// The [`MAX_TOP_FILES`] files with the most failed calls, most failures
/// first; a file no call failed on is not listed.
fn worst_files(files: BTreeMap<String, EditFileRow>) -> Vec<EditFileRow> {
    let mut rows: Vec<EditFileRow> = files
        .into_values()
        .filter(|row| row.failures > 0)
        .collect();
    rows.sort_by(|a, b| {
        b.failures
            .cmp(&a.failures)
            .then_with(|| b.attempts.cmp(&a.attempts))
            .then_with(|| a.path.cmp(&b.path))
    });
    rows.truncate(MAX_TOP_FILES);
    rows
}

/// Attribute one call to the files it named: a success counts for every file it
/// touched, a failure once for the first named path (a failed `multi_edit`
/// wrote nothing, so the paths after it did not change).
fn add_to_files(files: &mut BTreeMap<String, EditFileRow>, call: &CountedCall) {
    if call.outcome != EditOutcome::Ok {
        if let Some(path) = call.paths.first() {
            file_row(files, path).add(call.outcome);
        }
        return;
    }
    for path in &call.paths {
        file_row(files, path).add(call.outcome);
    }
}

/// The row for `path`, created on first sight.
fn file_row<'a>(files: &'a mut BTreeMap<String, EditFileRow>, path: &str) -> &'a mut EditFileRow {
    files
        .entry(path.to_string())
        .or_insert_with(|| EditFileRow::new(path))
}

/// A transcript's parsed calls and the file state that identified them.
struct CachedCalls {
    /// The transcript's length when it was parsed.
    len: u64,
    /// Its modification time then, when the platform reports one.
    modified: Option<SystemTime>,
    /// The edit-tool calls read from it.
    calls: Vec<CountedCall>,
}

/// Parsed transcripts by path, per process: a tab re-open reuses the parse
/// while the file's length and modification time stay put.
fn cache() -> &'static Mutex<HashMap<PathBuf, CachedCalls>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedCalls>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One transcript's edit-tool calls, or `None` when it is missing or unreadable.
fn transcript_calls(path: &Path) -> Option<Vec<CountedCall>> {
    let metadata = std::fs::metadata(path).ok()?;
    let len = metadata.len();
    let modified = metadata.modified().ok();
    let guard = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = guard.get(path) {
        if cached.len == len && cached.modified == modified {
            return Some(cached.calls.clone());
        }
    }
    drop(guard);
    let text = std::fs::read_to_string(path).ok()?;
    let calls = parse_calls(&text)?;
    cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            path.to_path_buf(),
            CachedCalls {
                len,
                modified,
                calls: calls.clone(),
            },
        );
    Some(calls)
}

/// A transcript's edit-tool calls in order, or `None` when the text is not a
/// transcript at all.
fn parse_calls(text: &str) -> Option<Vec<CountedCall>> {
    let transcript: serde_json::Value = serde_json::from_str(text).ok()?;
    let messages = transcript.get("messages")?.as_array()?;
    // Results arrive in their own messages; index them by the call they answer.
    let mut results: HashMap<&str, &str> = HashMap::new();
    for message in messages {
        if message.get("role").and_then(serde_json::Value::as_str) != Some("tool") {
            continue;
        }
        let Some(id) = message
            .get("tool_call_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(content) = message.get("content").and_then(serde_json::Value::as_str) else {
            continue;
        };
        results.entry(id).or_insert(content);
    }
    let mut calls = Vec::new();
    for message in messages {
        let Some(tool_calls) = message
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for call in tool_calls {
            let Some(name) = call.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !EDIT_TOOLS.contains(&name) {
                continue;
            }
            let Some(id) = call.get("id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            // A call whose result never landed (a turn cut off mid-flight) is
            // not a completed attempt.
            let Some(content) = results.get(id) else {
                continue;
            };
            calls.push(CountedCall {
                outcome: classify(content),
                paths: named_paths(call.get("arguments")),
            });
        }
    }
    Some(calls)
}

/// The paths an edit call names: its `path`, or each of its `files[].path`
/// entries; `(unparsed)` when the arguments do not parse.
fn named_paths(arguments: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arguments) = arguments else {
        return vec![UNPARSED_PATH.to_string()];
    };
    let parsed = match arguments {
        serde_json::Value::String(raw) => serde_json::from_str::<serde_json::Value>(raw).ok(),
        other => Some(other.clone()),
    };
    let Some(parsed) = parsed else {
        return vec![UNPARSED_PATH.to_string()];
    };
    if let Some(path) = parsed.get("path").and_then(serde_json::Value::as_str) {
        return vec![path.to_string()];
    }
    if let Some(files) = parsed.get("files").and_then(serde_json::Value::as_array) {
        let paths: Vec<String> = files
            .iter()
            .filter_map(|file| file.get("path").and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    vec![UNPARSED_PATH.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real result text from `simArms01.json` (message 157): the stale-read gate
    /// intercepted a blind retry.
    const BLOCKED: &str = r#"[tool error] EDIT INTERCEPTED: your last 1 file_edit attempts failed because the old_string did not match — the file has drifted from your last read. Do not retry blind. First re-read the file with read_files (this exact path: crates/fg_sim/src/layout.rs), then retry the edit with the exact current text."#;

    /// Real result text from `simFactories01.json` (message 389): `lines`
    /// arrived as the string `"108, 130"` and the whole call was refused.
    const SHAPE: &str = r#"[tool error] Error: The tool 'file_edit' failed because an argument must be a sequence, not a string. Please check each parameter's type against the tool's schema and try again with the correct type. The executor received 2418 bytes of arguments (keys: from_read, lines, new_string, path); the received arguments end: `sert!(stats.sink_totals.get(\"iron_plate\").copied().unwrap_or(0) > 0);\n}","path":"crates/fg_sim/tests/determinism.rs"}`."#;

    /// Real result text from `simArms01.json` (message 233): the ops array
    /// arrived as the value of a single key instead of under `files`.
    const SHAPE_FILES_AS_KEY: &str = r#"[tool error] Error: The tool 'multi_edit' failed because an argument must be a sequence, not a string. Please check each parameter's type against the tool's schema and try again with the correct type. The executor received 5114 bytes of arguments (keys: [{"ops": [{"new_string": "            power_poles: save"#;

    /// Real result text from `simFactories01.json` (message 202): the cite came
    /// from a read the worker's own earlier edit had invalidated.
    const RECEIPT_DRIFT: &str = r#"[tool error] not found: line 180 changed since read 'rsx' — the file was modified after the read that minted the receipt — the content has likely drifted from your last read. Re-read the file (read_files, this exact path) and retry with the exact current text; do not edit without a fresh read."#;

    /// Real result text from `simArms01.json` (message 155): a remembered
    /// `old_string` no longer matched.
    const MISMATCH: &str = r#"[tool error] not found: multi_edit: files[0] 'crates/fg_sim/src/layout.rs': edit 2/2 failed (anchor: '/// Squared distance between two tiles.'): old_string not found in file — near-miss: matched 113/132 chars; first difference at char 114: file has 'let ', you sent 'dx *' — the content has likely drifted from your last read. Re-read the file (read_files, this exact path) and retry with the exact current text; do not edit without a fresh read."#;

    /// Real result text from `simFactories01.json` (message 229): a tool error
    /// none of the named causes covers yet.
    const OTHER: &str = r#"[tool error] invalid input: multi_edit: files[0] 'crates/fg_sim/src/views.rs': invalid input: edit rejected: emission artifact detected (unbalanced delimiters after the edit — the brace/bracket/paren deficit grew by 1 (0 → 1); ops[1] ('from_read rsc') grows it by +1 — rebalance each named op in the SAME call (pair it with its compensating op), or split the array; set artifact_check:false when you have verified the result yourself (cargo check / rustfmt --check)"#;

    /// Real success text from `simFactories01.json` (message 200).
    const OK: &str = r#"edited crates/fg_sim/src/types.rs
NOTE: large edit payload (old_string ~349 chars, new_string ~3491 chars) — emission fragility has been observed near this size in long sessions; use smaller fragments, or file_write for a full-file replacement (backlog e8b39d72 H5)"#;

    /// Write `transcript` as the transcript of `worker` under `dir`.
    fn write_transcript(dir: &Path, worker: &str, transcript: &serde_json::Value) {
        std::fs::create_dir_all(dir).expect("transcript dir");
        std::fs::write(dir.join(format!("{worker}.json")), transcript.to_string())
            .expect("transcript file");
    }

    /// A small transcript in the shipped shape: two `file_edit`s, one
    /// successful and one failed `multi_edit`, a `file_write` whose `files`
    /// arrived as a string, a drifted `file_append`, and a `shell` call that
    /// must not count.
    fn fixture_transcript() -> serde_json::Value {
        json!({
            "messages": [
                {"role": "assistant", "tool_calls": [
                    {"id": "c1", "name": "file_edit", "arguments": "{\"path\":\"src/a.rs\",\"old_string\":\"x\"}"}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": MISMATCH},
                {"role": "assistant", "tool_calls": [
                    {"id": "c2", "name": "file_edit", "arguments": "{\"path\":\"src/a.rs\",\"old_string\":\"y\"}"}
                ]},
                {"role": "tool", "tool_call_id": "c2", "content": "edited src/a.rs"},
                {"role": "assistant", "tool_calls": [
                    {"id": "c3", "name": "multi_edit", "arguments": "{\"files\":[{\"path\":\"src/b.rs\"},{\"path\":\"src/c.rs\"}]}"}
                ]},
                {"role": "tool", "tool_call_id": "c3", "content": "edited 2 files"},
                {"role": "assistant", "tool_calls": [
                    {"id": "c4", "name": "multi_edit", "arguments": "{\"files\":[{\"path\":\"src/b.rs\"},{\"path\":\"src/c.rs\"}]}"}
                ]},
                {"role": "tool", "tool_call_id": "c4", "content": BLOCKED},
                {"role": "assistant", "tool_calls": [
                    {"id": "c5", "name": "file_write", "arguments": "{\"files\":\"[{\\\"path\\\":\\\"src/d.rs\\\"}]\"}"}
                ]},
                {"role": "tool", "tool_call_id": "c5", "content": SHAPE},
                {"role": "assistant", "tool_calls": [
                    {"id": "c6", "name": "file_append", "arguments": {"path": "src/e.rs"}}
                ]},
                {"role": "tool", "tool_call_id": "c6", "content": RECEIPT_DRIFT},
                {"role": "assistant", "tool_calls": [
                    {"id": "c7", "name": "shell", "arguments": "{\"command\":\"ls\"}"}
                ]},
                {"role": "tool", "tool_call_id": "c7", "content": BLOCKED}
            ]
        })
    }

    #[test]
    fn classify_blocked_interception() {
        assert_eq!(classify(BLOCKED), EditOutcome::Blocked);
    }

    #[test]
    fn classify_shape_errors() {
        assert_eq!(classify(SHAPE), EditOutcome::Shape);
        assert_eq!(classify(SHAPE_FILES_AS_KEY), EditOutcome::Shape);
        // The markers step 4 of the plan adds: a cleanly parsed misshape and a
        // named parameter.
        assert_eq!(
            classify("[tool error] `files` must be a JSON array, but it arrived as a string"),
            EditOutcome::Shape
        );
        assert_eq!(
            classify("[tool error] Error: the tool 'file_edit' failed because parameter `lines` must be an array"),
            EditOutcome::Shape
        );
    }

    #[test]
    fn classify_receipt_drift() {
        assert_eq!(classify(RECEIPT_DRIFT), EditOutcome::ReceiptDrift);
        assert_eq!(
            classify("[tool error] not found: unknown read receipt 'r9'"),
            EditOutcome::ReceiptDrift
        );
        assert_eq!(
            classify("[tool error] not found: lines [1, 2] were rewritten by an edit since that read"),
            EditOutcome::ReceiptDrift
        );
        assert_eq!(
            classify("[tool error] not found: lines [9, 10] reach past the file's current end"),
            EditOutcome::ReceiptDrift
        );
    }

    #[test]
    fn classify_mismatch_and_other_and_ok() {
        assert_eq!(classify(MISMATCH), EditOutcome::Mismatch);
        assert_eq!(classify(OTHER), EditOutcome::Other);
        assert_eq!(classify(OK), EditOutcome::Ok);
    }

    #[test]
    fn aggregate_counts_by_model_worker_and_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transcripts = dir.path().join(TRANSCRIPTS_DIR);
        write_transcript(&transcripts, "editStat01", &fixture_transcript());

        let outcomes = collect(
            &transcripts,
            &[("editStat01".to_string(), "glm-5.3".to_string())],
        );

        assert_eq!(outcomes.skipped, 0);
        let model = outcomes.by_model.first().expect("model row");
        assert_eq!(model.key, "glm-5.3");
        assert_eq!(
            (
                model.attempts,
                model.ok,
                model.shape,
                model.receipt_drift,
                model.mismatch,
                model.blocked,
                model.other
            ),
            (6, 2, 1, 1, 1, 1, 0),
            "one count per edit-tool call; the shell call does not count"
        );
        assert_eq!(model.failures(), 4);

        let worker = outcomes.by_worker.first().expect("worker row");
        assert_eq!(worker.key, "editStat01");
        assert_eq!((worker.attempts, worker.ok), (6, 2));

        let paths: Vec<&str> = outcomes
            .top_files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            ["src/a.rs", "src/b.rs", UNPARSED_PATH, "src/e.rs"],
            "most failures first, then the busiest file, then path"
        );
        let a = outcomes
            .top_files
            .iter()
            .find(|file| file.path == "src/a.rs")
            .expect("a row");
        assert_eq!((a.attempts, a.failures, a.mismatch), (2, 1, 1));
        let b = outcomes
            .top_files
            .iter()
            .find(|file| file.path == "src/b.rs")
            .expect("b row");
        assert_eq!(
            (b.attempts, b.failures, b.blocked),
            (2, 1, 1),
            "the success counts for every file, the failure once for the first"
        );
        assert!(
            outcomes.top_files.iter().all(|file| file.path != "src/c.rs"),
            "a file with no failed call is not a problem file"
        );
    }

    #[test]
    fn a_worker_with_two_outcome_rows_counts_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transcripts = dir.path().join(TRANSCRIPTS_DIR);
        write_transcript(&transcripts, "editStat01", &fixture_transcript());

        let outcomes = collect(
            &transcripts,
            &[
                ("editStat01".to_string(), "glm-5.3".to_string()),
                ("editStat01".to_string(), "glm-5.3".to_string()),
            ],
        );

        assert_eq!(outcomes.by_worker.len(), 1);
        assert_eq!(outcomes.by_model[0].attempts, 6);
    }

    #[test]
    fn missing_transcript_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcomes = collect(
            dir.path(),
            &[("gone0001".to_string(), "glm-5.3".to_string())],
        );
        assert_eq!(outcomes.skipped, 1);
        assert!(outcomes.by_model.is_empty());
        assert!(outcomes.by_worker.is_empty());
        assert!(outcomes.top_files.is_empty());
    }

    #[test]
    fn unreadable_transcript_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("broken01.json"), "not a transcript").expect("write");
        let outcomes = collect(
            dir.path(),
            &[("broken01".to_string(), "glm-5.3".to_string())],
        );
        assert_eq!(outcomes.skipped, 1);
        assert!(outcomes.by_model.is_empty());
    }

    #[test]
    fn a_call_without_a_result_is_not_an_attempt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transcripts = dir.path().join(TRANSCRIPTS_DIR);
        write_transcript(
            &transcripts,
            "cutoff01",
            &json!({
                "messages": [
                    {"role": "assistant", "tool_calls": [
                        {"id": "c1", "name": "file_edit", "arguments": "{\"path\":\"src/a.rs\"}"}
                    ]}
                ]
            }),
        );
        let outcomes = collect(
            &transcripts,
            &[("cutoff01".to_string(), "glm-5.3".to_string())],
        );
        assert_eq!(outcomes.skipped, 0);
        assert!(outcomes.by_model.is_empty());
    }

    #[test]
    fn unparseable_arguments_are_filed_under_unparsed() {
        let broken = serde_json::Value::String("{\"files\":[{\"path\":\"src/x.rs\",}]".to_string());
        assert_eq!(named_paths(Some(&broken)), [UNPARSED_PATH]);
    }

    #[test]
    fn a_top_level_path_names_the_file_even_when_files_is_broken() {
        let arguments = json!({"files": "", "path": "crates/fg_script/src/api.rs"});
        assert_eq!(
            named_paths(Some(&arguments)),
            ["crates/fg_script/src/api.rs"],
            "a multi_edit that also carried a top-level path is filed under it"
        );
    }

    #[test]
    fn a_rewritten_transcript_is_reparsed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transcripts = dir.path().join(TRANSCRIPTS_DIR);
        write_transcript(&transcripts, "editStat01", &fixture_transcript());
        let first = collect(
            &transcripts,
            &[("editStat01".to_string(), "glm-5.3".to_string())],
        );
        assert_eq!(first.by_model[0].attempts, 6);

        write_transcript(&transcripts, "editStat01", &json!({"messages": []}));
        let second = collect(
            &transcripts,
            &[("editStat01".to_string(), "glm-5.3".to_string())],
        );
        assert!(
            second.by_model.is_empty(),
            "a shorter rewrite changes the length fingerprint and invalidates the cache"
        );
    }
}

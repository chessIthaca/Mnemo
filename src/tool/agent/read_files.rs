// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `read_files` — batch read of multiple files in one call.
//!
//! The single read tool: takes an array of `{path, start_line?, max_lines?}`
//! specs so the agent can read one file, or several, in a single round-trip
//! (the retired `file_read` name redirects here at the unknown-tool error).
//! Each file is validated through the sandbox, numbered, and
//! capped independently; per-file errors are reported inline without failing
//! the whole call. A whole-file read (no `start_line`/`max_lines`) of a large
//! indexed source file (an extension the code graph parses — any indexed
//! language: .rs/.ts/.tsx/.js/.py/…)
//! prepends one SYMBOL NUDGE line pointing at `graph_context(id=...)`, so
//! the agent takes targeted edges instead of another whole-file dump. With
//! the `delta_reads` lever on, a whole read of a big source file serves its
//! signature outline first, and a re-read serves a skeleton or a diff (see
//! `read_one_delta`). Lines are numbered `{n}: {line}` — no padding, no
//! trailing whitespace ([`numbered_line`]). Auto-run (read-only).

use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::optimizer::{estimate_tokens, savings_event};
use super::read_cache::{diff_if_small, skeleton_for, ReadCache, Skeleton, SKELETON_MAX_LINES};
use crate::codegraph::walk::Lang;
use crate::provider::ToolSchema;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Default maximum number of lines returned per file when the caller doesn't
/// specify `max_lines`.
pub(crate) const DEFAULT_MAX_LINES: usize = 500;

/// A whole-file read must exceed this line count to trigger the SYMBOL
/// NUDGE — small files fit in context comfortably; the note only earns its
/// keep when the agent just swallowed a big chunk of source.
pub(crate) const READ_NUDGE_MIN_LINES: usize = 300;

/// A FIRST whole-file read of a source file longer than this serves its
/// signature outline instead of the content (the `delta_reads` lever) —
/// the agent then reads the region it needs with `start_line`/`max_lines`.
/// At or below this line count a whole read serves the whole file, so an
/// outline would only force follow-up reads; above it a whole read would be
/// cut at [`DEFAULT_MAX_LINES`] anyway, so the outline costs nothing extra.
pub(crate) const OUTLINE_MIN_LINES: usize = DEFAULT_MAX_LINES;

/// Default maximum output size in bytes per file (~100 KB).
pub(crate) const DEFAULT_MAX_BYTES: usize = 102_400;

/// Maximum number of files accepted in a single call. Prevents unbounded
/// output from a huge file list.
const MAX_FILES: usize = 10;

/// Total output byte cap across all files (~500 KB). If the joined output
/// exceeds this, it is truncated and a note is appended.
pub(crate) const TOTAL_BYTE_CAP: usize = 512_000;

/// One numbered line of read output: `{n}: {line}` with no column padding
/// and the line's trailing spaces/tabs dropped (`{n}:` for a blank line).
/// Both are pure context cost: the padding carries nothing, and the anchor
/// matcher ignores trailing spaces/tabs — an `old_string` copied from this
/// view still lands on a line that has them, via the whitespace-normalized
/// fallback (`edit_ops::resolve_match_variant`). Leading indentation is
/// KEPT: this view is the only place the model sees it, and every
/// `new_string` and line-op payload must reproduce it.
pub(crate) fn numbered_line(n: usize, line: &str) -> String {
    match line.trim_end_matches([' ', '\t']) {
        "" => format!("{n}:"),
        line => format!("{n}: {line}"),
    }
}

/// Truncate `s` to at most `max` bytes, backing up to the nearest UTF-8 char
/// boundary so the result is always valid UTF-8 (a naive `String::truncate`
/// panics if the cut point lands inside a multi-byte char). Returns the
/// truncated string.
pub(crate) fn truncate_to_boundary(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Build an actionable "invalid arguments" error for a failed arg parse.
///
/// The bare serde message (e.g. `missing field 'files'`) is technically true
/// but useless when a model mixed up two similar tool shapes: serde silently
/// ignores unknown keys (no `deny_unknown_fields`), so a `read_files` call
/// that received `file_write`-shaped args (`{"path": ..., "content": ...}`)
/// reports only the missing required field. This helper appends the sorted
/// list of keys the call actually sent plus a `hint`, so the model can
/// self-correct on the next attempt. (The retired `file_read` name never
/// reaches argument deserialization any more — it dies at the unknown-tool
/// check, already carrying the one-shot redirect.)
pub(crate) fn invalid_args_error(
    tool: &str,
    e: &serde_json::Error,
    args: &serde_json::Value,
    hint: &str,
) -> String {
    let mut keys: Vec<String> = args
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    let keys = if keys.is_empty() {
        "none".to_string()
    } else {
        keys.join(", ")
    };
    let hint = if hint.is_empty() {
        String::new()
    } else {
        format!(" {hint}")
    };
    // The sanitized base (plan 21118961) names the offending
    // parameter/type in plain words instead of serde vocabulary; the
    // received-keys + hint context rides on top of it, and the
    // with-variant adds the received-bytes context (byte count + tail +
    // truncation remedy, backlog 05a0905f).
    format!(
        "{} (received keys: {keys}).{hint}",
        crate::tool::error_message::sanitize_arguments_error_with(tool, e, args)
    )
}

/// A single file-read spec within a `read_files` call.
#[derive(Debug, Deserialize)]
pub(crate) struct ReadSpec {
    pub path: String,
    /// Optional 1-indexed start line.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// Optional maximum number of lines to read.
    #[serde(default)]
    pub max_lines: Option<usize>,
}

/// Arguments for `read_files`.
#[derive(Debug, Deserialize)]
struct ReadFilesArgs {
    files: Vec<ReadSpec>,
}

/// Split a compact read spec — `path`, `path:start`, `path:start+count` or
/// `path:start-end` (1-indexed, end inclusive) — into (path, start_line,
/// max_lines). Only a trailing `:<digits>` range suffix is split off: a
/// Windows drive colon or any other colon stays part of the path. `Err`
/// names a `start-end` range whose end precedes its start.
pub(crate) fn parse_compact_spec(s: &str) -> Result<ReadSpec, String> {
    let s = s.trim();
    let whole = || ReadSpec { path: s.to_string(), start_line: None, max_lines: None };
    let Some((path, range)) = s.rsplit_once(':') else {
        return Ok(whole());
    };
    let num = |t: &str| {
        if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
            t.parse::<usize>().ok()
        } else {
            None
        }
    };
    let spec = |start, max_lines| ReadSpec { path: path.to_string(), start_line: Some(start), max_lines };
    if let Some(start) = num(range) {
        return Ok(spec(start, None));
    }
    if let Some((start, count)) = range.split_once('+').and_then(|(a, b)| Some((num(a)?, num(b)?))) {
        return Ok(spec(start, Some(count)));
    }
    if let Some((start, end)) = range.split_once('-').and_then(|(a, b)| Some((num(a)?, num(b)?))) {
        if end < start {
            return Err(format!(
                "\"{s}\": the range ends at line {end}, before its start line {start} — \
                 use \"path:start-end\" with end >= start, or \"path:start+count\""
            ));
        }
        return Ok(spec(start, Some(end - start + 1)));
    }
    Ok(whole())
}

/// One `files` entry in either accepted form: the compact string (see
/// [`parse_compact_spec`]) or the `{path, start_line?, max_lines?}` object.
/// The single parse every reader of read_files arguments shares (steering,
/// compaction digests, re-read pointers, the result archive); `None` for an
/// entry that is neither, or a string whose range is invalid.
pub(crate) fn parse_spec(item: &serde_json::Value) -> Option<ReadSpec> {
    match item {
        serde_json::Value::String(s) => parse_compact_spec(s).ok(),
        serde_json::Value::Object(o) => {
            let line = |key: &str| o.get(key).and_then(|v| v.as_u64()).map(|n| n as usize);
            Some(ReadSpec {
                path: o.get("path")?.as_str()?.to_string(),
                start_line: line("start_line"),
                max_lines: line("max_lines"),
            })
        }
        _ => None,
    }
}

/// Every spec a read_files (or file_read) call names: the single-file
/// top-level `path` shorthand first, then each `files` entry.
pub(crate) fn read_specs(args: &serde_json::Value) -> Vec<ReadSpec> {
    let mut specs: Vec<ReadSpec> = args.get("path").and_then(|_| parse_spec(args)).into_iter().collect();
    if let Some(files) = args.get("files").and_then(|v| v.as_array()) {
        specs.extend(files.iter().filter_map(parse_spec));
    }
    specs
}

/// Expand the compact string entries of a `files` array into the object
/// form [`ReadSpec`] deserializes; object entries pass through untouched.
fn expand_compact_specs(mut args: serde_json::Value) -> Result<serde_json::Value, String> {
    if let Some(files) = args.get_mut("files").and_then(|f| f.as_array_mut()) {
        for item in files.iter_mut() {
            if let Some(s) = item.as_str() {
                let spec = parse_compact_spec(s)?;
                let mut object = serde_json::Map::new();
                object.insert("path".into(), json!(spec.path));
                if let Some(start) = spec.start_line {
                    object.insert("start_line".into(), json!(start));
                }
                if let Some(max) = spec.max_lines {
                    object.insert("max_lines".into(), json!(max));
                }
                *item = serde_json::Value::Object(object);
            }
        }
    }
    Ok(args)
}

/// The `read_files` tool — batch read of multiple files.
pub struct ReadFilesTool {
    sandbox: Sandbox,
    /// The project's code graph, when indexing is enabled. Drives the
    /// whole-file SYMBOL NUDGE (see [`READ_NUDGE_MIN_LINES`]); `None` in
    /// tests and codegraph-opted-out projects, where the nudge never fires.
    graph: Option<Arc<crate::codegraph::CodeGraph>>,
    /// Token-optimizer lever 1 (backlog e4a50d22): the live
    /// `[general.optimizer]` config. `None` (CLI contract builds, tests
    /// pinning pre-lever behavior) keeps the lever off — full content
    /// every time, byte-identical to the pre-lever build.
    optimizer: Option<Arc<RwLock<crate::config::OptimizerConfig>>>,
    /// This agent's read cache (the delta baseline). Built fresh by
    /// [`with_optimizer`](Self::with_optimizer) — per-agent, never shared
    /// across subagents (a subagent's first read must not inherit a
    /// false "already in context" state).
    read_cache: Option<Arc<ReadCache>>,
}

impl ReadFilesTool {
    /// Create the tool, bound to a sandbox (no graph — the nudge is off).
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            graph: None,
            optimizer: None,
            read_cache: None,
        }
    }

    /// Attach the project's code graph (builder pattern, like
    /// `SpawnAgentTool::with_model_resolver`). Enables the whole-file
    /// SYMBOL NUDGE: a whole read of a large indexed source file earns an
    /// advisory note pointing at the graph tools.
    pub fn with_codegraph(mut self, graph: Option<Arc<crate::codegraph::CodeGraph>>) -> Self {
        self.graph = graph;
        self
    }

    /// Enable the delta/skeleton re-read lever (backlog e4a50d22): attach
    /// the live `[general.optimizer]` config and a fresh per-agent read
    /// cache. While `delta_reads` is off the tool serves the full content
    /// exactly as before and the cache stays untouched (byte-identical
    /// default); the flag is re-read per call, so a config save lands on
    /// the next read without a rebuild.
    pub fn with_optimizer(mut self, cfg: Arc<RwLock<crate::config::OptimizerConfig>>) -> Self {
        self.optimizer = Some(cfg);
        if self.read_cache.is_none() {
            self.read_cache = Some(Arc::new(ReadCache::new()));
        }
        self
    }
}

#[async_trait]
impl Tool for ReadFilesTool {
    fn name(&self) -> &str {
        "read_files"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "read_files",
            format!(
                "{} Up to 10 file strings, each \"path\", \"path:start+count\" \
                 (count lines from start) or \"path:start-end\" (inclusive), \
                 1-indexed. Each file comes back under a header with line \
                 numbers (default: the first 500 lines). A directory path \
                 returns a sorted listing. Per-file errors are reported \
                 inline, never fatal. Each read mints a read-receipt id in its \
                 trailer: cite it in file_edit/multi_edit as {{\"from_read\": \
                 \"<id>\", \"lines\": [s, e], \"new_string\": …}} to anchor an \
                 edit without carrying the verbatim bytes; if the file \
                 drifted, the edit fails and asks for a fresh read.",
                tool_contract::contract(
                    "`files`",
                    "{\"files\":[\"src/main.rs\",\"src/lib.rs:120+40\"]}"
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Files to read (max 10), paths relative to the project root.",
                        "items": {"type": "string"}
                    }
                },
                "required": ["files"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn on_context_reduced(&self) {
        if let Some(cache) = &self.read_cache {
            cache.forget_served();
        }
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        // Single-file shorthand: a top-level `path` (with the optional
        // start_line/max_lines that go with it) is lifted into a one-element
        // batch. This is what `file_read` used to be — absorbing the shape
        // rather than erroring on it is why that tool no longer needs to
        // exist, and it means the shape a model most naturally reaches for
        // simply works instead of costing a failed call plus a retry.
        // (Backlog 26cdbaf8: the shorthand is now UNADVERTISED — the schema
        // steers the model to the files-array form only, the dual optional
        // forms being the ambiguity behind the empty-argument failures —
        // but the absorption stays: harness steering (read_files_paths
        // parses both forms from raw args) and habit-shaped calls keep
        // working.)
        let args = match args.get("path").and_then(|p| p.as_str()) {
            Some(path) if args.get("files").is_none() => {
                let mut spec = serde_json::Map::new();
                spec.insert("path".into(), serde_json::Value::from(path));
                for k in ["start_line", "max_lines"] {
                    if let Some(v) = args.get(k) {
                        spec.insert(k.into(), v.clone());
                    }
                }
                serde_json::json!({ "files": [serde_json::Value::Object(spec)] })
            }
            _ => args,
        };
        // Backlog c19ec2ff (c): the files argument sometimes arrives as one
        // stringified JSON array (the transport degradation class — hit 3x
        // in one turn, 2027-01-25; the generic "an argument must be a
        // sequence, not a string" taught nothing, so the model re-emitted
        // the identical payload). The dispatch seam repairs a clean parse;
        // this layer catches direct execute calls and goes further: a
        // MALFORMED string (observed: a stray quote after a numeric field)
        // still recovers its well-formed {"path": ...} specs, with the byte
        // position of the parse failure in the NOTE/error.
        let (args, repair_note) = match args.get("files") {
            Some(serde_json::Value::String(s)) => {
                let (specs, skipped, parse_err) = recover_files_from_string(s);
                if specs.is_empty() {
                    if let Some((pos, reason)) = parse_err {
                        let fragments = if skipped.is_empty() {
                            String::new()
                        } else {
                            format!(" (malformed fragments: {})", skipped.join(" | "))
                        };
                        return ToolResult::error(format!(
                            "the files argument arrived as a STRING, not an array — its \
                             JSON parse failed at byte {pos}: {reason}. Well-formed \
                             {{\"path\": ...}} specs inside the string are auto-recovered, \
                             but none were{fragments}. Emit files as a native JSON \
                             array of strings: [\"src/main.rs:10+40\"]"
                        ));
                    }
                    // A clean "[]": splice the empty array through so the
                    // canonical empty-array error fires below.
                    (json!({"files": []}), None)
                } else if let Some((pos, reason)) = parse_err {
                    let note = format!(
                        "NOTE (repaired files argument): the files argument arrived \
                         as a STRING; {} well-formed spec(s) recovered and read, {} \
                         malformed fragment(s) skipped (JSON parse failed at byte \
                         {pos}: {reason}). Re-read the skipped file(s) with a \
                         corrected array form. Fragments: {}",
                        specs.len(),
                        skipped.len(),
                        skipped.join(" | ")
                    );
                    (json!({"files": specs}), Some(note))
                } else {
                    // A clean stringified array — repaired silently (the
                    // dispatch seam normally catches this before the tool).
                    (json!({"files": specs}), None)
                }
            }
            _ => (args, None),
        };
        // The advertised compact strings ("path:start+count") expand into
        // the object form; object specs still deserialize as they are.
        let args = match expand_compact_specs(args) {
            Ok(args) => args,
            Err(e) => return ToolResult::error(e),
        };
        let args: ReadFilesArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => {
                // Backlog 26cdbaf8: the recovery rule rides the error itself —
                // the model reads this at retry time (the description note
                // only helps before the first failure, and the circuit
                // breaker only fires after two identical failures).
                return ToolResult::error(invalid_args_error(
                    "read_files",
                    &e,
                    &args,
                    &tool_contract::recovery_hint(
                        "files (an array of \"path\" or \"path:start+count\" strings)",
                    ),
                ));
            }
        };
        if args.files.is_empty() {
            return ToolResult::error("files array is empty — provide at least one file spec");
        }
        if args.files.len() > MAX_FILES {
            return ToolResult::error(format!(
                "too many files: {} (max {MAX_FILES}). Split into multiple read_files calls.",
                args.files.len()
            ));
        }

        let sandbox = self.sandbox.clone();
        let graph = self.graph.clone();
        let optimizer = self.optimizer.clone();
        let read_cache = self.read_cache.clone();
        tokio::task::spawn_blocking(move || {
            let mut sections: Vec<String> = Vec::with_capacity(args.files.len());
            // The whole-file SYMBOL NUDGE: a WHOLE-FILE read (no start_line
            // and no max_lines — the single-file `path` shorthand lifts into
            // the same batch) of a large indexed source file earns one
            // advisory line ABOVE the results. Tool output is truncated from
            // the END, so a note appended at the bottom could be cut away
            // exactly when the results are long (the with_note pattern,
            // search.rs). Best-effort at every step: per-file read errors,
            // small files, non-source extensions, and absent/unindexed
            // graphs all stay quiet.
            let mut nudge: Vec<String> = Vec::new();
            // Token-optimizer lever 1 (backlog e4a50d22): with the flag
            // on, a whole-file RE-read serves a signature skeleton
            // (unchanged file) or a unified diff (changed file) against
            // the last-served content. A first read serves the outline of a
            // big file, else the full content (the delta baseline).
            let delta_reads = optimizer
                .as_ref()
                .map(|c| c.read().map(|c| c.delta_reads).unwrap_or(false))
                .unwrap_or(false);
            let mut savings: Vec<serde_json::Value> = Vec::new();
            for spec in &args.files {
                if is_whole_file_source_read(spec) {
                    let (section, content, _) = match (delta_reads, read_cache.as_ref()) {
                        (true, Some(cache)) => read_one_delta(&sandbox, spec, cache, &mut savings),
                        _ => read_one_with_count(&sandbox, spec),
                    };
                    if let Some(content) = content {
                        if content.lines().count() > READ_NUDGE_MIN_LINES {
                            if let Some(graph) = &graph {
                                if let Some(count) = graph.symbol_count_in_file(&spec.path) {
                                    if count > 0 {
                                        nudge.push(format!(
                                            "{} has {count} indexed symbols — graph_context(id) \
                                             gives targeted edges (definition/callers/blast \
                                             radius); read line slices for surrounding detail",
                                            spec.path
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    sections.push(section);
                } else {
                    let section = read_one(&sandbox, spec);
                    if let (true, Some(cache)) = (delta_reads, read_cache.as_ref()) {
                        charge_outline_followup(&sandbox, spec, &section, cache, &mut savings);
                    }
                    sections.push(section);
                }
            }
            let mut output = sections.join("\n\n");
            if !nudge.is_empty() {
                // ONE leading note: the marker prefix once, the per-file
                // clauses joined with "; " (review 2026-09-14 — a repeated
                // prefix per file read as multiple notes).
                output = format!("SYMBOL NUDGE: {}\n\n{output}", nudge.join("; "));
            }
            if let Some(note) = repair_note {
                // Backlog c19ec2ff (c): the repair NOTE leads the output —
                // it is about the call itself, not the results, and a
                // LEADING note survives the END-truncation below.
                output = format!("{note}\n\n{output}");
            }

            // Total byte cap across all files. Back up to a char boundary so a
            // multi-byte char at the cut point doesn't panic (String::truncate
            // panics on non-boundary lengths).
            if output.len() > TOTAL_BYTE_CAP {
                truncate_to_boundary(&mut output, TOTAL_BYTE_CAP);
                output.push_str("\n... (truncated: total output exceeded size limit)");
            }
            if savings.is_empty() {
                ToolResult::success(output)
            } else {
                // The before/after counts of every reduced serve ride the
                // result's data; the turn loop records each as a
                // savings_events ledger row (fire-and-forget).
                ToolResult::success(output).with_data(json!({ "savings": savings }))
            }
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("read_files task failed: {e}")))
    }
}

/// Backlog c19ec2ff (c): recover file specs from a stringified `files`
/// argument. Layer 1: a clean JSON-array parse (the dispatch seam
/// normally repairs this shape before the call reaches the tool; this
/// layer keeps direct execute calls working). Layer 2: when the payload
/// does not parse (observed, 2027-01-25: a stray quote after a numeric
/// field inside one spec), an escape-aware brace scan extracts each
/// `{...}` candidate and keeps the ones that parse as objects carrying a
/// string `path`; failures become short excerpts for the NOTE/error. The
/// clean-parse failure (0-based byte position + serde's reason) rides
/// along for the caller's positional message.
fn recover_files_from_string(
    s: &str,
) -> (Vec<serde_json::Value>, Vec<String>, Option<(usize, String)>) {
    match crate::tool::parse_stringified_sequence(s) {
        Ok(specs) => (specs, Vec::new(), None),
        Err((pos, reason)) => {
            // Specs with a stray quote after a number are recoverable too,
            // even when another defect fails the whole-array parse.
            let repaired = crate::tool::drop_stray_quotes_after_numbers(s);
            let (specs, skipped) = extract_path_objects(repaired.as_deref().unwrap_or(s));
            (specs, skipped, Some((pos, reason)))
        }
    }
}

/// The lenient layer of [`recover_files_from_string`]: scan `s` for
/// brace-balanced `{...}` candidates (string/escape-aware) and keep those
/// that parse as objects with a string `path`. Candidates are cut on ASCII
/// delimiters only, so byte slicing never splits a multi-byte char. A
/// candidate that fails to parse may have swallowed later well-formed
/// specs (a stray quote derails the string state) — the scan resumes
/// INSIDE its span so they are still recovered.
fn extract_path_objects(s: &str) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut specs: Vec<serde_json::Value> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let start = i;
        // String/escape-aware balance walk for the candidate's close.
        let mut depth: usize = 0;
        let mut in_string = false;
        let mut j = i;
        while j < bytes.len() {
            match bytes[j] {
                b'\\' if in_string => j += 1,
                b'"' => in_string = !in_string,
                b'{' if !in_string => depth += 1,
                b'}' if !in_string => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        let end = j.min(s.len());
        let candidate = &s[start..end];
        match serde_json::from_str::<serde_json::Value>(candidate) {
            Ok(obj) => {
                if obj.get("path").is_some_and(|p| p.is_string()) {
                    specs.push(obj);
                } else {
                    skipped.push(excerpt_of(candidate));
                }
                // Parsed cleanly — nothing recoverable inside; advance past.
                i = end;
            }
            Err(_) => {
                skipped.push(excerpt_of(candidate));
                // Rescan inside the failed candidate's span (see doc).
                i = start + 1;
            }
        }
    }
    (specs, skipped)
}

/// A compact one-line excerpt of a skipped fragment for the NOTE/error.
fn excerpt_of(s: &str) -> String {
    let short: String = s.chars().take(48).collect();
    short.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Maximum number of entries a directory listing renders before capping —
/// mirrors search's MAX_MATCHES discipline: a huge directory (target/,
/// node_modules/) must not flood the conversation. The header still shows
/// the TRUE entry count and the cap note names the remainder.
const LISTING_ENTRY_CAP: usize = 300;

/// Render a directory listing for a read_files spec: a header naming the
/// directory and its entry count, then one line per entry sorted by name —
/// `name/ (dir)` for directories, `name (file, S bytes, mtime T)` for
/// files. Unreadable entries are skipped and an unreadable directory
/// degrades to a per-spec error line — a listing must never blank out
/// sibling reads.
fn directory_listing(dir: &Path, display: &str) -> String {
    let mut entries: Vec<(String, bool, u64, u64)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let meta = e.metadata().ok()?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Some((name, meta.is_dir(), meta.len(), mtime))
            })
            .collect(),
        Err(e) => {
            return format!("=== {} (error) ===\nfailed to list directory: {e}", display);
        }
    };
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let total = entries.len();
    let mut out = format!("=== {} (directory, {} entries) ===\n", display, total);
    for (name, is_dir, size, mtime) in entries.iter().take(LISTING_ENTRY_CAP) {
        if *is_dir {
            out.push_str(&format!("{name}/ (dir)\n"));
        } else {
            out.push_str(&format!("{name} (file, {size} bytes, mtime {mtime})\n"));
        }
    }
    if total > LISTING_ENTRY_CAP {
        out.push_str(&format!(
            "... and {} more entries (narrow with a more specific path)\n",
            total - LISTING_ENTRY_CAP
        ));
    }
    out
}

/// Read a single file spec into a header + numbered-body section (or an
/// inline error string for this file). Never propagates an error to the
/// caller — one bad path must not blank out the other reads.
pub(crate) fn read_one(sandbox: &Sandbox, spec: &ReadSpec) -> String {
    read_one_with_count(sandbox, spec).0
}

/// Charge a follow-up ranged read against what a prior outline of the same
/// file saved: a ledger `outline_followup` event (tokens_before 0,
/// tokens_after = the tokens charged), so the outline is credited only with
/// what it really saved — never more. A range starting beyond
/// [`DEFAULT_MAX_LINES`] is not charged: a whole read could not have served
/// those lines either, so the range would have been needed anyway.
fn charge_outline_followup(
    sandbox: &Sandbox,
    spec: &ReadSpec,
    section: &str,
    cache: &ReadCache,
    savings: &mut Vec<serde_json::Value>,
) {
    if spec.start_line.unwrap_or(1) > DEFAULT_MAX_LINES {
        return;
    }
    let Ok(key) = sandbox.validate(Path::new(&spec.path)) else {
        return;
    };
    let tokens = estimate_tokens(section);
    if let Some(charged) = cache.charge_outline(&key, tokens) {
        savings.push(savings_event("outline_followup", &spec.path, 0, charged));
    }
}

/// [`read_one`] plus the raw file content (`None` for per-spec errors) —
/// `read_files::execute` uses the line count for the whole-file SYMBOL
/// NUDGE without a second disk read. `search_read` keeps calling
/// [`read_one`]; it is behavior-identical there.
///
/// The third element is the minted read-receipt id (backlog 3fdc605f,
/// plan 5a21fc8f): every real serve mints a receipt for the lines it
/// showed and appends the cite-form trailer to the section — `None`
/// when nothing was minted (per-spec errors, directories, empty
/// ranges). `read_one_delta` revokes it on its reduced serves (the
/// model never saw those lines verbatim).
pub(crate) fn read_one_with_count(
    sandbox: &Sandbox,
    spec: &ReadSpec,
) -> (String, Option<String>, Option<String>) {
    let validated = match sandbox.validate(Path::new(&spec.path)) {
        Ok(p) => p,
        Err(e) => {
            return (
                format!("=== {} (error) ===\npath validation failed: {e}", spec.path),
                None,
                None,
            )
        }
    };
    // Directory spec (backlog #89 named the path kind; 2026-12-29 session:
    // "what files are in this dir" had no affordance — the old error hint
    // sent the model to search, whose 100-match cap + display truncation
    // made enumeration unreliable). Return a sorted listing instead.
    if validated.is_dir() {
        return (directory_listing(&validated, &spec.path), None, None);
    }
    let content = match std::fs::read_to_string(&validated) {
        Ok(c) => c,
        Err(e) => {
            return (
                format!("=== {} (error) ===\nfailed to read: {e}", spec.path),
                None,
                None,
            )
        }
    };

    let total_lines = content.lines().count();
    let start = spec.start_line.unwrap_or(1).saturating_sub(1);
    // Cap the number of lines at DEFAULT_MAX_LINES unless the caller
    // explicitly requested fewer.
    let max = spec
        .max_lines
        .unwrap_or(DEFAULT_MAX_LINES)
        .min(DEFAULT_MAX_LINES);

    let numbered: Vec<String> = content
        .lines()
        .skip(start)
        .take(max)
        .enumerate()
        .map(|(i, line)| numbered_line(start + i + 1, line))
        .collect();

    let showed = numbered.len();
    let mut body = numbered.join("\n");

    // Byte-cap per file. Back up to a char boundary so a multi-byte char at
    // the cut point doesn't panic.
    let truncated_by_bytes = if body.len() > DEFAULT_MAX_BYTES {
        truncate_to_boundary(&mut body, DEFAULT_MAX_BYTES);
        true
    } else {
        false
    };

    // Truncation note only when the default line cap was the limiting factor
    // (not when the caller explicitly requested a range). The byte cap always
    // notes.
    let available_from_start = total_lines.saturating_sub(start);
    let default_capped = spec.max_lines.is_none() && showed < available_from_start;
    if truncated_by_bytes || default_capped {
        body.push_str(&format!(
            "\n... (truncated: {total_lines} lines total, showed {showed})"
        ));
    }

    let first_line = start + 1;
    let last_line = start + showed;
    // Backlog 3fdc605f (plan 5a21fc8f): mint the receipt for what was
    // ACTUALLY served (post-truncation) and teach the cite form in the
    // trailer — the from_read anchor cites the id instead of carrying
    // the verbatim bytes, so context compaction is harmless to the
    // edit path. Only a real serve mints (directories, errors, and
    // empty ranges never do).
    let receipt = if showed > 0 {
        let served: Vec<&str> = content.lines().skip(start).take(showed).collect();
        let id =
            crate::tool::agent::read_registry::mint(validated.clone(), first_line, &served);
        body.push_str(&format!(
            "\n[read receipt {id} — edit via file_edit \
             {{\"from_read\": \"{id}\", \"lines\": [s, e], \"new_string\": …}}]"
        ));
        Some(id)
    } else {
        None
    };
    let header = if showed == 0 {
        format!("=== {} (empty range) ===", spec.path)
    } else {
        format!(
            "=== {} (lines {first_line}-{last_line} of {total_lines}) ===",
            spec.path
        )
    };
    (format!("{header}\n{body}"), Some(content), receipt)
}

/// Whole-file read with the delta/skeleton lever applied (backlog
/// e4a50d22, `delta_reads`). Returns the section, the raw content (for the
/// SYMBOL NUDGE probe), and appends any savings events to `savings`.
/// Semantics:
/// - first read of a source file over [`OUTLINE_MIN_LINES`] lines →
///   numbered signature outline + a note pointing at ranged reads; no
///   baseline is recorded (the agent has not seen the content);
/// - first read of any other path → full content, and the content becomes
///   the delta baseline;
/// - re-read, file UNCHANGED → numbered signature skeleton + a note
///   pointing at the ranged-read escape hatch;
/// - re-read, file CHANGED → unified diff against the baseline (full
///   re-serve once the diff would exceed ~40% of the file);
/// - ranged reads never reach here (`is_whole_file_source_read` filters
///   them) — they always serve exact lines.
fn read_one_delta(
    sandbox: &Sandbox,
    spec: &ReadSpec,
    cache: &ReadCache,
    savings: &mut Vec<serde_json::Value>,
) -> (String, Option<String>, Option<String>) {
    // Today's full serve — the baseline path and the fallback on every
    // reduced-path miss (per-spec errors, directories, degenerate input).
    let (full_section, content, receipt) = read_one_with_count(sandbox, spec);
    let Some(content) = content else {
        return (full_section, None, receipt);
    };
    let Some(lang) = Path::new(&spec.path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| Lang::from_extension(&e.to_ascii_lowercase()))
    else {
        // is_whole_file_source_read guarantees a known Lang; this is a
        // belt-and-braces fail-open (full serve) if that ever drifts.
        return (full_section, Some(content), receipt);
    };
    let Ok(key) = sandbox.validate(Path::new(&spec.path)) else {
        return (full_section, Some(content), receipt);
    };
    let before_tokens = estimate_tokens(&full_section);
    match cache.snapshot(&key) {
        None => {
            // A big file's FIRST whole read serves its outline: the agent
            // reads the region it needs next instead of swallowing up to
            // DEFAULT_MAX_LINES lines it mostly won't use. The baseline is
            // NOT recorded — the agent never saw the content, so a later
            // whole read must not be answered with a diff against it.
            let total = content.lines().count();
            if total > OUTLINE_MIN_LINES && lang != Lang::Html {
                let sk = skeleton_for(&content, lang);
                // No signature lines: an empty outline maps nothing.
                if sk.kept > 0 {
                    let header = format!(
                        "=== {} (outline: {} signature lines of {total}) ===",
                        spec.path, sk.kept
                    );
                    let section = skeleton_section(
                        &header,
                        sk,
                        &format!(
                            "[outline read: a {total}-line file, signature lines only. Read \
                             the region you need as a \"path:start+count\" range — ranges \
                             always serve exact lines.]"
                        ),
                    );
                    let after_tokens = estimate_tokens(&section);
                    if after_tokens < before_tokens {
                        savings.push(savings_event(
                            "outline",
                            &spec.path,
                            before_tokens,
                            after_tokens,
                        ));
                        // Charge any later ranged read of this file against
                        // what this outline really saved (outline_followup).
                        cache.note_outline(key, before_tokens - after_tokens);
                        // An outline is a signature skeleton, not the lines:
                        // the full-serve receipt must not stay anchorable
                        // (backlog 3fdc605f), like the unchanged-file skeleton.
                        if let Some(id) = receipt.as_deref() {
                            crate::tool::agent::read_registry::revoke(id);
                        }
                        return (section, Some(content), None);
                    }
                }
            }
            cache.record(key, content.clone());
            (full_section, Some(content), receipt)
        }
        Some(old) if old == content => {
            if lang == Lang::Html {
                // No line-level signatures in HTML — a skeleton would be
                // noise. Serve the full content; the baseline stays cached
                // so future DIFF serves work.
                return (full_section, Some(content), receipt);
            }
            let sk = skeleton_for(&content, lang);
            let total = content.lines().count();
            let header = format!(
                "=== {} (unchanged since your last read — signature skeleton: {} of {total} lines) ===",
                spec.path,
                sk.kept
            );
            let section = skeleton_section(
                &header,
                sk,
                "[delta read: file is UNCHANGED since your last full read — signature lines \
                 only. Re-read a \"path:start+count\" range for exact lines.]",
            );
            let after_tokens = estimate_tokens(&section);
            if after_tokens >= before_tokens {
                // Tiny file: the header + note boilerplate costs more than
                // the full serve. A reduced serve must never cost MORE
                // than the full one — serve the full content (the cached
                // baseline is already current; no savings row).
                return (full_section, Some(content), receipt);
            }
            savings.push(savings_event(
                "skeleton",
                &spec.path,
                before_tokens,
                after_tokens,
            ));
            // The model saw a SIGNATURE SKELETON, not the lines — the
            // full-serve receipt minted inside read_one_with_count must
            // not stay anchorable (backlog 3fdc605f).
            if let Some(id) = receipt.as_deref() {
                crate::tool::agent::read_registry::revoke(id);
            }
            (section, Some(content), None)
        }
        Some(old) => match diff_if_small(&old, &content) {
            Some(diff) => {
                let header = format!(
                    "=== {} (changed since your last read — unified diff vs your last full read) ===",
                    spec.path
                );
                let body = format!(
                    "{diff}\n[delta read: unified diff vs your last full read. Re-read a \
                     \"path:start+count\" range for the exact current content.]"
                );
                let section = format!("{header}\n{body}");
                let after_tokens = estimate_tokens(&section);
                if after_tokens >= before_tokens {
                    // The ratio gate compares the raw diff to the file; the
                    // served section still adds the header + note, so a
                    // small file can tip over. A reduced serve must never
                    // cost MORE than the full one — fall back to the full
                    // re-serve and refresh the baseline.
                    cache.record(key, content.clone());
                    return (full_section, Some(content), receipt);
                }
                savings.push(savings_event(
                    "delta_read",
                    &spec.path,
                    before_tokens,
                    after_tokens,
                ));
                cache.record(key, content.clone());
                // A unified DIFF is not the lines — revoke like the
                // skeleton path (backlog 3fdc605f).
                if let Some(id) = receipt.as_deref() {
                    crate::tool::agent::read_registry::revoke(id);
                }
                (section, Some(content), None)
            }
            None => {
                // Mostly-rewritten file: a diff would cost more than a
                // full re-serve. Keep today's full section; refresh the
                // baseline.
                cache.record(key, content.clone());
                (full_section, Some(content), receipt)
            }
        },
    }
}

/// A skeleton serve's section: the header, the numbered signature lines
/// (plus the cap note when [`SKELETON_MAX_LINES`] cut them short), then
/// `note` on how to reach exact lines.
fn skeleton_section(header: &str, sk: Skeleton, note: &str) -> String {
    let mut body = sk.body;
    if sk.capped {
        body.push_str(&format!(
            "... (skeleton capped at {SKELETON_MAX_LINES} signature lines)\n"
        ));
    }
    format!("{header}\n{body}{note}")
}

/// Whether a spec qualifies for the whole-file SYMBOL NUDGE probe: no
/// `start_line` and no `max_lines` (the caller asked for the WHOLE file)
/// and an extension the code graph parses (any indexed language — see
/// `Lang::from_extension`). Non-source and sliced reads never nudge.
fn is_whole_file_source_read(spec: &ReadSpec) -> bool {
    if spec.start_line.is_some() || spec.max_lines.is_some() {
        return false;
    }
    Path::new(&spec.path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| Lang::from_extension(&e.to_ascii_lowercase()).is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::codegraph::CodeGraph;
    use tempfile::tempdir;

    fn make_tool(dir: &Path) -> ReadFilesTool {
        ReadFilesTool::new(Sandbox::new(dir).unwrap())
    }

    fn make_indexed_tool(dir: &Path) -> ReadFilesTool {
        // Mirror search.rs's make_indexed_tool: an indexed in-memory graph
        // over the same root, so the whole-file nudge can fire.
        let graph = CodeGraph::open_in_memory(dir.to_path_buf()).unwrap();
        graph.index(None).unwrap();
        ReadFilesTool::new(Sandbox::new(dir).unwrap()).with_codegraph(Some(Arc::new(graph)))
    }

    /// Write a Rust file with `n` 12-line free functions: `n = 40` (480 lines)
    /// clears the 300-line whole-file nudge threshold but stays at or under
    /// the 500-line outline threshold; `n = 50` (600 lines) is outlined.
    fn write_big_rust_file(dir: &Path, name: &str, n: usize) {
        let mut src = String::new();
        for i in 0..n {
            src.push_str(&format!(
                "pub fn sym_{i}(x: u32) -> u32 {{\n    let a = x + {i};\n    let b = a * 2;\n    \
                 let c = b - 1;\n    let d = c + a;\n    let e = d * b;\n    let f = e - c;\n    \
                 let g = f + b;\n    let h = g * d;\n    h + e\n}}\n\n"
            ));
        }
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
    }

    #[tokio::test]
    async fn read_mints_a_receipt_id_trailer() {
        // Backlog 3fdc605f (plan 5a21fc8f): every successful file read
        // mints a short read-receipt id in its section trailer — the
        // anchor file_edit/multi_edit cite instead of re-emitting (and
        // post-compaction re-reading) the verbatim bytes. Red-first:
        // today no trailer exists.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"files": [{"path": "a.txt"}]})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("[read receipt "),
            "the read section must carry the receipt trailer: {}",
            result.output
        );
        let id = result
            .output
            .lines()
            .find(|l| l.contains("[read receipt "))
            .and_then(|l| l.split("[read receipt ").nth(1))
            .and_then(|rest| rest.split([' ', ']']).next())
            .expect("the trailer carries the id");
        assert!(!id.is_empty(), "the receipt id must be non-empty");
    }

    /// Backlog c19ec2ff (c): a stringified JSON array for `files` is
    /// accepted — a clean parse is spliced through and the files read
    /// (the 2027-01-25 session hit this 3x in one turn and got the
    /// generic "an argument must be a sequence, not a string" every
    /// time, causing identical re-emissions).
    #[tokio::test]
    async fn stringified_files_array_reads_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha content").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta content").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": "[{\"path\": \"a.txt\"}, {\"path\": \"b.txt\"}]"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("alpha content") && result.output.contains("beta content"),
            "both files read from the repaired array: {}",
            result.output
        );
        // A clean repair is silent — no NOTE clutter.
        assert!(
            !result.output.contains("NOTE"),
            "clean parse needs no note: {}",
            result.output
        );
    }

    /// Backlog c19ec2ff (c): a MALFORMED stringified array still reads
    /// its recoverable specs — well-formed ones, and ones whose only defect
    /// is the observed stray quote after a number (2027-01-25, and again on
    /// every spec of one call) — with a NOTE naming the skipped fragment
    /// and the byte position of the parse failure.
    #[tokio::test]
    async fn malformed_stringified_files_recovers_well_formed_specs_with_note() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha content").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta content").unwrap();
        std::fs::write(dir.path().join("c.txt"), "gamma content").unwrap();
        let tool = make_tool(dir.path());
        // a.txt's spec carries the stray-quote defect (max_lines: 5");
        // b.txt's spec is well-formed; c.txt's is missing its value.
        let result = tool
            .execute(json!({
                "files": "[{\"path\": \"a.txt\", \"max_lines\": 5\"}, {\"path\": \"b.txt\"}, {\"path\": \"c.txt\", \"max_lines\": }]"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("alpha content") && result.output.contains("beta content"),
            "the repaired and well-formed specs are read: {}",
            result.output
        );
        assert!(
            !result.output.contains("gamma content"),
            "the malformed spec must NOT be read: {}",
            result.output
        );
        assert!(
            result.output.contains("repaired files argument"),
            "the NOTE names the repair: {}",
            result.output
        );
        assert!(
            result.output.contains("byte") && result.output.contains("c.txt"),
            "the NOTE names the skipped fragment and the parse byte position: {}",
            result.output
        );
    }

    /// Every spec carrying a stray quote after a number (observed live)
    /// is repaired silently: all files read, no NOTE.
    #[tokio::test]
    async fn stray_quote_on_every_spec_reads_all_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one
two
three").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta content").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": "[{\"max_lines\": 1, \"path\": \"a.txt\", \"start_line\": 2\"}, {\"max_lines\": 100, \"path\": \"b.txt\", \"start_line\": 1\"}]"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("two") && !result.output.contains("three"), "{}", result.output);
        assert!(result.output.contains("beta content"), "{}", result.output);
        assert!(!result.output.contains("NOTE"), "{}", result.output);
    }

    /// Backlog c19ec2ff (c): an unrecoverable string fails with the byte
    /// position of the parse error and the expected native-array shape —
    /// not the generic sequence error that taught nothing.
    #[tokio::test]
    async fn unrecoverable_stringified_files_error_names_position() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"files": "not json at all"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("arrived as a STRING")
                && result.output.contains("byte")
                && result.output.contains("\"path\""),
            "the error names files, the parse byte position, and the expected shape: {}",
            result.output
        );
    }

    #[test]
    fn schema_collapses_to_the_files_form() {
        // Backlog 26cdbaf8: the dual input forms (files array + path
        // shorthand, neither required) were the ambiguity behind the
        // empty-argument failures — the advertised schema now offers ONE
        // form, required, with the recovery rule and example in the
        // description. The path shorthand stays as unadvertised compat
        // absorption in execute().
        let tool = make_tool(std::path::Path::new("."));
        let schema = tool.schema();
        let props = &schema.parameters["properties"];
        assert!(
            props.get("path").is_none()
                && props.get("start_line").is_none()
                && props.get("max_lines").is_none(),
            "the shorthand params are no longer advertised: {props}"
        );
        assert_eq!(schema.parameters["required"], json!(["files"]));
        assert!(
            schema.description.starts_with("Always pass `files`"),
            "the contract sentence LEADS the description: {}",
            schema.description
        );
        // The content-first clause lives once in TOOL_CALL_DISCIPLINE
        // (src/agent/prompt.rs) — the per-tool copy was the trim's point.
        assert!(
            !schema.description.contains("If you catch yourself"),
            "the content-first clause lives once in TOOL_CALL_DISCIPLINE, not per tool: {}",
            schema.description
        );
        // A model wrote "big.rs:115-20" for 20 lines from 115 once this was cut.
        assert!(
            schema.description.contains("\"path:start+count\" (count lines from start)"),
            "the start+count form must say what count means: {}",
            schema.description
        );
        assert_eq!(
            schema.parameters["properties"]["files"]["minItems"],
            json!(1),
            "the files array advertises minItems: 1"
        );
        assert!(
            schema.description.contains("Always pass `files`"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("No zero-argument form"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("rewrite the full call"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("\"files\":[\"src/main.rs\",\"src/lib.rs:120+40\"]"),
            "the inline example shows the exact call shape: {}",
            schema.description
        );
        // Plain strings, not objects: models that stringify nested object
        // arrays (and grow a stray quote after a number) emit strings
        // natively — and the schema no longer advertises the repair.
        assert_eq!(schema.parameters["properties"]["files"]["items"], json!({"type": "string"}));
        assert!(
            !schema.parameters["properties"]["files"]["description"]
                .as_str()
                .unwrap()
                .contains("stringified"),
            "the stringified-array repair stays silent"
        );
    }

    #[test]
    fn compact_specs_split_only_a_trailing_line_range() {
        let parts = |s: &str| {
            let spec = parse_compact_spec(s).unwrap();
            (spec.path, spec.start_line, spec.max_lines)
        };
        assert_eq!(parts("src/a.rs"), ("src/a.rs".into(), None, None));
        assert_eq!(parts("src/a.rs:10"), ("src/a.rs".into(), Some(10), None));
        assert_eq!(parts("src/a.rs:4270+220"), ("src/a.rs".into(), Some(4270), Some(220)));
        assert_eq!(parts("src/a.rs:10-19"), ("src/a.rs".into(), Some(10), Some(10)));
        assert_eq!(parts(" src/a.rs:5-5 "), ("src/a.rs".into(), Some(5), Some(1)));
        // A colon that is not a trailing range stays in the path.
        assert_eq!(parts("C:\\repo\\a.rs"), ("C:\\repo\\a.rs".into(), None, None));
        assert_eq!(parts("C:\\repo\\a.rs:7+3"), ("C:\\repo\\a.rs".into(), Some(7), Some(3)));
        assert_eq!(parts("notes:draft.md"), ("notes:draft.md".into(), None, None));
        assert_eq!(parts("a.rs:10+x"), ("a.rs:10+x".into(), None, None));
        let backwards = parse_compact_spec("a.rs:20-10").unwrap_err();
        assert!(backwards.contains("a.rs:20-10"), "{backwards}");
    }

    #[test]
    fn every_reader_sees_compact_and_object_specs_alike() {
        let args = json!({"files": ["a.rs:3+2", {"path": "b.rs", "start_line": 4}, 7]});
        let specs: Vec<_> = read_specs(&args)
            .into_iter()
            .map(|s| (s.path, s.start_line, s.max_lines))
            .collect();
        assert_eq!(
            specs,
            vec![("a.rs".to_string(), Some(3), Some(2)), ("b.rs".to_string(), Some(4), None)]
        );
        let shorthand = read_specs(&json!({"path": "c.rs", "max_lines": 9}));
        assert_eq!(shorthand.len(), 1);
        assert_eq!((shorthand[0].path.as_str(), shorthand[0].max_lines), ("c.rs", Some(9)));
    }

    #[tokio::test]
    async fn compact_string_specs_read_their_slices() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"files": ["a.txt:2+2", "a.txt:4-5", "b.txt"]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("2: two\n3: three"), "{}", result.output);
        assert!(!result.output.contains("1: one"), "{}", result.output);
        assert!(result.output.contains("4: four\n5: five"), "{}", result.output);
        assert!(result.output.contains("beta"), "{}", result.output);

        let backwards = tool.execute(json!({"files": ["a.txt:5-2"]})).await;
        assert!(!backwards.success);
        assert!(backwards.output.contains("before its start line"), "{}", backwards.output);
    }

    #[tokio::test]
    async fn empty_call_error_carries_the_recovery_hint() {
        // Backlog 26cdbaf8: the empty-argument call (the observed failure —
        // a well-formed call followed by a drained one) errors with the
        // recovery rule riding the error itself, so the FIRST retry
        // succeeds instead of waiting for the circuit breaker.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("files"),
            "the error names the missing parameter: {}",
            result.output
        );
        assert!(
            result.output.contains("rewrite the full call"),
            "the recovery hint rides the error: {}",
            result.output
        );
        assert!(
            result.output.contains("do not resend the empty shape"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn reads_two_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld").unwrap();
        std::fs::write(dir.path().join("b.txt"), "foo\nbar").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt"},
                    {"path": "b.txt"}
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("=== a.txt"));
        assert!(result.output.contains("hello"));
        assert!(result.output.contains("=== b.txt"));
        assert!(result.output.contains("foo"));
    }

    #[tokio::test]
    async fn per_file_line_range() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "l1\nl2\nl3\nl4\nl5").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x\ny\nz").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt", "start_line": 2, "max_lines": 2},
                    {"path": "b.txt"}
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // File A: only lines 2-3.
        assert!(result.output.contains("\n2: l2"));
        assert!(result.output.contains("\n3: l3"));
        assert!(!result.output.contains("l1"));
        assert!(!result.output.contains("l4"));
        // File B: full.
        assert!(result.output.contains("\n1: x"));
    }

    #[tokio::test]
    async fn one_missing_file_doesnt_fail_call() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt"},
                    {"path": "missing.txt"}
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // The good file is still returned.
        assert!(result.output.contains("hello"));
        // The missing file is reported inline.
        assert!(result.output.contains("missing.txt"));
        assert!(result.output.contains("failed to read"));
    }

    #[tokio::test]
    async fn directory_spec_returns_directory_hint_not_raw_os_error() {
        // Regression (backlog #89, review finding L1): read_to_string on a
        // directory surfaced the raw Windows "Access is denied. (os error
        // 5)" — the per-spec result must name the path kind instead, and a
        // bad spec must not blank out the sibling reads. Since 2026-01-03
        // the directory spec returns a full sorted listing (backlog
        // 2026-12-29 session) — still inline, still non-fatal.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(dir.path().join("plans")).unwrap();
        std::fs::write(dir.path().join("plans/p1.md"), "x").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt"},
                    {"path": "plans"}
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // The good file is still returned.
        assert!(result.output.contains("hello"), "{}", result.output);
        // The directory spec lists inline — no raw OS error, no failure.
        assert!(
            result.output.contains("=== plans (directory"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("p1.md"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn directory_path_returns_listing() {
        // Backlog (2026-12-29 session, plan 72329f2c): "what files are in
        // this dir" had no affordance — read_files errored on directories
        // ("is a directory, not a file") and search's 100-match cap +
        // display truncation made enumeration unreliable. A directory spec
        // must return a sorted listing instead of the error.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plans")).unwrap();
        std::fs::write(dir.path().join("plans/b.md"), "hello").unwrap();
        std::fs::write(dir.path().join("plans/a.md"), "world").unwrap();
        std::fs::create_dir_all(dir.path().join("plans/sub")).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "plans"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("plans (directory"),
            "the spec renders as a directory listing: {}",
            result.output
        );
        assert!(
            result.output.contains("a.md"),
            "entries are listed: {}",
            result.output
        );
        assert!(
            result.output.contains("b.md"),
            "entries are listed: {}",
            result.output
        );
        // Sorted: a.md before b.md.
        let a = result.output.find("a.md").unwrap();
        let b = result.output.find("b.md").unwrap();
        assert!(a < b, "entries are sorted: {}", result.output);
        // Subdirectories are marked as directories.
        assert!(
            result.output.contains("sub") && result.output.contains("dir"),
            "subdirectory entries are marked as dirs: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn directory_listing_caps_entries() {
        // Review L5 (2026-01-03): a huge directory must not flood the
        // conversation — the listing caps at LISTING_ENTRY_CAP entries with
        // a remainder note; the header still shows the true count.
        let dir = tempdir().unwrap();
        let sub = dir.path().join("big");
        std::fs::create_dir_all(&sub).unwrap();
        for i in 0..350 {
            std::fs::write(sub.join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "big"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("(directory, 350 entries)"),
            "the header shows the true count: {}",
            result.output
        );
        assert!(
            !result.output.contains("f349"),
            "beyond the cap, the tail entry is hidden: {}",
            result.output
        );
        assert!(
            result.output.contains("and 50 more entries"),
            "the cap note names the remainder: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn empty_files_array_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"files": []})).await;
        assert!(!result.success);
        assert!(result.output.contains("empty"));
    }

    #[tokio::test]
    async fn top_level_path_is_read_as_a_single_file() {
        // This shape used to be an error that pointed at `file_read`. Now it
        // IS the single-file call — absorbing it is what let that tool go
        // away, and it turns the most natural shape from a failed call plus a
        // retry into a working read.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "one
two
three
",
        )
        .unwrap();
        let tool = make_tool(dir.path());

        let result = tool.execute(json!({"path": "a.txt"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("one"), "{}", result.output);
        assert!(result.output.contains("three"), "{}", result.output);

        // The slice arguments come along for the ride.
        let sliced = tool
            .execute(json!({"path": "a.txt", "start_line": 2, "max_lines": 1}))
            .await;
        assert!(sliced.success, "{}", sliced.output);
        assert!(sliced.output.contains("two"), "{}", sliced.output);
        assert!(!sliced.output.contains("three"), "{}", sliced.output);

        // An explicit `files` array still wins — the shorthand only applies
        // when there is no batch to read.
        let batch = tool
            .execute(json!({"path": "ignored.txt", "files": [{"path": "a.txt"}]}))
            .await;
        assert!(batch.success, "{}", batch.output);
        assert!(!batch.output.contains("ignored.txt"), "{}", batch.output);
    }

    #[tokio::test]
    async fn too_many_files_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let files: Vec<serde_json::Value> = (0..MAX_FILES + 1)
            .map(|i| json!({"path": format!("f{i}.txt")}))
            .collect();
        let result = tool.execute(json!({"files": files})).await;
        assert!(!result.success);
        assert!(result.output.contains("too many files"));
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let outside = dir.path().parent().unwrap().join("outside_batch.txt");
        std::fs::write(&outside, "secret").unwrap();
        std::fs::write(dir.path().join("a.txt"), "ok").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt"},
                    {"path": "../outside_batch.txt"}
                ]
            }))
            .await;
        // The call succeeds (per-file error isolation); the bad path is inline.
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("ok"));
        assert!(result.output.contains("path validation failed"));
    }

    #[tokio::test]
    async fn line_numbers_reflect_original_positions() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\nfive").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [{"path": "a.txt", "start_line": 3, "max_lines": 2}]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // Line numbers are the original file positions (3, 4), not 1, 2.
        assert!(result.output.contains("\n3: three"));
        assert!(result.output.contains("\n4: four"));
        assert!(!result.output.contains("\n1: "));
    }

    #[tokio::test]
    async fn header_shows_line_range() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\nd\ne").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [{"path": "a.txt", "start_line": 2, "max_lines": 2}]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // Header reports the line range and total.
        assert!(result.output.contains("lines 2-3 of 5"));
    }

    #[tokio::test]
    async fn per_file_byte_cap_truncates() {
        // A single file with one very long line (~150 KB) exceeding
        // DEFAULT_MAX_BYTES (100 KB). The section body is truncated and a
        // truncation note is appended.
        let dir = tempdir().unwrap();
        let long_line = "x".repeat(150_000);
        std::fs::write(dir.path().join("long.txt"), &long_line).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"files": [{"path": "long.txt"}]})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("truncated"),
            "expected byte truncation note"
        );
    }

    #[tokio::test]
    async fn total_byte_cap_truncates() {
        // Multiple files whose joined output exceeds TOTAL_BYTE_CAP (512 KB).
        // 6 files × ~100 KB each ≈ 600 KB > 512 KB.
        let dir = tempdir().unwrap();
        let line = "y".repeat(100_000);
        for i in 0..6 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), &line).unwrap();
        }
        let tool = make_tool(dir.path());
        let files: Vec<serde_json::Value> = (0..6)
            .map(|i| json!({"path": format!("f{i}.txt")}))
            .collect();
        let result = tool.execute(json!({"files": files})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("total output exceeded size limit"),
            "expected total-cap truncation note"
        );
    }

    #[tokio::test]
    async fn default_line_cap_truncation_note() {
        // A >500-line file with no max_lines → default cap fires, note appended.
        let dir = tempdir().unwrap();
        let content: String = (0..3000)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("big.txt"), content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"files": [{"path": "big.txt"}]})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("truncated"));
        assert!(result.output.contains("3000 lines total"));
        assert!(result.output.contains("showed 500"));
    }

    #[tokio::test]
    async fn empty_range_header_when_start_beyond_eof() {
        // start_line past the file's end → "empty range" header, no panic.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"files": [{"path": "a.txt", "start_line": 99}]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("empty range"));
    }

    #[tokio::test]
    async fn start_line_zero_reads_from_top() {
        // start_line: 0 is treated as line 1 (saturating_sub), reads from top.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"files": [{"path": "a.txt", "start_line": 0}]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("\n1: a"));
    }

    #[tokio::test]
    async fn max_lines_zero_yields_empty_range() {
        // max_lines: 0 → no lines shown → "empty range" header, no error.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"files": [{"path": "a.txt", "max_lines": 0}]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("empty range"));
    }

    #[tokio::test]
    async fn multibyte_utf8_under_caps_no_panic() {
        // Multi-byte UTF-8 content large enough to hit the per-file byte cap —
        // must not panic (regression for char-boundary truncation).
        let dir = tempdir().unwrap();
        // ~180 KB of 3-byte chars → exceeds DEFAULT_MAX_BYTES (100 KB).
        let content = "→→→".repeat(20_000);
        std::fs::write(dir.path().join("multibyte.txt"), &content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"files": [{"path": "multibyte.txt"}]}))
            .await;
        assert!(result.success, "{}", result.output.len());
        // The per-file byte cap should have fired (150 KB > 100 KB).
        assert!(
            result.output.contains("truncated"),
            "expected byte truncation note (output len = {})",
            result.output.len()
        );
        // Output must be valid UTF-8 (no panic, no replacement chars from a bad cut).
        assert!(result.output.contains("→→→"));
    }

    #[tokio::test]
    async fn big_indexed_rust_file_whole_read_nudges_with_symbol_count() {
        // A whole-file read (>300 lines) of an indexed .rs file prepends the
        // SYMBOL NUDGE line above the results, with the indexed symbol count.
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 40);
        let tool = make_indexed_tool(dir.path());
        let result = tool.execute(json!({"path": "big.rs"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.starts_with("SYMBOL NUDGE: big.rs has "),
            "{}",
            result.output
        );
        assert!(
            result
                .output
                .contains("indexed symbols — graph_context(id)"),
            "{}",
            result.output
        );
        // The file content still follows below the note.
        assert!(
            result.output.contains("=== big.rs (lines 1-"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn sliced_read_never_nudges() {
        // Same big indexed file, but a sliced read (start_line OR max_lines)
        // gets NO nudge — only whole-file reads qualify.
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 40);
        let tool = make_indexed_tool(dir.path());
        for spec in [
            json!({"path": "big.rs", "start_line": 2}),
            json!({"path": "big.rs", "max_lines": 500}),
        ] {
            let result = tool.execute(json!({"files": [spec]})).await;
            assert!(result.success, "{}", result.output);
            assert!(
                !result.output.contains("SYMBOL NUDGE"),
                "sliced read must not nudge: {}",
                result.output
            );
        }
    }

    #[tokio::test]
    async fn batch_whole_reads_join_into_one_nudge() {
        // The `files` batch form (not the single-file shorthand) with TWO
        // qualifying files prepends ONE nudge line naming both, joined
        // with "; " — the "SYMBOL NUDGE:" marker appears exactly once.
        // A big .py file qualifies too: the nudge covers every indexed
        // language, not just Rust.
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "a.rs", 40);
        write_big_rust_file(dir.path(), "b.rs", 40);
        let mut py = String::from("#!/usr/bin/env python3\n");
        for i in 0..150 {
            py.push_str(&format!("def py_fn_{i}():\n    pass\n\n"));
        }
        std::fs::write(dir.path().join("big.py"), py).unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(
                json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}, {"path": "big.py"}]}),
            )
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.starts_with("SYMBOL NUDGE: a.rs has "),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("; b.rs has "),
            "qualifying files are joined into ONE nudge: {}",
            result.output
        );
        assert!(
            result.output.contains("; big.py has "),
            "any indexed language qualifies for the nudge: {}",
            result.output
        );
        assert_eq!(
            result.output.matches("SYMBOL NUDGE:").count(),
            1,
            "the marker prefix appears exactly once: {}",
            result.output
        );
        assert!(
            result.output.contains("=== a.rs") && result.output.contains("=== b.rs"),
            "both sections follow the nudge: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn small_file_non_source_or_no_graph_never_nudges() {
        // (a) Small indexed .rs file (under 300 lines): no note.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("small.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool.execute(json!({"path": "small.rs"})).await;
        assert!(result.success, "{}", result.output);
        assert!(!result.output.contains("SYMBOL NUDGE"), "{}", result.output);

        // (b) A big indexed NON-source file (.txt): no note.
        let big_txt: String = (0..400)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("big.txt"), &big_txt).unwrap();
        let result = tool.execute(json!({"path": "big.txt"})).await;
        assert!(result.success, "{}", result.output);
        assert!(!result.output.contains("SYMBOL NUDGE"), "{}", result.output);

        // (c) Big indexed .rs file WITHOUT a graph wired (tests /
        // codegraph-opted-out projects): no note, unchanged behavior.
        write_big_rust_file(dir.path(), "big2.rs", 40);
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "big2.rs"})).await;
        assert!(result.success, "{}", result.output);
        assert!(!result.output.contains("SYMBOL NUDGE"), "{}", result.output);
    }

    /// A delta-lever tool: the flag on, everything else default. Mirrors
    /// the factory wiring (`with_optimizer` on a sandbox-bound tool).
    fn make_delta_tool(dir: &Path) -> ReadFilesTool {
        let cfg = crate::config::OptimizerConfig {
            delta_reads: true,
            ..Default::default()
        };
        ReadFilesTool::new(Sandbox::new(dir).unwrap())
            .with_optimizer(Arc::new(std::sync::RwLock::new(cfg)))
    }

    /// The per-read receipt trailer (backlog 3fdc605f) carries a fresh id
    /// on EVERY read — strip it for the byte-identity comparisons below;
    /// the CONTENT is the pin, the id is necessarily per-read.
    fn without_receipt_trailer(output: &str) -> String {
        output
            .lines()
            .filter(|l| !l.contains("[read receipt "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn delta_lever_off_serves_full_content_identically() {
        // No optimizer attached (CLI contract builds, pre-lever tests):
        // re-reads serve the full content again, byte-identical to the
        // pre-lever build, and no savings data rides the result — the
        // regression pin for the ABSENT-optimizer path (an explicit
        // `delta_reads = false` is pinned by the test below). The
        // per-read receipt trailer (backlog 3fdc605f) is the one new
        // line — its id differs per read, so the comparison normalizes
        // it away (content identity is the pin).
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let r1 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r1.success && r2.success);
        assert_eq!(
            without_receipt_trailer(&r1.output),
            without_receipt_trailer(&r2.output),
            "flag off: byte-identical re-reads"
        );
        assert!(r1.data.is_none() && r2.data.is_none());
        assert!(r1.output.contains("fn b() {}"));
    }

    #[tokio::test]
    async fn flag_off_then_flipped_on_populates_cache_only_under_the_flag() {
        // "Cache populated only when flag on": with the optimizer
        // ATTACHED but `delta_reads` off, re-reads stay byte-identical and
        // the cache stays untouched — proven by flipping the flag on
        // mid-session (a config save; the flag is re-read per call): the
        // first flag-on read must serve the FULL content (no baseline was
        // recorded while off), and only the one after serves the skeleton.
        let dir = tempdir().unwrap();
        // Explicit opt-out: the levers default ON (2027-01-25), so the
        // flag-off half of this test has to say so.
        let cfg = Arc::new(std::sync::RwLock::new(crate::config::OptimizerConfig {
            delta_reads: false,
            ..Default::default()
        }));
        let tool = ReadFilesTool::new(Sandbox::new(dir.path()).unwrap())
            .with_optimizer(Arc::clone(&cfg));
        let src: String = (0..60)
            .map(|i| {
                format!(
                    "fn f_{i}() {{\n    let x = {i} + 1;\n    let y = {i} * 2;\n    \
                     let z = x + y;\n}}\n"
                )
            })
            .collect();
        std::fs::write(dir.path().join("a.rs"), &src).unwrap();
        let r1 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r1.success && r2.success);
        assert_eq!(
            without_receipt_trailer(&r1.output),
            without_receipt_trailer(&r2.output),
            "flag off: byte-identical re-reads"
        );
        assert!(r1.data.is_none() && r2.data.is_none());
        // Flip the flag on mid-session: no cache entry may exist yet.
        cfg.write().unwrap().delta_reads = true;
        let r3 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r3.success, "{}", r3.output);
        assert!(r3.data.is_none(), "no baseline was recorded while off");
        assert!(r3.output.contains("let z = x + y;"), "{}", r3.output);
        assert!(!r3.output.contains("unchanged since your last read"), "{}", r3.output);
        // Only now does a re-read serve the skeleton.
        let r4 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r4.output.contains("unchanged since your last read"), "{}", r4.output);
    }

    #[tokio::test]
    async fn tiny_file_reread_serves_full_even_with_the_flag_on() {
        // The reduced-never-costs-more invariant: on a tiny file the
        // note + numbering boilerplate exceeds the content itself, so
        // even with the flag on an unchanged re-read serves the FULL
        // content — no skeleton, no savings row.
        let dir = tempdir().unwrap();
        let tool = make_delta_tool(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r2.success, "{}", r2.output);
        assert!(r2.output.contains("fn b() {}"), "{}", r2.output);
        assert!(!r2.output.contains("unchanged since your last read"), "{}", r2.output);
        assert!(r2.data.is_none());
    }

    #[tokio::test]
    async fn unchanged_reread_serves_skeleton() {
        let dir = tempdir().unwrap();
        let tool = make_delta_tool(dir.path());
        // A file with real bodies — the skeleton only pays off from this
        // size up; a tiny file's note boilerplate exceeds the content and
        // serves in full (pinned by tiny_file_reread below).
        let src: String = (0..60)
            .map(|i| {
                format!(
                    "fn f_{i}() {{\n    let x = {i} + 1;\n    let y = {i} * 2;\n    \
                     let z = x + y;\n}}\n"
                )
            })
            .collect();
        std::fs::write(dir.path().join("a.rs"), &src).unwrap();
        // First read: full content, no savings, baseline established.
        let r1 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r1.success, "{}", r1.output);
        assert!(r1.data.is_none());
        assert!(r1.output.contains("let z = x + y;"));
        // Second read, file unchanged: signature skeleton.
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r2.success, "{}", r2.output);
        assert!(r2.output.contains("unchanged since your last read"), "{}", r2.output);
        assert!(r2.output.contains("fn f_59() {"), "{}", r2.output);
        assert!(!r2.output.contains("let z = x + y;"), "{}", r2.output);
        assert!(r2.output.contains("Re-read a \"path:start+count\" range"), "{}", r2.output);
        let savings = r2.data.as_ref().unwrap()["savings"].as_array().unwrap();
        assert_eq!(savings.len(), 1);
        assert_eq!(savings[0]["kind"], "skeleton");
        assert_eq!(savings[0]["detail"], "a.rs");
        let before = savings[0]["tokens_before"].as_i64().unwrap();
        let after = savings[0]["tokens_after"].as_i64().unwrap();
        assert!(after < before, "skeleton must be smaller: {before} -> {after}");
        assert_eq!(savings[0]["measured"], false);
    }

    #[tokio::test]
    async fn changed_reread_serves_diff() {
        let dir = tempdir().unwrap();
        let tool = make_delta_tool(dir.path());
        let src: String = (0..60)
            .map(|i| format!("fn f_{i}() {{\n    let x = {i} + 1;\n    let y = {i} * 2;\n}}\n"))
            .collect();
        std::fs::write(dir.path().join("a.rs"), &src).unwrap();
        let r1 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r1.data.is_none());
        let changed = src.replace("let x = 30 + 1;", "let x = 30 + 999;");
        std::fs::write(dir.path().join("a.rs"), &changed).unwrap();
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r2.success, "{}", r2.output);
        assert!(r2.output.contains("changed since your last read"), "{}", r2.output);
        assert!(r2.output.contains("-    let x = 30 + 1;"), "{}", r2.output);
        assert!(r2.output.contains("+    let x = 30 + 999;"), "{}", r2.output);
        let savings = r2.data.as_ref().unwrap()["savings"].as_array().unwrap();
        assert_eq!(savings.len(), 1);
        assert_eq!(savings[0]["kind"], "delta_read");
        assert_eq!(savings[0]["detail"], "a.rs");
        assert!(
            savings[0]["tokens_after"].as_i64().unwrap()
                < savings[0]["tokens_before"].as_i64().unwrap()
        );
    }

    #[tokio::test]
    async fn mostly_rewritten_reread_serves_full() {
        // A file rewritten end-to-end serves the full content — a huge
        // diff costs more than re-reading. No savings row (nothing was
        // served in a reduced form).
        let dir = tempdir().unwrap();
        let tool = make_delta_tool(dir.path());
        // Under the 500-line default serve cap so the whole file is in
        // the output (300 lines).
        let old: String = (0..100).map(|i| format!("fn o_{i}() {{\n    body_{i}\n}}\n")).collect();
        std::fs::write(dir.path().join("a.rs"), &old).unwrap();
        tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        let new: String = (0..100).map(|i| format!("fn n_{i}() {{\n    other_{i}\n}}\n")).collect();
        std::fs::write(dir.path().join("a.rs"), &new).unwrap();
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r2.success, "{}", r2.output);
        assert!(r2.output.contains("fn n_99()"), "{}", r2.output);
        assert!(!r2.output.contains("changed since your last read"), "{}", r2.output);
        assert!(r2.data.is_none());
    }

    #[tokio::test]
    async fn ranged_reread_serves_full_lines() {
        // The escape hatch: ranged reads always serve exact lines, even on
        // an unchanged file the agent just read whole.
        let dir = tempdir().unwrap();
        let tool = make_delta_tool(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        let r2 = tool
            .execute(json!({"files":[{"path":"a.rs","start_line":2,"max_lines":1}]}))
            .await;
        assert!(r2.success, "{}", r2.output);
        assert!(r2.output.contains("\n2: fn b() {}"), "{}", r2.output);
        assert!(!r2.output.contains("unchanged since your last read"), "{}", r2.output);
        assert!(r2.data.is_none());
    }

    #[tokio::test]
    async fn numbered_lines_drop_padding_and_trailing_whitespace_but_keep_indentation() {
        let dir = tempdir().unwrap();
        let content = "fn f() {  \n\tlet x = 1;\t\n    \n    g();\n}\n";
        std::fs::write(dir.path().join("a.rs"), content).unwrap();
        let result = make_tool(dir.path())
            .execute(json!({"files":[{"path":"a.rs"}]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            without_receipt_trailer(&result.output)
                .ends_with("\n1: fn f() {\n2: \tlet x = 1;\n3:\n4:     g();\n5: }"),
            "{}",
            result.output
        );
        // An anchor copied from that view (prefixes off) still lands on
        // the lines whose trailing whitespace the view dropped.
        let anchor = "fn f() {\n\tlet x = 1;\n\n    g();";
        let outcome = crate::tool::agent::edit_ops::literal_splice(
            content,
            anchor,
            "fn f() {\n\tlet x = 2;\n\n    g();",
            false,
            1,
        )
        .expect("splice")
        .expect("the copied anchor matches");
        assert_eq!(outcome.new_content, "fn f() {\n\tlet x = 2;\n\n    g();\n}\n");
    }

    #[tokio::test]
    async fn first_whole_read_of_a_big_file_serves_its_outline() {
        let dir = tempdir().unwrap();
        // 50 functions = 600 lines, over the 500-line outline threshold.
        write_big_rust_file(dir.path(), "big.rs", 50);
        let total = std::fs::read_to_string(dir.path().join("big.rs"))
            .unwrap()
            .lines()
            .count();
        assert!(total > OUTLINE_MIN_LINES, "{total}");
        let tool = make_delta_tool(dir.path());
        let whole = json!({"files":[{"path":"big.rs"}]});

        let r1 = tool.execute(whole.clone()).await;
        assert!(r1.success, "{}", r1.output);
        assert!(r1.output.contains(&format!("signature lines of {total}) ===")), "{}", r1.output);
        assert!(r1.output.contains("\n1: pub fn sym_0(x: u32) -> u32 {"), "{}", r1.output);
        assert!(r1.output.contains("pub fn sym_39("), "{}", r1.output);
        assert!(!r1.output.contains("let a = x + 0;"), "{}", r1.output);
        assert!(r1.output.contains("Read the region you need as a \"path:start+count\" range"));
        let savings = r1.data.as_ref().unwrap()["savings"].as_array().unwrap();
        assert_eq!(savings[0]["kind"], "outline");
        assert!(savings[0]["tokens_after"].as_i64() < savings[0]["tokens_before"].as_i64());

        // The region the agent asks for next comes back exact.
        let ranged = tool
            .execute(json!({"files":[{"path":"big.rs","start_line":2,"max_lines":1}]}))
            .await;
        assert!(
            without_receipt_trailer(&ranged.output).ends_with("\n2:     let a = x + 0;"),
            "{}",
            ranged.output
        );

        // No baseline was recorded: a whole re-read is the outline again,
        // never a skeleton of, or a diff against, content the agent never saw.
        let r2 = tool.execute(whole.clone()).await;
        assert_eq!(r2.output, r1.output);
        let src = std::fs::read_to_string(dir.path().join("big.rs")).unwrap();
        std::fs::write(dir.path().join("big.rs"), src.replace("x + 7;", "x + 70;")).unwrap();
        let r3 = tool.execute(whole.clone()).await;
        assert!(r3.output.contains("(outline:"), "{}", r3.output);
        assert!(!r3.output.contains("x + 70;"), "{}", r3.output);

        // Lever off: the whole file as before.
        let full = make_tool(dir.path()).execute(whole).await;
        assert!(full.output.contains("\n2:     let a = x + 0;"), "{}", full.output);
        assert!(full.data.is_none());
    }

    #[tokio::test]
    async fn a_big_file_without_signatures_is_not_outlined() {
        // 600 lines is over OUTLINE_MIN_LINES, but there are no signatures to
        // extract: the outline fallback serves the ordinary whole read — no
        // skeleton, no `outline` savings event. (The 500-line read cap is a
        // separate rule: a whole read is capped at DEFAULT_MAX_LINES.)
        let dir = tempdir().unwrap();
        let rows: String = (0..600).map(|i| format!("    {i},\n")).collect();
        std::fs::write(dir.path().join("table.rs"), &rows).unwrap();
        let result = make_delta_tool(dir.path())
            .execute(json!({"files":[{"path":"table.rs"}]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("\n500:     499,"), "{}", result.output);
        assert!(!result.output.contains("signature"), "{}", result.output);
        assert!(result.data.is_none(), "no outline event without signatures");
    }

    #[tokio::test]
    async fn a_file_at_or_under_the_cap_is_served_whole_on_first_read() {
        // 40 functions = 480 lines, at or under OUTLINE_MIN_LINES: a whole
        // read serves the whole file, because an outline at this size would
        // only force a follow-up ranged read. The baseline is still
        // recorded, so a second whole read serves the skeleton.
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "mid.rs", 40);
        let total = std::fs::read_to_string(dir.path().join("mid.rs"))
            .unwrap()
            .lines()
            .count();
        assert!(total <= OUTLINE_MIN_LINES, "{total}");
        let tool = make_delta_tool(dir.path());
        let whole = json!({"files":[{"path":"mid.rs"}]});
        let r1 = tool.execute(whole.clone()).await;
        assert!(r1.success, "{}", r1.output);
        assert!(r1.output.contains("let a = x + 0;"), "{}", r1.output);
        assert!(!r1.output.contains("signature lines of"), "{}", r1.output);
        assert!(r1.data.is_none(), "no outline event for a whole-served file");
        let r2 = tool.execute(whole).await;
        assert!(
            r2.output.contains("unchanged since your last read"),
            "{}",
            r2.output
        );
    }

    #[tokio::test]
    async fn a_ranged_read_after_an_outline_records_a_followup() {
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 50);
        let tool = make_delta_tool(dir.path());
        let whole = tool.execute(json!({"files":[{"path":"big.rs"}]})).await;
        assert!(whole.success, "{}", whole.output);
        assert_eq!(whole.data.as_ref().unwrap()["savings"][0]["kind"], "outline");
        let ranged = tool
            .execute(json!({"files":[{"path":"big.rs","start_line":2,"max_lines":3}]}))
            .await;
        assert!(ranged.success, "{}", ranged.output);
        let savings = ranged.data.as_ref().unwrap()["savings"].as_array().unwrap();
        assert_eq!(savings.len(), 1, "{savings:?}");
        assert_eq!(savings[0]["kind"], "outline_followup");
        assert_eq!(savings[0]["detail"], "big.rs");
        assert_eq!(savings[0]["tokens_before"], 0);
        assert!(savings[0]["tokens_after"].as_i64().unwrap() > 0, "{savings:?}");
    }

    #[tokio::test]
    async fn followups_stop_at_what_the_outline_saved() {
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 50);
        let tool = make_delta_tool(dir.path());
        let whole = tool.execute(json!({"files":[{"path":"big.rs"}]})).await;
        let outline = &whole.data.as_ref().unwrap()["savings"][0];
        let saved = outline["tokens_before"].as_i64().unwrap()
            - outline["tokens_after"].as_i64().unwrap();
        assert!(saved > 0, "{outline}");
        let range = json!({"files":[{"path":"big.rs","start_line":2,"max_lines":400}]});
        let mut charged = 0i64;
        for _ in 0..40 {
            if charged >= saved {
                break;
            }
            let r = tool.execute(range.clone()).await;
            let sum: i64 = r
                .data
                .as_ref()
                .and_then(|d| d["savings"].as_array())
                .map(|events| {
                    events
                        .iter()
                        .filter(|e| e["kind"] == "outline_followup")
                        .map(|e| e["tokens_after"].as_i64().unwrap())
                        .sum()
                })
                .unwrap_or(0);
            charged += sum;
        }
        assert_eq!(charged, saved, "charged exactly what the outline saved");
        let after = tool.execute(range).await;
        assert!(after.data.is_none(), "no charge once the debt is paid");
    }

    #[tokio::test]
    async fn a_range_beyond_the_whole_read_cap_is_not_charged() {
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 50);
        let tool = make_delta_tool(dir.path());
        let _ = tool.execute(json!({"files":[{"path":"big.rs"}]})).await;
        let r = tool
            .execute(json!({"files":[{"path":"big.rs","start_line":501,"max_lines":3}]}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.data.is_none(),
            "a range a whole read could not have served is not charged"
        );
    }

    #[tokio::test]
    async fn a_range_of_a_file_never_outlined_is_not_charged() {
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 50);
        let tool = make_delta_tool(dir.path());
        let r = tool
            .execute(json!({"files":[{"path":"big.rs","start_line":2,"max_lines":3}]}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.data.is_none(), "no outline, nothing to charge");
    }

    #[tokio::test]
    async fn no_followup_with_the_lever_off() {
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "big.rs", 50);
        let tool = make_tool(dir.path());
        let _ = tool.execute(json!({"files":[{"path":"big.rs"}]})).await;
        let r = tool
            .execute(json!({"files":[{"path":"big.rs","start_line":2,"max_lines":3}]}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(r.data.is_none());
    }

    #[tokio::test]
    async fn on_context_reduced_serves_the_full_file_again() {
        let dir = tempdir().unwrap();
        write_big_rust_file(dir.path(), "a.rs", 40);
        let tool = make_delta_tool(dir.path());
        let whole = json!({"files":[{"path":"a.rs"}]});
        let r1 = tool.execute(whole.clone()).await;
        assert!(r1.output.contains("let a = x + 0;"), "{}", r1.output);
        let r2 = tool.execute(whole.clone()).await;
        assert!(
            r2.output.contains("unchanged since your last read"),
            "{}",
            r2.output
        );
        tool.on_context_reduced();
        let r3 = tool.execute(whole).await;
        assert!(r3.output.contains("let a = x + 0;"), "{}", r3.output);
        assert!(!r3.output.contains("unchanged since your last read"));
    }
}

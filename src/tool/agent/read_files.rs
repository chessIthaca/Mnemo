// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `read_files` — batch read of multiple files in one call.
//!
//! Like `file_read`, but takes an array of `{path, start_line?, max_lines?}`
//! specs so the agent can read several files (or slices of them) in a single
//! round-trip. Each file is validated through the sandbox, numbered, and
//! capped independently; per-file errors are reported inline without failing
//! the whole call. A whole-file read (no `start_line`/`max_lines`) of a large
//! indexed source file (an extension the code graph parses — any indexed
//! language: .rs/.ts/.tsx/.js/.py/…)
//! prepends one SYMBOL NUDGE line pointing at `graph_context(id=...)`, so
//! the agent takes targeted edges instead of another whole-file dump. Auto-run
//! (read-only).

use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::optimizer::{estimate_tokens, savings_event};
use super::read_cache::{diff_if_small, skeleton_for, ReadCache, SKELETON_MAX_LINES};
use crate::codegraph::walk::Lang;
use crate::provider::ToolSchema;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Default maximum number of lines returned per file when the caller doesn't
/// specify `max_lines`. Mirrors `file_read`.
pub(crate) const DEFAULT_MAX_LINES: usize = 500;

/// A whole-file read must exceed this line count to trigger the SYMBOL
/// NUDGE — small files fit in context comfortably; the note only earns its
/// keep when the agent just swallowed a big chunk of source.
pub(crate) const READ_NUDGE_MIN_LINES: usize = 300;

/// Default maximum output size in bytes per file (~100 KB). Mirrors
/// `file_read`.
pub(crate) const DEFAULT_MAX_BYTES: usize = 102_400;

/// Maximum number of files accepted in a single call. Prevents unbounded
/// output from a huge file list.
const MAX_FILES: usize = 10;

/// Total output byte cap across all files (~500 KB). If the joined output
/// exceeds this, it is truncated and a note is appended.
pub(crate) const TOTAL_BYTE_CAP: usize = 512_000;

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
/// The bare serde message (e.g. `missing field 'path'`) is technically true
/// but useless when a model mixed up two similar tool shapes: serde silently
/// ignores unknown keys (no `deny_unknown_fields`), so a `file_read` call
/// that received `read_files`-shaped args (`{"files": [...]}`) reports only
/// the missing required field. This helper appends the sorted list of keys
/// the call actually sent plus a `hint` pointing at the sibling tool, so the
/// model can self-correct on the next attempt.
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
    // parameter/type in plain words instead of serde vocabulary;
    // the received-keys + hint context rides on top of it.
    format!(
        "{} (received keys: {keys}).{hint}",
        crate::tool::error_message::sanitize_arguments_error(tool, e)
    )
}

/// A single file-read spec within a `read_files` call.
#[derive(Debug, Deserialize)]
pub(crate) struct ReadSpec {
    pub path: String,
    /// Optional 1-indexed start line (mirrors `file_read`).
    #[serde(default)]
    pub start_line: Option<usize>,
    /// Optional maximum number of lines to read (mirrors `file_read`).
    #[serde(default)]
    pub max_lines: Option<usize>,
}

/// Arguments for `read_files`.
#[derive(Debug, Deserialize)]
struct ReadFilesArgs {
    files: Vec<ReadSpec>,
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
                "{} An array of up to 10 {{path, start_line?, max_lines?}} specs. \
                 Read files — one or many, in one call instead of N round-trips. \
                 Each file comes back under a header with line numbers; start_line + \
                 max_lines read only the relevant slice. A directory path returns a \
                 sorted listing. Per-file errors are reported inline, never fatal.",
                tool_contract::contract(
                    "`files`",
                    "{\"files\":[{\"path\":\"a.js\",\"start_line\":10,\"max_lines\":40}]}"
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Read specs (max 10): {path, start_line?, max_lines?} per file.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string", "description": "Path relative to the project root."},
                                "start_line": {"type": "integer", "description": "1-indexed start line (optional)."},
                                "max_lines": {"type": "integer", "description": "Max lines to read (optional, default 500)."}
                            },
                            "required": ["path"]
                        }
                    }
                },
                "required": ["files"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
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
                        "files (an array of {path, start_line?, max_lines?} specs)",
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
            // the last-served content. First reads serve the full content
            // and establish the delta baseline.
            let delta_reads = optimizer
                .as_ref()
                .map(|c| c.read().map(|c| c.delta_reads).unwrap_or(false))
                .unwrap_or(false);
            let mut savings: Vec<serde_json::Value> = Vec::new();
            for spec in &args.files {
                if is_whole_file_source_read(spec) {
                    let (section, content) = match (delta_reads, read_cache.as_ref()) {
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
                    sections.push(read_one(&sandbox, spec));
                }
            }
            let mut output = sections.join("\n\n");
            if !nudge.is_empty() {
                // ONE leading note: the marker prefix once, the per-file
                // clauses joined with "; " (review 2026-09-14 — a repeated
                // prefix per file read as multiple notes).
                output = format!("SYMBOL NUDGE: {}\n\n{output}", nudge.join("; "));
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

/// [`read_one`] plus the raw file content (`None` for per-spec errors) —
/// `read_files::execute` uses the line count for the whole-file SYMBOL
/// NUDGE without a second disk read. `search_read` keeps calling
/// [`read_one`]; it is behavior-identical there.
pub(crate) fn read_one_with_count(sandbox: &Sandbox, spec: &ReadSpec) -> (String, Option<String>) {
    let validated = match sandbox.validate(Path::new(&spec.path)) {
        Ok(p) => p,
        Err(e) => {
            return (
                format!("=== {} (error) ===\npath validation failed: {e}", spec.path),
                None,
            )
        }
    };
    // Directory spec (backlog #89 named the path kind; 2026-12-29 session:
    // "what files are in this dir" had no affordance — the old error hint
    // sent the model to search, whose 100-match cap + display truncation
    // made enumeration unreliable). Return a sorted listing instead.
    if validated.is_dir() {
        return (directory_listing(&validated, &spec.path), None);
    }
    let content = match std::fs::read_to_string(&validated) {
        Ok(c) => c,
        Err(e) => {
            return (
                format!("=== {} (error) ===\nfailed to read: {e}", spec.path),
                None,
            )
        }
    };

    let total_lines = content.lines().count();
    let start = spec.start_line.unwrap_or(1).saturating_sub(1);
    // Cap the number of lines at DEFAULT_MAX_LINES unless the caller
    // explicitly requested fewer (mirrors file_read).
    let max = spec
        .max_lines
        .unwrap_or(DEFAULT_MAX_LINES)
        .min(DEFAULT_MAX_LINES);

    let numbered: Vec<String> = content
        .lines()
        .skip(start)
        .take(max)
        .enumerate()
        .map(|(i, line)| format!("{:>4}: {}", start + i + 1, line))
        .collect();

    let showed = numbered.len();
    let mut body = numbered.join("\n");

    // Byte-cap per file (mirrors file_read). Back up to a char boundary so a
    // multi-byte char at the cut point doesn't panic.
    let truncated_by_bytes = if body.len() > DEFAULT_MAX_BYTES {
        truncate_to_boundary(&mut body, DEFAULT_MAX_BYTES);
        true
    } else {
        false
    };

    // Truncation note only when the default line cap was the limiting factor
    // (not when the caller explicitly requested a range). The byte cap always
    // notes. Mirrors file_read.
    let available_from_start = total_lines.saturating_sub(start);
    let default_capped = spec.max_lines.is_none() && showed < available_from_start;
    if truncated_by_bytes || default_capped {
        body.push_str(&format!(
            "\n... (truncated: {total_lines} lines total, showed {showed})"
        ));
    }

    let first_line = start + 1;
    let last_line = start + showed;
    let header = if showed == 0 {
        format!("=== {} (empty range) ===", spec.path)
    } else {
        format!(
            "=== {} (lines {first_line}-{last_line} of {total_lines}) ===",
            spec.path
        )
    };
    (format!("{header}\n{body}"), Some(content))
}

/// Whole-file read with the delta/skeleton lever applied (backlog
/// e4a50d22, `delta_reads`). Returns the section, the raw content (for the
/// SYMBOL NUDGE probe), and appends any savings events to `savings`.
/// Semantics:
/// - first read of the path → full content, and the content becomes the
///   delta baseline;
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
) -> (String, Option<String>) {
    // Today's full serve — the baseline path and the fallback on every
    // reduced-path miss (per-spec errors, directories, degenerate input).
    let (full_section, content) = read_one_with_count(sandbox, spec);
    let Some(content) = content else {
        return (full_section, None);
    };
    let Some(lang) = Path::new(&spec.path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| Lang::from_extension(&e.to_ascii_lowercase()))
    else {
        // is_whole_file_source_read guarantees a known Lang; this is a
        // belt-and-braces fail-open (full serve) if that ever drifts.
        return (full_section, Some(content));
    };
    let Ok(key) = sandbox.validate(Path::new(&spec.path)) else {
        return (full_section, Some(content));
    };
    let before_tokens = estimate_tokens(&full_section);
    match cache.snapshot(&key) {
        None => {
            cache.record(key, content.clone());
            (full_section, Some(content))
        }
        Some(old) if old == content => {
            if lang == Lang::Html {
                // No line-level signatures in HTML — a skeleton would be
                // noise. Serve the full content; the baseline stays cached
                // so future DIFF serves work.
                return (full_section, Some(content));
            }
            let sk = skeleton_for(&content, lang);
            let total = content.lines().count();
            let header = format!(
                "=== {} (unchanged since your last read — signature skeleton: {} of {total} lines) ===",
                spec.path,
                sk.kept
            );
            let mut body = sk.body;
            if sk.capped {
                body.push_str(&format!(
                    "... (skeleton capped at {SKELETON_MAX_LINES} signature lines)\n"
                ));
            }
            body.push_str(
                "[delta read: file is UNCHANGED since your last full read — signature lines \
                 only. Re-read with start_line/max_lines for exact lines.]",
            );
            let section = format!("{header}\n{body}");
            let after_tokens = estimate_tokens(&section);
            if after_tokens >= before_tokens {
                // Tiny file: the header + note boilerplate costs more than
                // the full serve. A reduced serve must never cost MORE
                // than the full one — serve the full content (the cached
                // baseline is already current; no savings row).
                return (full_section, Some(content));
            }
            savings.push(savings_event(
                "skeleton",
                &spec.path,
                before_tokens,
                after_tokens,
            ));
            (section, Some(content))
        }
        Some(old) => match diff_if_small(&old, &content) {
            Some(diff) => {
                let header = format!(
                    "=== {} (changed since your last read — unified diff vs your last full read) ===",
                    spec.path
                );
                let body = format!(
                    "{diff}\n[delta read: unified diff vs your last full read. Re-read with \
                     start_line/max_lines for the exact current content.]"
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
                    return (full_section, Some(content));
                }
                savings.push(savings_event(
                    "delta_read",
                    &spec.path,
                    before_tokens,
                    after_tokens,
                ));
                cache.record(key, content.clone());
                (section, Some(content))
            }
            None => {
                // Mostly-rewritten file: a diff would cost more than a
                // full re-serve. Keep today's full section; refresh the
                // baseline.
                cache.record(key, content.clone());
                (full_section, Some(content))
            }
        },
    }
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

    /// Write a Rust file with `n` ~10-line free functions (`n = 40` clears
    /// the 300-line whole-file nudge threshold).
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
            schema.description.contains("\"files\":[{\"path\":\"a.js\""),
            "the inline example shows the exact call shape: {}",
            schema.description
        );
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
        assert!(result.output.contains("   2: l2"));
        assert!(result.output.contains("   3: l3"));
        assert!(!result.output.contains("l1"));
        assert!(!result.output.contains("l4"));
        // File B: full.
        assert!(result.output.contains("   1: x"));
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
        assert!(result.output.contains("   3: three"));
        assert!(result.output.contains("   4: four"));
        assert!(!result.output.contains("   1: "));
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
        assert!(result.output.contains("   1: a"));
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
        // ~180 KB of CJK chars (3 bytes each) → exceeds DEFAULT_MAX_BYTES (100 KB).
        let content = "日本語".repeat(20_000);
        std::fs::write(dir.path().join("cjk.txt"), &content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"files": [{"path": "cjk.txt"}]})).await;
        assert!(result.success, "{}", result.output.len());
        // The per-file byte cap should have fired (150 KB > 100 KB).
        assert!(
            result.output.contains("truncated"),
            "expected byte truncation note (output len = {})",
            result.output.len()
        );
        // Output must be valid UTF-8 (no panic, no replacement chars from a bad cut).
        assert!(result.output.contains("日本語"));
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

    #[tokio::test]
    async fn delta_lever_off_serves_full_content_identically() {
        // No optimizer attached (CLI contract builds, pre-lever tests):
        // re-reads serve the full content again, byte-identical to the
        // pre-lever build, and no savings data rides the result — the
        // regression pin for the lever's default-off invariant.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let r1 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        let r2 = tool.execute(json!({"files":[{"path":"a.rs"}]})).await;
        assert!(r1.success && r2.success);
        assert_eq!(r1.output, r2.output, "flag off: byte-identical re-reads");
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
        let cfg = Arc::new(std::sync::RwLock::new(crate::config::OptimizerConfig::default()));
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
        assert_eq!(r1.output, r2.output, "flag off: byte-identical re-reads");
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
        assert!(r2.output.contains("Re-read with start_line/max_lines"), "{}", r2.output);
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
        assert!(r2.output.contains("   2: fn b() {}"), "{}", r2.output);
        assert!(!r2.output.contains("unchanged since your last read"), "{}", r2.output);
        assert!(r2.data.is_none());
    }
}

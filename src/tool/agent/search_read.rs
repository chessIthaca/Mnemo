// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `search_read` — search + auto-read combo tool.
//!
//! Runs the same regex+glob content search as `search`, then immediately
//! reads the top-N matched files (with line numbers, capped per-file and
//! total) in a single call. This collapses the common search → see line
//! numbers → `read_files` round-trip into one tool call. Auto-run (read-only).
//!
//! AUTO-DELEGATION (backlog b804012f): same contract as `search` — a symbol
//! hunt the graph can answer (or a memory-targeted query: knowledge/reviews
//! globs, typed SPEC:/DECISION:/… prefixes) gets the answer inline and the
//! search+read is skipped entirely; re-issuing the SAME query runs the
//! plain search+read (the escape hatch). See search.rs for the full
//! contract.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::pattern;
use crate::tool::agent::read_files::{read_one, truncate_to_boundary, ReadSpec, TOTAL_BYTE_CAP};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::search;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Maximum number of matched files to auto-read. Keeps the output bounded —
/// a broad search can match dozens of files, but reading more than a handful
/// in one call would blow the context window. The caller may request fewer
/// via `max_files`, but never more than this.
const MAX_FILES_TO_READ: usize = 5;

/// Arguments for `search_read`.
#[derive(Debug, Deserialize)]
struct SearchReadArgs {
    /// A regex pattern to search for in file contents.
    pattern: String,
    /// A glob pattern to limit which files to search (e.g. "**/*.rs").
    #[serde(default)]
    glob: Option<String>,
    /// Whether to do a literal (non-regex) search.
    #[serde(default)]
    literal: bool,
    /// Maximum number of matched files to read (default 5, capped at 5).
    #[serde(default)]
    max_files: Option<usize>,
}

/// The `search_read` tool — search file contents, then auto-read the top-N
/// matched files in one call.
pub struct SearchReadTool {
    sandbox: Sandbox,
    /// The project's code graph, when enabled — its FTS content index ranks
    /// literal queries (matched files arrive in relevance order instead of
    /// walk order). `None` → every query walks the tree.
    graph: Option<std::sync::Arc<crate::codegraph::CodeGraph>>,
    /// The memory store, when wired — a uuid-shaped pattern earns a
    /// fired-only "known memory hit" note (F9, same as `search`).
    memory: Option<std::sync::Arc<dyn crate::memory::MemoryStoreTrait>>,
    /// The bypassed delegated-query keys (backlog b804012f, set semantics
    /// 2026-01-03): once a query has delegated, its exact key is recorded
    /// here and identical re-issues NEVER re-delegate in-session — the
    /// documented escape must survive interleaved delegating queries.
    /// Bounded (most recent DELEGATION_SET_CAP keys, oldest evicted).
    delegation_state: std::sync::Arc<std::sync::Mutex<Vec<search::DelegatedKey>>>,
}

impl SearchReadTool {
    /// Create the tool, bound to a sandbox. `graph` (when present) enables
    /// the index-backed path for literal queries.
    pub fn new(
        sandbox: Sandbox,
        graph: Option<std::sync::Arc<crate::codegraph::CodeGraph>>,
    ) -> Self {
        Self {
            sandbox,
            graph,
            memory: None,
            delegation_state: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Wire the memory store: uuid-shaped patterns then earn the known-
    /// memory-hit note (F9) when a derived row knows the id.
    pub fn with_memory(
        mut self,
        memory: std::sync::Arc<dyn crate::memory::MemoryStoreTrait>,
    ) -> Self {
        self.memory = Some(memory);
        self
    }
}

#[async_trait]
impl Tool for SearchReadTool {
    fn name(&self) -> &str {
        "search_read"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "search_read",
            "Search + auto-read in one call: the same content search as `search`, then reads \
             the top matched files' full numbered content — collapsing the search→read \
             round-trip. Literal queries use the content index when populated (engine: \
             index), walking otherwise. Build output and dependencies are always skipped. \
              ESCAPE-HATCH: for text with regex metacharacters or backslashes \
              prefer literal:true (it avoids escaping); if a call is rejected as \
              malformed, do NOT resend it — reformulate. \
               Returns a summary line, then the matched files (capped per-file and total). \
              For symbol questions — where is X defined, who calls X — call graph_search \
              (then graph_context(id=...)) FIRST; symbol-shaped patterns naming an indexed \
              symbol, and memory hunts (.coding/knowledge|reviews globs, typed SPEC:/DECISION:/ \
              prefixes), auto-delegate: the answer rides inline and the search+read is skipped; \
              re-issue the same search to get the plain search.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The pattern to search for (regex by default, or literal). Prefer literal:true for text with regex metacharacters or backslashes."},
                    "glob": {"type": "string", "description": "Glob pattern to filter files (e.g. \"**/*.rs\")."},
                    "literal": {"type": "boolean", "description": "Treat pattern as literal text (default: false, regex). RECOMMENDED for text with regex metacharacters or backslashes — avoids escaping."},
                    "max_files": {"type": "integer", "description": "Maximum number of matched files to read (default 5, max 5)."}
                },
                "required": ["pattern"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: SearchReadArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };

        // Validate the glob BEFORE use so a pattern that escapes the sandbox
        // root (absolute path, drive prefix, UNC, `..` traversal) is rejected
        // outright rather than read. Auto-run tool — no approval to catch this.
        if let Some(glob) = &args.glob {
            if let Err(e) = crate::tool::agent::search::validate_glob(glob) {
                return ToolResult::error(e);
            }
        }

        let max_files = args
            .max_files
            .unwrap_or(MAX_FILES_TO_READ)
            .min(MAX_FILES_TO_READ)
            .max(1);

        let sandbox = self.sandbox.clone();
        let root = sandbox.root().to_path_buf();
        let graph = self.graph.clone();
        let delegation_state = self.delegation_state.clone();
        // F9: known-backlog-item note — fired only when the pattern is
        // uuid-shaped AND a derived row knows it (best-effort, one SELECT).
        let memory_note = match &self.memory {
            Some(store) => search::known_memory_note(store, &args.pattern).await,
            None => None,
        };
        // Auto-delegation escape hatch (backlog b804012f, set semantics
        // 2026-01-03): a repeat of ANY previously delegated query skips
        // delegation entirely and runs the plain search+read. Sticky per
        // exact key — interleaved delegating queries never break an earlier
        // escape, and there is no delegate/escape ping-pong.
        let key = search::DelegatedKey::new(&args.pattern, args.glob.as_deref(), args.literal);
        let escape = self.delegation_state.lock().unwrap().contains(&key);
        // Memory delegation (backlog b804012f): a query targeting the memory
        // store (knowledge/reviews glob or typed prefix) gets the recall
        // answer inline. Async store read — before the blocking scan, like
        // the F9 note above.
        let memory_block = if escape {
            None
        } else {
            match &self.memory {
                Some(store) => {
                    search::memory_delegation_block(store, &args.pattern, args.glob.as_deref())
                        .await
                }
                None => None,
            }
        };
        tokio::task::spawn_blocking(move || {
            // Compile via the shared helper: a broken regex degrades to
            // literal matching with a visible note (read tool — no mutation
            // risk), instead of erroring the whole call.
            let compiled = match pattern::compile_with_fallback(&args.pattern, args.literal, true) {
                Ok(c) => c,
                Err(e) => return ToolResult::error(e),
            };
            let re = compiled.regex;
            let fallback_note = compiled.fallback_note;

            // Auto-delegation (backlog b804012f): same contract as `search`
            // — a symbol hunt the graph can answer (or a memory-targeted
            // query) gets the answer inline; when it fully serves the query
            // the search+read is skipped entirely and the key is recorded
            // for the escape hatch; a symbol hunt narrowed by a glob
            // prepends the block above the normal results instead.
            let symbol_block = if escape {
                None
            } else {
                graph.as_deref().and_then(|g| {
                    search::symbol_hunt_names(&args.pattern).and_then(|names| {
                        crate::tool::agent::codegraph::symbol_delegation_block(g, &names)
                    })
                })
            };
            let memory_delegated = memory_block.is_some();
            let delegated = memory_block.or(symbol_block);
            let mut prepended: Option<String> = None;
            if let Some(block) = delegated {
                // Record the key on BOTH arms: the fast-path return AND the
                // glob-narrowed prepend — the block promises the escape
                // ("re-issue this exact search…") either way, so the repeat
                // must run the plain search+read (review 2026-12-28 H1). Set
                // semantics: the key joins the bypass set (dedup, bounded —
                // oldest evicted) without replacing other queries' escapes.
                {
                    let mut set = delegation_state.lock().unwrap();
                    set.retain(|k| *k != key);
                    set.push(key);
                    let overflow = set.len().saturating_sub(search::DELEGATION_SET_CAP);
                    set.drain(..overflow);
                }
                if memory_delegated || args.glob.is_none() {
                    return ToolResult::success(block);
                }
                prepended = Some(block);
            }

            // Symbol-shaped pattern? The same advisory nudge as `search` —
            // one indexed SELECT — plus the same literal-engine TIP for
            // metachar-free regex walks (post index-parity the advice is
            // true here too: literal:true rides the index). The TIP yields
            // when the nudge fires (symbol steering wins) and both yield
            // when a delegated block is prepended (it subsumes them); all
            // merged with any literal-fallback note, prepended above the
            // results.
            let nudge = if prepended.is_some() {
                None
            } else {
                search::symbol_nudge(&graph, &args.pattern)
            };
            let tip = if prepended.is_some() {
                None
            } else {
                search::literal_tip(&graph, &args.pattern, args.literal, nudge.is_some())
            };
            let note = search::merged_note(prepended, fallback_note, nudge, tip, memory_note);

            // The FTS content index serves literal, single-line queries when
            // populated (same guards as search.rs — a fallback-fired pattern
            // walks so "matched literally" stays exact, review B1); matched
            // files arrive in relevance (bm25) order instead of walk order.
            // A stale index (F10) is repaired inline for a modest staleness
            // (search::try_index re-indexes at most STALE_REINDEX_CAP files
            // under the codegraph stale-reindex budget, then re-queries
            // once) — the re-index is disclosed in the note; a Stale outcome
            // falls through to the walk with the staleness merged into the
            // prepended note.
            let mut stale_note: Option<String> = None;
            if args.literal && !args.pattern.contains('\n') {
                if let Some(g) = &graph {
                    match search::try_index(g, &root, &args.pattern, args.glob.as_deref()) {
                        Some(search::FtsOutcome::Hits(ix)) => {
                            // Disclose the inline re-index (F10): a read
                            // tool mutated the index DB — the note rides
                            // ABOVE the results (truncation-safe).
                            let note = if ix.reindexed > 0 {
                                search::push_note(
                                    note,
                                    format!(
                                        "reindexed {} stale file(s) — serving fresh index results",
                                        ix.reindexed
                                    ),
                                )
                            } else {
                                note
                            };
                            return build_index_output(ix, max_files, &sandbox, &note);
                        }
                        Some(search::FtsOutcome::Stale { files }) => {
                            stale_note = Some(format!(
                                "content index stale for {files} file(s) — serving tree-walk results"
                            ));
                        }
                        None => {}
                    }
                }
            }
            let note = match stale_note {
                Some(s) => search::push_note(note, s),
                None => note,
            };

            // Pruned tree walk (shared helper, same as search.rs): ignored
            // directories are cut wholesale and never enumerated; every
            // yielded path is a file under the sandbox root by construction.
            let (paths, pruned_dirs) = search::walk_searchable(&root, args.glob.as_deref())
                .unwrap_or((Vec::new(), 0));

            // Collect matched files in discovery order, dedup by path, stop
            // once we have `max_files` distinct matched files (but keep
            // counting total matches + files searched for the summary).
            let mut matched_files: Vec<PathBuf> = Vec::new();
            let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            let mut files_searched = 0u32;
            let mut files_matched = 0u32;
            let mut total_matches = 0usize;

            for entry in &paths {
                files_searched += 1;
                let content = match std::fs::read_to_string(entry) {
                    Ok(c) => c,
                    Err(_) => continue, // skip binary/unreadable
                };
                // Shared matcher: per-line for single-line patterns,
                // whole-content (CRLF-normalized) for spanning patterns.
                let matches = pattern::match_lines(&content, &re, &args.pattern);
                total_matches += matches.len();
                let file_matched = !matches.is_empty();
                if file_matched {
                    files_matched += 1;
                    // Only collect up to max_files distinct files for reading.
                    if matched_files.len() < max_files && seen.insert(entry.clone()) {
                        matched_files.push(entry.clone());
                    }
                }
            }

            if matched_files.is_empty() {
                // C2: a literal-mode miss over a regex-shaped pattern is
                // the signature of a mode mistake — the retry hint (shared
                // helper with search).
                let mode_hint = if args.literal && search::has_regex_metachars(&args.pattern) {
                    search::literal_metachar_hint()
                } else {
                    ""
                };
                // F7: distinguish "glob matched no files" from "pattern
                // matched no content" (same hint as search's walk path).
                let glob_hint = if args.glob.is_some() && files_searched == 0 {
                    "; the glob matched no files — if that's unexpected, \
                     use extension-anchored shapes like **/*.rs"
                } else {
                    ""
                };
                // F2: filename fallback (shared helper with search).
                let name_hint =
                    search::filename_hint(&search::filename_matches(&paths, &root, &re));
                let body = format!(
                    "no matches found{mode_hint}{glob_hint}{name_hint} (searched {files_searched} files, skipped {pruned_dirs} ignored dirs, engine: walk)"
                );
                return ToolResult::success(search::with_note(body, &note));
            }

            // Read each matched file via the shared read_one formatter
            // (per-file line/byte caps + truncation notes). Convert each
            // absolute path to a project-relative path so read_one's sandbox
            // validation + header use the relative form.
            let mut sections: Vec<String> = Vec::with_capacity(matched_files.len());
            for abs in &matched_files {
                let rel = abs
                    .strip_prefix(&root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| abs.to_string_lossy().replace('\\', "/"));
                let spec = ReadSpec {
                    path: rel,
                    start_line: None,
                    max_lines: None,
                };
                sections.push(read_one(&sandbox, &spec));
            }

            let read_count = sections.len();
            let mut output = search::with_note(
                format!(
                    "{total_matches} matches in {files_matched} files (searched {files_searched}, skipped {pruned_dirs} ignored dirs, engine: walk); reading top {read_count}\n\n{}",
                    sections.join("\n\n")
                ),
                &note,
            );

            // Total byte cap across all read sections (mirrors read_files).
            if output.len() > TOTAL_BYTE_CAP {
                truncate_to_boundary(&mut output, TOTAL_BYTE_CAP);
                output.push_str("\n... (truncated: total output exceeded size limit)");
            }
            ToolResult::success(output)
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("search_read task failed: {e}")))
    }
}

/// Render the index-engine result: rank-ordered distinct files, the top
/// `max_files` read via the shared formatter. Index paths are
/// project-relative and `/`-separated (the DB key form) — `read_one`'s
/// sandbox validation handles them directly. try_index returns Hits only
/// with total ≥ 1 (an empty fetch walks — 2026-01-03), so the hits are
/// never empty here; the C2 metachar retry hint lives on the walk path.
fn build_index_output(
    ix: search::IndexHits,
    max_files: usize,
    sandbox: &Sandbox,
    note: &Option<String>,
) -> ToolResult {
    let mut seen = std::collections::HashSet::new();
    let mut sections = Vec::new();
    for h in &ix.hits {
        if sections.len() >= max_files {
            break;
        }
        if seen.insert(h.path.clone()) {
            sections.push(read_one(
                sandbox,
                &ReadSpec {
                    path: h.path.clone(),
                    start_line: None,
                    max_lines: None,
                },
            ));
        }
    }
    let read_count = sections.len();
    let body = format!(
        "{} matches in {} files (engine: index); reading top {read_count}\n\n{}",
        ix.total,
        ix.files,
        sections.join("\n\n")
    );
    let mut output = search::with_note(body, note);
    // Total byte cap across all read sections (same as the walk path).
    if output.len() > TOTAL_BYTE_CAP {
        truncate_to_boundary(&mut output, TOTAL_BYTE_CAP);
        output.push_str("\n... (truncated: total output exceeded size limit)");
    }
    ToolResult::success(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &std::path::Path) -> SearchReadTool {
        SearchReadTool::new(Sandbox::new(dir).unwrap(), None)
    }

    /// A tool backed by a freshly indexed in-memory graph (index path).
    fn make_indexed_tool(dir: &std::path::Path) -> SearchReadTool {
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.to_path_buf()).unwrap();
        graph.index(None).unwrap();
        SearchReadTool::new(Sandbox::new(dir).unwrap(), Some(std::sync::Arc::new(graph)))
    }

    #[test]
    fn schema_advertises_the_literal_escape_hatch() {
        // Backlog f4d5e053: parity with search — the ESCAPE-HATCH note, the
        // recovery rule, and the RECOMMENDED/pattern-param wording must
        // stay advertised (the inline example lives in search's
        // description). Mirrors the read_files collapse test (plan
        // 9e0b266a).
        let tool = make_tool(std::path::Path::new("."));
        let schema = tool.schema();
        assert!(
            schema.description.contains("ESCAPE-HATCH"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("prefer literal:true"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("do NOT resend"),
            "the recovery rule must ride the description: {}",
            schema.description
        );
        let props = &schema.parameters["properties"];
        assert!(
            props["literal"]["description"]
                .as_str()
                .unwrap()
                .contains("RECOMMENDED"),
            "the literal param must carry the RECOMMENDED wording: {}",
            props["literal"]
        );
        assert!(
            props["pattern"]["description"]
                .as_str()
                .unwrap()
                .contains("Prefer literal:true"),
            "the pattern param must carry the recommendation: {}",
            props["pattern"]
        );
    }

    #[tokio::test]
    async fn searches_and_reads_matched_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn hello() {}\nfn other() {}").unwrap();
        std::fs::write(dir.path().join("c.rs"), "fn goodbye() {}").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "hello"})).await;
        assert!(result.success, "{}", result.output);
        // Both matching files are read (headers + numbered content).
        assert!(result.output.contains("=== a.rs"));
        assert!(result.output.contains("=== b.rs"));
        assert!(result.output.contains("hello"));
        // The non-matching file is absent.
        assert!(!result.output.contains("=== c.rs"));
        // Summary line present.
        assert!(result.output.contains("matches in"));
        assert!(result.output.contains("reading top"));
    }

    #[tokio::test]
    async fn stale_index_reindexes_and_reads_fresh_content() {
        // F10 inline re-index (user request 2026-12-30): a small staleness
        // is repaired inside the tool call — the match set comes from the
        // re-queried fresh index, the auto-read serves the fresh disk
        // content, and the side effect is disclosed with a single
        // "note: " prefix (the doubled "note: note:" bug).
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn old_marker() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        // Simulate the edit→watcher gap: rewrite + bump mtime, no reindex.
        std::fs::write(&path, "fn old_marker() {}\nfn fresh_marker() {}\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let r = tool
            .execute(json!({"pattern": "marker", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("reindexed 1 stale file(s)"),
            "the inline re-index is disclosed: {}",
            r.output
        );
        assert!(r.output.contains("engine: index"), "{}", r.output);
        // The auto-read serves the fresh disk content.
        assert!(r.output.contains("fresh_marker"), "{}", r.output);
        assert!(
            !r.output.contains("note: note:"),
            "single note prefix: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn respects_glob_filter() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "target").unwrap();
        std::fs::write(dir.path().join("a.txt"), "target").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "target", "glob": "**/*.rs"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("=== a.rs"));
        assert!(!result.output.contains("=== a.txt"));
    }

    #[tokio::test]
    async fn literal_search_works() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "price: $5.00").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "$5.00", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("=== a.txt"));
    }

    #[tokio::test]
    async fn no_matches_message() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "nonexistent"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("no matches"));
    }

    #[tokio::test]
    async fn invalid_regex_falls_back_to_literal() {
        // Regression: unbalanced metacharacters used to hard-error the whole
        // call ("invalid regex"). Read tools degrade to literal matching with
        // a visible note instead.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "call [invalid syntax here").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "[invalid"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("matched literally"),
            "fallback note present: {}",
            result.output
        );
        assert!(
            result.output.contains("=== a.rs"),
            "matched file read: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn multi_line_pattern_matches_crlf_file() {
        // Regression: spanning patterns could never match a per-line scan,
        // and CRLF files broke whole-content matching too.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {\r\n    bar();\r\n}\r\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "fn foo() {\n    bar();"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("=== a.rs"),
            "matched file read: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn literal_query_uses_index_engine() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "the memory_recall engine\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "memory_recall", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: index"),
            "index engine: {}",
            result.output
        );
        assert!(result.output.contains("=== a.rs"));
        assert!(!result.output.contains("=== b.rs"));
    }

    #[tokio::test]
    async fn empty_index_falls_back_to_walk() {
        // Graph present but never indexed → zero content rows → walk.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn helper() {}\n").unwrap();
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        // Deliberately no index() call — cg_content_meta stays empty.
        let tool = SearchReadTool::new(
            Sandbox::new(dir.path()).unwrap(),
            Some(std::sync::Arc::new(graph)),
        );
        let result = tool
            .execute(json!({"pattern": "helper", "literal": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: walk"),
            "walk engine: {}",
            result.output
        );
        assert!(result.output.contains("=== a.rs"));
    }

    #[tokio::test]
    async fn regex_query_walks_even_with_index() {
        // Regex patterns can't be served by FTS — the engine must stay walk
        // even when the content index is populated (mirror of search.rs).
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn helper() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool.execute(json!({"pattern": "hel+per"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("engine: walk"),
            "regex walks: {}",
            result.output
        );
        assert!(
            result.output.contains("=== a.rs"),
            "matched file still read: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn fallback_literal_walks_even_with_index() {
        // Review B1 parity (mirror of search.rs): a broken regex degrades to
        // literal matching but must NOT ride the index — the FTS phrase drops
        // non-token characters (`[invalid` queries token `invalid`), which
        // would match MORE files than the literal promises. Only the walk
        // engine keeps "matched literally" semantics exact.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "call [invalid syntax here\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "just the word invalid alone\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let result = tool.execute(json!({"pattern": "[invalid"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("matched literally"),
            "fallback note present: {}",
            result.output
        );
        assert!(
            result.output.contains("engine: walk"),
            "fallback must walk for exact literal semantics: {}",
            result.output
        );
        assert!(
            result.output.contains("=== a.rs"),
            "literal match read: {}",
            result.output
        );
        assert!(
            !result.output.contains("=== b.rs"),
            "a file without the literal must not be read: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn caps_files_at_max() {
        // 7 files match — only 5 should be read (default max_files).
        let dir = tempdir().unwrap();
        for i in 0..7 {
            std::fs::write(dir.path().join(format!("f{i}.rs")), "findme").unwrap();
        }
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "findme"})).await;
        assert!(result.success, "{}", result.output);
        // Count the file headers (=== fN.rs).
        let headers: usize = result
            .output
            .lines()
            .filter(|l| l.starts_with("=== f") && l.contains(".rs"))
            .count();
        assert_eq!(headers, MAX_FILES_TO_READ, "expected at most 5 files read");
        // Summary reports all 7 matched files even though only 5 were read.
        assert!(result.output.contains("7 files"));
        assert!(result.output.contains("reading top 5"));
    }

    #[tokio::test]
    async fn skips_ignored_dirs() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn findme() {}").unwrap();
        // target/ — Rust build output (must be skipped).
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/built.rs"), "fn findme() {}").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"pattern": "findme"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("=== src.rs"));
        assert!(!result.output.contains("target/built.rs"));
        assert!(result.output.contains("skipped"));
    }

    // --- A1: glob sandbox-escape guards -------------------------------------

    #[tokio::test]
    async fn glob_parent_traversal_rejected() {
        // A `..` glob must be rejected outright — never read files outside root.
        let dir = tempdir().unwrap();
        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join("escape_a1_search_read.txt");
        std::fs::write(&outside, "secret-a1-read").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "secret-a1-read", "glob": "../**/*.txt"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("parent-directory")
                || result.output.contains("relative to the project root"),
            "expected glob-rejection error, got: {}",
            result.output
        );
        assert!(!result.output.contains("secret-a1-read"));
        let _ = std::fs::remove_file(&outside);
    }

    #[tokio::test]
    async fn glob_absolute_path_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "glob": "/etc/**/*.txt"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("relative to the project root"));
    }

    #[tokio::test]
    async fn glob_drive_prefix_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "glob": "C:/Windows/**/*.ini"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("relative to the project root"));
    }

    #[tokio::test]
    async fn filename_fallback_hints_when_only_paths_match() {
        // F2 (shared helper with search): a plan file never contains its
        // own id — when the content search finds nothing, path matches are
        // surfaced in the zero-hit line.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/plans")).unwrap();
        std::fs::write(dir.path().join(".coding/plans/812e1f0a.md"), "step one\n").unwrap();
        let tool = make_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "812e1f0a", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("file path(s) match the pattern"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains(".coding/plans/812e1f0a.md"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn literal_metachar_zero_hits_hint() {
        // C2 (shared helper with search): the mode-mistake retry hint
        // rides zero-hit literal metachar searches, and only those.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha beta\n").unwrap();
        let tool = make_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "(a|b)gamma", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );
        let r = tool
            .execute(json!({"pattern": "zzz", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn literal_metachar_zero_hits_hint_via_the_walk_fallback() {
        // Enforcement-ladder review finding 1: the retry hint rides zero-hit
        // metachar literal searches regardless of engine. Since 2026-01-03 an
        // empty index fetch is no longer authoritative (the watcher's
        // reindex lags new files — a zero fetch can never prove absence), so
        // zero-hit literals are always served by the WALK fallback, never
        // "engine: index"; the hint still fires there, and a metachar-free
        // zero hit stays hint-free.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha beta\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "(a|b)gamma", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("engine: index"),
            "the index engine never serves a zero hit: {}",
            r.output
        );
        assert!(
            r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );
        let r = tool
            .execute(json!({"pattern": "zzznotfound", "literal": true}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("engine: index"),
            "{}",
            r.output
        );
        assert!(
            !r.output.contains("retry with literal:false"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn bare_identifier_symbol_search_earns_the_graph_nudge() {
        // Mirrors search.rs: grepping a symbol name DELEGATES — the graph
        // answers inline and the search+read is skipped; the escape repeat
        // runs the plain search+read with the advisory nudge above the
        // summary + read sections. A spaced pattern (a text search) never
        // delegates nor nudges.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\nfn world() {}").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with("AUTO-DELEGATED to the code graph — 'hello' is an indexed symbol"),
            "prepended: {}",
            r.output
        );
        assert!(r.output.contains("def: a.rs::hello::1"), "{}", r.output);
        assert!(
            r.output.contains("graph_context(id=\"a.rs::hello::1\")"),
            "the symbol id is embedded: {}",
            r.output
        );
        assert!(
            !r.output.contains("=== a.rs"),
            "the search+read is skipped entirely: {}",
            r.output
        );

        // The escape repeat: plain search+read results, advisory nudge above.
        let r = tool.execute(json!({"pattern": "hello"})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.starts_with("note:"), "prepended: {}", r.output);
        assert!(
            r.output.contains("'hello' is an indexed symbol"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains("=== a.rs"),
            "results untouched: {}",
            r.output
        );

        // A spaced pattern (a text search) never nudges. NOTE: "fn hello"
        // used to be the negative here; the definition-prefix widening
        // deliberately made it nudge — the twin positive lives in
        // search.rs (definition_prefixed_identifier_searches_earn_the_
        // graph_nudge).
        let r = tool.execute(json!({"pattern": "fn hello world"})).await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("is an indexed symbol"), "{}", r.output);
    }

    #[tokio::test]
    async fn memory_targeted_query_delegates_and_skips_the_read() {
        // Backlog b804012f, search_read twin: a knowledge-dir glob gets the
        // recall answer inline — the search+read is skipped entirely.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let store: std::sync::Arc<dyn crate::memory::MemoryStoreTrait> = {
            let embedder: std::sync::Arc<dyn crate::memory::Embedder> =
                std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new());
            std::sync::Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap())
        };
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "DECISION: Tool cards are frameless by design",
                "Tool invocation cards in the agent chat are frameless; state rides text color.",
                1,
            ))
            .await
            .unwrap();
        let tool = SearchReadTool::new(Sandbox::new(dir.path()).unwrap(), None)
            .with_memory(std::sync::Arc::clone(&store));
        let r = tool
            .execute(json!({"pattern": "frameless", "glob": ".coding/knowledge/**"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.starts_with("AUTO-DELEGATED to memory"),
            "prepended: {}",
            r.output
        );
        assert!(
            r.output.contains("DECISION: Tool cards are frameless by design"),
            "{}",
            r.output
        );
        assert!(
            !r.output.contains("=== "),
            "the search+read is skipped entirely: {}",
            r.output
        );

        // The escape repeat: the plain search+read runs (the glob matches
        // nothing in this fixture) and no block rides it.
        let r = tool
            .execute(json!({"pattern": "frameless", "glob": ".coding/knowledge/**"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("AUTO-DELEGATED"),
            "the escape repeat is a plain search+read: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn glob_narrowed_delegation_escapes_on_the_repeat() {
        // H1 twin (review 2026-12-28): the prepend path records the key too,
        // so the escape repeat runs the plain search+read with no block.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool
            .execute(json!({"pattern": "hello", "glob": "**/*.rs"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("AUTO-DELEGATED to the code graph"),
            "prepended: {}",
            r.output
        );
        assert!(
            r.output.contains("=== a.rs"),
            "results ride below the block: {}",
            r.output
        );
        let r = tool
            .execute(json!({"pattern": "hello", "glob": "**/*.rs"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("AUTO-DELEGATED"),
            "the escape repeat is a plain search+read: {}",
            r.output
        );
        assert!(r.output.contains("=== a.rs"), "results: {}", r.output);
    }

    #[tokio::test]
    async fn metachar_free_regex_walk_earns_the_literal_tip() {
        // The shared helper wires the same TIP into search_read — and post
        // index-parity (step 1) the advice is genuinely true here too.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "some phrase here\n").unwrap();
        let tool = make_indexed_tool(dir.path());
        let r = tool.execute(json!({"pattern": "some phrase"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output
                .starts_with("note: TIP: pattern has no regex metacharacters"),
            "{}",
            r.output
        );
        assert!(r.output.contains("engine: walk"), "{}", r.output);
        assert!(r.output.contains("=== a.txt"), "{}", r.output);
    }
}

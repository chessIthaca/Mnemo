// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `expand_result` — retrieve a tool result archived by the progressive-
//! disclosure lever (token-optimizer lever 3, backlog e4a50d22).
//!
//! When a tool result is too large to keep in context, the full original is
//! archived in the project's memory DB ([`crate::memory`]) and the context
//! carries a bounded preview naming the archive row's id. This tool serves
//! that original back — by id, or by keyword when the id is unknown — so the
//! content is *disclosed progressively* instead of being lost to a lossy
//! truncation. Read-only: the archive write itself happens at ingestion.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::optimizer::{estimate_tokens, savings_event};
use super::output_compactor::redact_secrets;
use crate::memory::MemoryStoreTrait;
use crate::provider::ToolSchema;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Largest single expand serve (chars). An archived result can be
/// arbitrarily large, so a serve is bounded and the paging pointer tells the
/// model how to get the rest — never a silent cut.
pub(crate) const EXPAND_MAX_CHARS: usize = 60_000;

/// Cap on the number of rows a `query` listing returns.
pub(crate) const ARCHIVE_SEARCH_LIMIT: usize = 20;

/// Arguments for `expand_result`. Exactly one of `id` / `query` is used, in
/// that order of precedence; `start_line`/`max_lines` page a large result.
#[derive(Debug, Deserialize)]
struct ExpandResultArgs {
    /// The archive row id named by a result's preview.
    #[serde(default)]
    id: Option<String>,
    /// A keyword query over the archive (used when `id` is absent).
    #[serde(default)]
    query: Option<String>,
    /// 1-indexed first line to serve (like `read_files`).
    #[serde(default)]
    start_line: Option<usize>,
    /// Maximum lines to serve from `start_line`.
    #[serde(default)]
    max_lines: Option<usize>,
}

/// The `expand_result` tool.
pub struct ExpandResultTool {
    /// The project's memory store — the archive's backing. `None` in
    /// contexts with no store (a CLI contract build, a bare test): the tool
    /// then returns a helpful error instead of pretending it can expand.
    store: Option<Arc<dyn MemoryStoreTrait>>,
}

impl ExpandResultTool {
    /// Create the tool over `store` (`None` when the context has no memory
    /// store attached).
    pub fn new(store: Option<Arc<dyn MemoryStoreTrait>>) -> Self {
        Self { store }
    }

    /// The archive store, or a helpful error result when none is attached.
    fn store_or_error(&self) -> std::result::Result<&Arc<dyn MemoryStoreTrait>, ToolResult> {
        self.store.as_ref().ok_or_else(|| {
            ToolResult::error(
                "no memory store is attached to this agent, so archived tool results cannot \
                 be expanded here — the archive lives in the project's memory DB",
            )
        })
    }
}

#[async_trait]
impl Tool for ExpandResultTool {
    fn name(&self) -> &str {
        "expand_result"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "expand_result",
            format!(
                "{} Retrieve the FULL original of a tool result that was archived to save \
                 context: an oversized or truncated result carries a preview naming its \
                 archive `id`. Pass `id` to serve that original, or `query` to search the \
                 archive by keyword when you do not have the id. Use `start_line`/`max_lines` \
                 to page a very large result. Read-only.",
                tool_contract::contract(
                    "`id` (or `query` to search the archive)",
                    r#"{"id":"<archive-id>"}"#
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": ["string", "null"],
                        "description": "The archive row id named by a result's preview."
                    },
                    "query": {
                        "type": ["string", "null"],
                        "description": "Keyword search over the archive, used when `id` is absent."
                    },
                    "start_line": {
                        "type": ["integer", "null"],
                        "description": "1-indexed first line to serve (default 1)."
                    },
                    "max_lines": {
                        "type": ["integer", "null"],
                        "description": "Maximum lines to serve from start_line."
                    }
                }
            }),
        )
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let args: ExpandResultArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        let store = match self.store_or_error() {
            Ok(s) => s,
            Err(err) => return err,
        };
        // `id` wins when both are supplied (the precise handle).
        if let Some(id) = args.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return self.expand_by_id(store, id, args.start_line, args.max_lines).await;
        }
        if let Some(query) = args.query.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return self.search(store, query).await;
        }
        ToolResult::error(
            "pass `id` to retrieve one archived result, or `query` to search the archive — \
             there is no zero-argument form",
        )
    }
}

impl ExpandResultTool {
    /// Serve one archived result by id (paged, redacted, capped) and record
    /// the `archive_expand` savings row with a NEGATIVE `tokens_saved` — a
    /// re-expansion subtracts from the levers' cumulative savings.
    async fn expand_by_id(
        &self,
        store: &Arc<dyn MemoryStoreTrait>,
        id: &str,
        start_line: Option<usize>,
        max_lines: Option<usize>,
    ) -> ToolResult {
        let fetched = match store.expand_tool_result(id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return ToolResult::error(format!(
                    "no archived tool result with id '{id}' — the archive holds only results \
                     this project archived (another project's archive is not visible here)"
                ))
            }
            Err(e) => return ToolResult::error(format!("expanding '{id}' failed: {e}")),
        };
        let lines: Vec<&str> = fetched.content.lines().collect();
        let (body, paging_note) = slice_lines(&lines, start_line, max_lines);
        let served = redact_secrets(&body);
        let capped = cap_serve(&served);
        let mut out = format!(
            "=== archived tool result {} (tool: {}, detail: {}) — {} chars original ===\n{}{}",
            fetched.id,
            fetched.tool.as_deref().unwrap_or("unknown"),
            fetched.detail.as_deref().unwrap_or("n/a"),
            fetched.char_count,
            capped,
            paging_note,
        );
        // Lever 3's own accounting: nothing was in context before (0), the
        // served text is added now — so `tokens_saved` is negative by
        // construction and the dashboard's net savings stay honest.
        let after = estimate_tokens(&out) as i64;
        out.push_str(
            "\n[expanded from the tool-result archive — this content re-enters context and is \
             counted against the levers' net savings]",
        );
        let savings = savings_event("archive_expand", &fetched.id, 0, after);
        ToolResult::success(out).with_data(json!({ "savings": [savings] }))
    }

    /// List archive rows matching `query` (id + provenance + snippet), so the
    /// model can then expand one by id.
    async fn search(&self, store: &Arc<dyn MemoryStoreTrait>, query: &str) -> ToolResult {
        let hits = match store.search_archive(query, ARCHIVE_SEARCH_LIMIT).await {
            Ok(h) => h,
            Err(e) => return ToolResult::error(format!("archive search failed: {e}")),
        };
        if hits.is_empty() {
            return ToolResult::success(format!(
                "no archived tool result matches '{query}'. The archive holds only results too \
                 large to keep in context."
            ));
        }
        let mut out = format!("{} archived result(s) matching '{query}':\n", hits.len());
        for hit in &hits {
            out.push_str(&format!(
                "- id: {} (tool: {}, detail: {})\n  {}\n",
                hit.id,
                hit.tool.as_deref().unwrap_or("unknown"),
                hit.detail.as_deref().unwrap_or("n/a"),
                redact_secrets(&hit.snippet).replace('\n', " "),
            ));
        }
        out.push_str("[expand one by id to retrieve its full content]");
        ToolResult::success(out)
    }
}

/// Apply `start_line`/`max_lines` (1-indexed, like `read_files`) to the
/// archived content's lines. Returns the numbered body and a paging note
/// naming how to reach the remainder (empty when the whole result is served).
fn slice_lines(
    lines: &[&str],
    start_line: Option<usize>,
    max_lines: Option<usize>,
) -> (String, String) {
    let total = lines.len();
    let start = start_line.unwrap_or(1).max(1);
    if start > total {
        return (
            String::new(),
            format!("\n[start_line {start} is past the end of the result ({total} lines)]"),
        );
    }
    let idx = start - 1;
    let end = match max_lines {
        Some(n) if n > 0 => (idx + n).min(total),
        _ => total,
    };
    let mut body = String::new();
    for (offset, line) in lines[idx..end].iter().enumerate() {
        body.push_str(&format!("{:>5}: {line}\n", idx + offset + 1));
    }
    let note = if end < total {
        format!(
            "\n[... {} of {total} lines shown — pass start_line={} for the rest]",
            end - idx,
            end + 1
        )
    } else {
        String::new()
    };
    (body, note)
}

/// Bound one expand serve, naming what was dropped and how to page it — a
/// large original is never silently cut.
fn cap_serve(text: &str) -> String {
    let total = text.chars().count();
    if total <= EXPAND_MAX_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(EXPAND_MAX_CHARS).collect();
    format!(
        "{head}\n[expand serve capped at {EXPAND_MAX_CHARS} chars: {} of {total} dropped — \
         pass start_line/max_lines to page the archive]",
        total - EXPAND_MAX_CHARS
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{HashEmbedder, MemoryStore};

    /// An in-memory store with the archive schema applied.
    fn store() -> Arc<dyn MemoryStoreTrait> {
        Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap())
    }

    #[tokio::test]
    async fn expand_by_id_serves_the_original_and_records_a_negative_savings_row() {
        // The round-trip the whole lever exists for: an archived original is
        // served back, and the re-expansion is accounted as a NEGATIVE saving.
        let store = store();
        let content = format!("HEAD_MARKER\n{}\nTAIL_MARKER\n", "body line\n".repeat(3_000));
        let id = store
            .archive_tool_result(None, "shell", Some("cargo test"), &content)
            .await
            .unwrap();
        let tool = ExpandResultTool::new(Some(Arc::clone(&store)));
        let result = tool.execute(json!({"id": id})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("archived tool result"), "{}", result.output);
        assert!(result.output.contains("cargo test"), "{}", result.output);
        assert!(result.output.contains("HEAD_MARKER"), "{}", result.output);
        assert!(result.output.contains("TAIL_MARKER"), "{}", result.output);
        let data = result.data.expect("data");
        let savings = data["savings"].as_array().expect("savings array");
        assert_eq!(savings[0]["kind"], "archive_expand");
        let before = savings[0]["tokens_before"].as_i64().unwrap();
        let after = savings[0]["tokens_after"].as_i64().unwrap();
        // Nothing was in context before; the served text is added now.
        assert_eq!(before, 0, "a re-expansion starts from nothing in context");
        assert!(after > 0 && after > before, "re-expansion ADDS tokens: {before} -> {after}");
    }

    #[tokio::test]
    async fn expand_query_lists_matches_with_snippets() {
        let store = store();
        let id = store
            .archive_tool_result(
                None,
                "shell",
                Some("cargo build"),
                "noise\nSEARCHABLE_TERM failed to link\nnoise",
            )
            .await
            .unwrap();
        store
            .archive_tool_result(None, "read_files", Some("src/z.rs"), "unrelated content")
            .await
            .unwrap();
        let tool = ExpandResultTool::new(Some(Arc::clone(&store)));
        let result = tool.execute(json!({"query": "SEARCHABLE_TERM"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains(&id), "{}", result.output);
        assert!(result.output.contains("SEARCHABLE_TERM"), "{}", result.output);
        assert!(!result.output.contains("unrelated content"), "listing must not dump content");
        // No match: a helpful message, not an error.
        let none = tool.execute(json!({"query": "NOSUCHTERM_QQQ"})).await;
        assert!(none.success, "{}", none.output);
        assert!(none.output.contains("no archived tool result matches"), "{}", none.output);
    }

    #[tokio::test]
    async fn expand_requires_an_id_or_a_query() {
        let tool = ExpandResultTool::new(Some(store()));
        for args in [json!({}), json!({"id": "   "})] {
            let result = tool.execute(args.clone()).await;
            assert!(!result.success, "{args} must be rejected: {}", result.output);
            assert!(result.output.contains("pass `id`"), "{}", result.output);
        }
    }

    #[tokio::test]
    async fn expand_without_a_store_is_a_helpful_error() {
        let tool = ExpandResultTool::new(None);
        let result = tool.execute(json!({"id": "whatever"})).await;
        assert!(!result.success);
        assert!(result.output.contains("no memory store"), "{}", result.output);
    }

    #[tokio::test]
    async fn expand_unknown_id_is_an_error_not_a_panic() {
        let tool = ExpandResultTool::new(Some(store()));
        let result = tool.execute(json!({"id": "does-not-exist"})).await;
        assert!(!result.success);
        assert!(result.output.contains("no archived tool result with id"), "{}", result.output);
    }

    #[tokio::test]
    async fn expand_paging_serves_the_requested_line_range() {
        let store = store();
        let content: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let id = store
            .archive_tool_result(None, "shell", Some("cmd"), &content)
            .await
            .unwrap();
        let tool = ExpandResultTool::new(Some(Arc::clone(&store)));
        let result = tool
            .execute(json!({"id": id, "start_line": 10, "max_lines": 5}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("line 10"), "{}", result.output);
        assert!(result.output.contains("line 14"), "{}", result.output);
        assert!(!result.output.contains("line 15"), "{}", result.output);
        // The paging pointer names how to reach the rest.
        assert!(result.output.contains("start_line=15"), "{}", result.output);
    }
}

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `memory_search` — the single read path into the semantic memory system.
//!
//! One tool replaces six (`memory_recall`, `memory_list`, `plans_search`,
//! `reviews_search`, `past_fixes`, `context_pack`) that differed only by
//! parameter values: a record-type constant, a tier, or the presence of a
//! query. Read-only (auto-run); ranking goes through the store's normal
//! recall pipeline, so per-class derived caps and recency decay apply
//! automatically and a large indexed corpus stays bounded.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::memory::{MemoryFilter, MemoryRecordType, MemoryStoreTrait};
use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// The output cap for `memory_search` SEARCH mode — sized so a full
/// DEFAULT page (limit 12, worst-case ~200-char content digests ≈
/// 12 × ~360 chars ≈ 4.4 KB) survives untruncated: the chat's expandable
/// matched-hits card parses exactly this row list, and a truncated page
/// would offer fewer expandable rows than the card's "N matched" chip
/// (backlog 41f39672 review finding 1, 2026-09-17). Explicit larger
/// limits may still exceed it — the truncation note then rides the
/// output. (Successor of the old CONTEXT_PACK_CAP_CHARS 2048, which
/// despite its name governed search output.)
const SEARCH_CAP_CHARS: usize = 6144;

/// Format one scored memory as a recall-style line — the same
/// `[tier] title (id: …, score: …)` shape as `memory_recall` (the frontend
/// parser's regexes match it), with the content capped at 200 chars.
fn format_hit(sm: &crate::memory::ScoredMemory) -> String {
    // Superseded rows only appear when the caller opted into history — flag
    // them so a stale fact is never mistaken for live knowledge. The suffix
    // sits AFTER the pinned "(id/score/strength)" group so the frontend
    // parser's tier/title/snippet regexes are unaffected.
    let superseded = if sm.memory.superseded_by.is_some() {
        " [superseded]"
    } else {
        ""
    };
    format!(
        "[{}] {} (id: {}, score: {:.2}, strength: {:.2}){}\n  {}\n\n",
        sm.memory.tier,
        sm.memory.title,
        sm.memory.id,
        sm.score,
        sm.memory.strength,
        superseded,
        sm.memory.content.chars().take(200).collect::<String>(),
    )
}

/// Arguments for `memory_search` — the union of the six read tools it
/// replaced. Every field is optional: the presence of `query` is what selects
/// SEARCH mode over BROWSE mode.
#[derive(Debug, Deserialize, Default)]
struct MemorySearchArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    record_type: Option<String>,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_superseded: bool,
}

/// `memory_search` — the single read path into the memory store.
///
/// Replaces six tools that were one decision wearing six hats: `memory_recall`
/// (search all tiers), `memory_list` (browse with no query), `plans_search` /
/// `reviews_search` / `past_fixes` (the SAME `typed_recall` function with
/// three different `MemoryRecordType` constants) and `context_pack` (search
/// across all record types).
///
/// They differed only by parameter values, never by what the caller had to
/// decide — so the model paid ~3.3k characters of schema to be handed an
/// arbitrary six-way choice, on top of the real decision ("should I look
/// something up?"). The MANDATORY triggers that used to be attached to the
/// tool *names* now live in TOOL STRATEGY, attached to `record_type` values.
///
/// Two modes, selected by `query`:
/// - **absent** → BROWSE: `list_filtered`, newest first, one line per record.
/// - **present** → SEARCH: ranked `recall`, the `[tier] title (id, score,
///   strength)` shape the frontend's memory parser matches.
pub struct MemorySearchTool {
    store: Arc<dyn MemoryStoreTrait>,
}

impl MemorySearchTool {
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_search",
            "Read the memory store — the ONE way to look anything up. With a `query` it \
             ranks matches semantically; without one it browses newest-first. Narrow with \
             `record_type` (plan/review/bug/decision/spec/how), `tier`, or `prefix`. \
             Pointer-first: a typed record carries the gist plus a path/commit pointer — \
             read the file for detail.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Natural-language query. Omit to browse instead of search."},
                    "record_type": {"type": "string", "enum": ["spec", "decision", "bug", "plan", "how", "review", "none"], "description": "Restrict to one typed-record class. Omit to span all of them."},
                    "tier": {"type": "string", "enum": ["working", "episodic", "semantic", "procedural"], "description": "Optional: restrict to one tier."},
                    "prefix": {"type": "string", "description": "Optional: title starts-with filter (case-sensitive), e.g. \"BUG: login\"."},
                    "limit": {"type": "integer", "description": "Max rows (default 12 searching, 50 browsing)."},
                    "include_superseded": {"type": "boolean", "description": "Include superseded records (history), annotated [superseded]. Default false — they are excluded, not downranked."}
                }
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemorySearchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };

        let mut filter = MemoryFilter::new();
        if let Some(tier_str) = &args.tier {
            match crate::memory::MemoryTier::from_str(tier_str) {
                Some(t) => filter = filter.tier(t),
                None => return ToolResult::error(format!("unknown tier '{tier_str}'")),
            }
        }
        if let Some(rt_str) = &args.record_type {
            match MemoryRecordType::from_str(rt_str) {
                Some(rt) => filter = filter.record_type(rt),
                None => {
                    return ToolResult::error(format!(
                        "unknown record_type '{rt_str}' — valid: spec, decision, bug, plan, how, review, none"
                    ))
                }
            }
        }
        if let Some(prefix) = &args.prefix {
            filter = filter.title_prefix(prefix.clone());
        }
        if args.include_superseded {
            filter = filter.include_superseded();
        }

        let query = args
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty());
        match query {
            // ── BROWSE ──────────────────────────────────────────────────
            None => {
                filter = filter.limit(args.limit.unwrap_or(50));
                match self.store.list_filtered(&filter).await {
                    Ok(memories) => {
                        if memories.is_empty() {
                            return ToolResult::success("no memories match the filter");
                        }
                        let mut out = format!("{} memories (newest first):\n\n", memories.len());
                        for m in &memories {
                            let superseded = if m.superseded_by.is_some() {
                                " [superseded]"
                            } else {
                                ""
                            };
                            out.push_str(&format!(
                                "{}  {}  {}  {}  {}{}\n",
                                m.id,
                                m.tier,
                                m.record_type,
                                crate::tool::memory::format_epoch_date(m.created_at),
                                m.title,
                                superseded,
                            ));
                        }
                        ToolResult::success(out)
                    }
                    Err(e) => ToolResult::error(format!("failed to list memories: {e}")),
                }
            }
            // ── SEARCH ──────────────────────────────────────────────────
            Some(q) => {
                filter = filter.limit(args.limit.unwrap_or(12));
                match self.store.recall(q, &filter).await {
                    Ok(results) => {
                        if results.is_empty() {
                            return ToolResult::success("no memories matched the query");
                        }
                        let mut out = format!("{} memories matched:\n\n", results.len());
                        for sm in &results {
                            out.push_str(&format_hit(sm));
                        }
                        // Beyond a full default page (SEARCH_CAP_CHARS —
                        // see the const), keep the bundle bounded; the
                        // note tells the model to narrow the query.
                        if out.chars().count() > SEARCH_CAP_CHARS {
                            out = out.chars().take(SEARCH_CAP_CHARS).collect();
                            out.push_str("\n… [truncated — narrow the query]");
                        }
                        ToolResult::success(out)
                    }
                    Err(e) => ToolResult::error(format!("failed to search memories: {e}")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::{Memory, MemoryClass, MemoryStore, MemoryTier};

    fn make_store() -> Arc<dyn MemoryStoreTrait> {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
    }

    /// Seed a store with one record of each typed class (authored) plus one
    /// derived PLAN digest, all mentioning a shared keyword.
    async fn seed(store: &Arc<dyn MemoryStoreTrait>) {
        for (title, content) in [
            (
                "PLAN: storage migration",
                "migrate storage to sqlite with a plan file",
            ),
            (
                "REVIEW: storage review",
                "review verdict: pass, 2 findings, report path",
            ),
            (
                "BUG: storage crash",
                "root cause: unwrap on None in storage open",
            ),
            ("DECISION: storage engine", "we use sqlite for storage"),
            (
                "SPEC: storage api",
                "the storage api exposes open/close/read",
            ),
        ] {
            store
                .write(Memory::new(MemoryTier::Semantic, title, content, 1000))
                .await
                .unwrap();
        }
        // One derived PLAN digest (indexer-style) — must rank through the
        // same pipeline and be capped by the per-class backstop.
        let mut derived = Memory::new(
            MemoryTier::Semantic,
            "PLAN: derived digest",
            "indexer-built plan digest about storage",
            1000,
        );
        derived.record_class = MemoryClass::Derived;
        store.write(derived).await.unwrap();
    }

    #[tokio::test]
    async fn record_type_scopes_the_search() {
        // What plans_search / reviews_search / past_fixes each used to be:
        // the same query, narrowed by one record_type constant.
        let store = make_store();
        seed(&store).await;
        let tool = MemorySearchTool::new(store);

        for (record_type, marker, absent) in [
            ("plan", "PLAN:", "REVIEW:"),
            ("review", "REVIEW:", "BUG:"),
            ("bug", "BUG:", "PLAN:"),
        ] {
            let result = tool
                .execute(json!({"query": "storage", "record_type": record_type}))
                .await;
            assert!(result.success, "{}", result.output);
            assert!(
                result.output.contains(marker),
                "{record_type} search surfaces {marker}: {}",
                result.output
            );
            assert!(
                !result.output.contains(absent),
                "{record_type} search excludes {absent}: {}",
                result.output
            );
        }
    }

    #[tokio::test]
    async fn search_without_record_type_spans_all_classes_and_stays_capped() {
        // What context_pack used to be: one query across every record class,
        // pointer-first and bounded.
        let store = make_store();
        seed(&store).await;
        let tool = MemorySearchTool::new(store);
        let result = tool.execute(json!({"query": "storage"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("PLAN:"), "{}", result.output);
        assert!(result.output.contains("REVIEW:"), "{}", result.output);
        assert!(result.output.contains("BUG:"), "{}", result.output);
        assert!(result.output.contains("(id: "), "{}", result.output);
        assert!(result.output.contains("score:"), "{}", result.output);

        // A store with many hits must not blow the budget.
        let big = make_store();
        for i in 0..40 {
            big.write(Memory::new(
                MemoryTier::Semantic,
                format!("PLAN: digest {i}"),
                format!("storage plan digest number {i} with a long tail of detail"),
                1000,
            ))
            .await
            .unwrap();
        }
        let tool = MemorySearchTool::new(big);
        let result = tool.execute(json!({"query": "storage"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.chars().count() <= SEARCH_CAP_CHARS + 128,
            "bundle stays near the cap: {} chars",
            result.output.chars().count()
        );
    }

    #[tokio::test]
    async fn search_default_page_survives_untruncated() {
        // Backlog 41f39672 review finding 1: the search-mode cap must let a
        // full DEFAULT page (limit 12) through untruncated — the expandable
        // matched-hits card parses exactly these rows, and its chip says
        // "12 matched" while a truncated page would offer fewer rows.
        let store = make_store();
        for i in 0..12 {
            store
                .write(Memory::new(
                    MemoryTier::Semantic,
                    format!("PLAN: page row {i}"),
                    format!("storage digest {i} {}", "x".repeat(180)),
                    1000,
                ))
                .await
                .unwrap();
        }
        let tool = MemorySearchTool::new(store);
        let result = tool.execute(json!({"query": "storage"})).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            result.output.matches("(id: ").count(),
            12,
            "all 12 default-page rows present: {}",
            result.output
        );
        assert!(
            !result.output.contains("[truncated"),
            "a full default page is never truncated: {}",
            result.output
        );

        // An explicitly huge page still truncates gracefully with the note.
        let big = make_store();
        for i in 0..30 {
            big.write(Memory::new(
                MemoryTier::Semantic,
                format!("PLAN: wide row {i}"),
                format!("storage digest {i} {}", "y".repeat(180)),
                1000,
            ))
            .await
            .unwrap();
        }
        let tool = MemorySearchTool::new(big);
        let result = tool.execute(json!({"query": "storage", "limit": 30})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("[truncated"),
            "an over-cap explicit page still truncates: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn omitting_the_query_browses_instead_of_searching() {
        // What memory_list used to be. The presence of `query` is the only
        // thing that selects between the two modes.
        let store = make_store();
        seed(&store).await;
        let tool = MemorySearchTool::new(store);

        let browsed = tool.execute(json!({})).await;
        assert!(browsed.success, "{}", browsed.output);
        assert!(
            browsed.output.contains("newest first"),
            "browse mode lists newest-first: {}",
            browsed.output
        );

        // Browsing accepts the same narrowing filters as searching.
        let typed = tool.execute(json!({"record_type": "bug"})).await;
        assert!(typed.success, "{}", typed.output);
        assert!(typed.output.contains("BUG:"), "{}", typed.output);
        assert!(!typed.output.contains("REVIEW:"), "{}", typed.output);

        // An empty/whitespace query is treated as absent rather than as a
        // query that matches nothing — a model emitting "" still gets a
        // useful browse instead of a dead end.
        let blank = tool.execute(json!({"query": "   "})).await;
        assert!(blank.success, "{}", blank.output);
        assert!(blank.output.contains("newest first"), "{}", blank.output);
    }

    #[tokio::test]
    async fn empty_store_returns_no_match_success() {
        let store = make_store();
        let tool = MemorySearchTool::new(store);
        for args in [
            json!({"query": "x"}),
            json!({"query": "x", "record_type": "plan"}),
            json!({"query": "x", "record_type": "bug"}),
            json!({}),
        ] {
            let result = tool.execute(args.clone()).await;
            assert!(result.success, "{args}: {}", result.output);
            assert!(
                result.output.starts_with("no memories"),
                "{args}: {}",
                result.output
            );
        }
    }

    #[tokio::test]
    async fn unknown_filter_values_error_with_the_valid_set() {
        let tool = MemorySearchTool::new(make_store());
        let r = tool
            .execute(json!({"query": "x", "record_type": "nope"}))
            .await;
        assert!(!r.success);
        assert!(r.output.contains("spec, decision, bug"), "{}", r.output);

        let r = tool.execute(json!({"query": "x", "tier": "nope"})).await;
        assert!(!r.success);
        assert!(r.output.contains("unknown tier"), "{}", r.output);
    }

    #[tokio::test]
    async fn superseded_records_stay_hidden_by_default() {
        let store = make_store();
        let old_id = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "BUG: old crash",
                "old root cause: null deref",
                1000,
            ))
            .await
            .unwrap();
        store
            .supersede_memory(
                &old_id,
                Memory::new(
                    MemoryTier::Semantic,
                    "BUG: old crash",
                    "new root cause: use-after-free",
                    1001,
                ),
            )
            .await
            .unwrap();
        let tool = MemorySearchTool::new(store);
        let result = tool
            .execute(json!({"query": "crash", "record_type": "bug"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("null deref"),
            "superseded content stays hidden: {}",
            result.output
        );
        assert!(
            result.output.contains("use-after-free"),
            "{}",
            result.output
        );

        // ...and opting into history brings it back, annotated.
        let hist = tool
            .execute(json!({"query": "crash", "record_type": "bug", "include_superseded": true}))
            .await;
        assert!(hist.success, "{}", hist.output);
        assert!(hist.output.contains("[superseded]"), "{}", hist.output);
    }
}

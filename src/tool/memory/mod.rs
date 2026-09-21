// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Memory tools — `memory_write`, `memory_recall`, `memory_consolidate`, plus the
//! hygiene tools `memory_update`, `memory_amend`, `memory_supersede`, `memory_delete`,
//! `memory_list`, and the Phase-3 retrieval tools `plans_search`, `reviews_search`,
//! `past_fixes`, `context_pack` (in [`retrieval`]).
//!
//! Always available regardless of workflow state. All are auto-run — they
//! only touch the project's own memory store (sandboxed bookkeeping), so they
//! never prompt for approval.

pub mod retrieval;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::memory::consolidation::consolidate_session;
use crate::memory::indexer::{index_derived, reindex_knowledge_files};
use crate::memory::knowledge::{self, KnowledgeStore};
use crate::memory::{
    Memory, MemoryClass, MemoryFilter, MemoryRecordType, MemoryStoreTrait, MemoryTier,
};
use crate::provider::{LlmClient, SwappableProvider, ToolSchema};
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Arguments for `memory_write`.
#[derive(Debug, Deserialize)]
struct MemoryWriteArgs {
    tier: String,
    title: String,
    content: String,
}

/// Arguments for `memory_recall`.
#[derive(Debug, Deserialize)]
struct MemoryRecallArgs {
    query: String,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_superseded: bool,
}

/// Arguments for `memory_consolidate`.
#[derive(Debug, Deserialize)]
struct MemoryConsolidateArgs {
    session_id: String,
}

/// The process-global count of successful knowledge-file writes (all agents
/// — the store is factory-shared). The run-all completion note diffs this
/// across the run window to surface knowledge written during the run
/// (memory review 2026-09-08, suggestion 3).
pub static KNOWLEDGE_WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The `memory_write` tool.
pub struct MemoryWriteTool {
    store: Arc<dyn MemoryStoreTrait>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    /// The knowledge-file backing — `None` keeps the historical DB-only
    /// behavior (and is the test-store path).
    knowledge: Option<KnowledgeSupport>,
    /// The sandbox root of the agent this tool serves, when it differs from
    /// the knowledge store's main-tree root — a run-all lane agent is rooted
    /// in its worktree while the knowledge store stays factory-wide (the
    /// shared corpus). `None` (main-tree agents, tests) means no lane note.
    agent_root: Option<PathBuf>,
}

impl MemoryWriteTool {
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self {
            store,
            now: Box::new(utc_now_secs),
            knowledge: None,
            agent_root: None,
        }
    }

    /// Wire the knowledge-file backing: typed writes then land in
    /// `.coding/knowledge/` (files as truth) + a targeted reindex.
    pub fn with_knowledge(
        store: Arc<dyn MemoryStoreTrait>,
        knowledge: Arc<KnowledgeStore>,
        plans_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            knowledge: Some(KnowledgeSupport {
                knowledge,
                store: store.clone(),
                plans_dir: plans_dir.into(),
            }),
            store,
            now: Box::new(utc_now_secs),
            agent_root: None,
        }
    }

    /// Mark this tool as serving a worktree-rooted agent (a run-all lane):
    /// the knowledge store stays factory-wide (the main tree), so the result
    /// message says the record landed in the MAIN project tree — the lane's
    /// own file tools cannot read the reported path.
    pub fn with_agent_root(mut self, root: Option<PathBuf>) -> Self {
        self.agent_root = root;
        self
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_write",
            "MANDATORY the moment you learn a durable fact, decision, convention, or bug \
             root cause — do NOT defer to session end; write it immediately or it is lost. \
             Memories persist across sessions and are auto-recalled into future prompts: this \
             is how you learn. Typed title prefixes classify the record \
             (SPEC:/DECISION:/BUG:/PLAN:/HOW:/REVIEW:) as a pointer to on-disk \
             truth — keep it compact (gist + path/commit pointer); the file \
             carries the detail. \
             All three fields (tier, title, content) are required — e.g. \
             {\"tier\":\"semantic\",\"title\":\"DECISION: …\",\"content\":\"…\"}. No \
             zero-argument form; on a required-field error rewrite the full \
             call, do not resend the empty shape.",
            json!({
                "type": "object",
                "properties": {
                    "tier": {"type": "string", "enum": ["working", "episodic", "semantic", "procedural"], "description": "Memory tier: working (raw events), episodic (session summaries), semantic (facts), procedural (workflows)."},
                    "title": {"type": "string", "description": "Short label, used in recall listings; carries the typed prefix."},
                    "content": {"type": "string", "description": "The memory body — the full text to persist and recall later."}
                },
                "required": ["tier", "title", "content"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Writes only to the project's own memory store (sandboxed
        // bookkeeping) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemoryWriteArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            // Backlog d9ad618e: the recovery rule rides the error (the
            // read_files precedent, backlog 26cdbaf8).
            Err(e) => {
                return ToolResult::error(crate::tool::agent::read_files::invalid_args_error(
                    "memory_write",
                    &e,
                    &args,
                    "All of tier, title, content are required — there is no \
                     zero-argument form; rewrite the full call, do not resend \
                     the empty shape.",
                ))
            }
        };
        let tier = match MemoryTier::from_str(&args.tier) {
            Some(t) => t,
            None => return ToolResult::error(format!("unknown tier '{}'", args.tier)),
        };
        let title = args.title;
        let record_type = MemoryRecordType::from_title(&title);
        // Knowledge-backed typed write: the record's FILE is the truth — the
        // body is UNBOUNDED (the indexer budgets the derived digest, so the
        // digest stays a budgeted pointer while the file carries everything).
        let is_knowledge_type = record_type != MemoryRecordType::None
            && knowledge::dir_for_record_type(record_type).is_some();
        // Knowledge-backed write: the record's FILE is the truth —
        // write it, reindex the one file (the derived row), and report the
        // deterministic row id. Without a knowledge
        // backing (tests, non-rooted plans dirs) a typed write falls back
        // to the historical DB-only row.
        if let Some(support) = self.knowledge.as_ref().filter(|_| is_knowledge_type) {
            let body = args.content.clone();
            match support.knowledge.write(record_type, &title, &body) {
                Ok(rel) => match support.reindex(&[rel.clone()]).await {
                    Ok(()) => {
                        // Count the knowledge-file write (the run-all
                        // completion note diffs this across the run window).
                        KNOWLEDGE_WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let row_id = crate::memory::indexer::knowledge_id(&rel);
                        let (phrase, suffix) = tier_summary(tier);
                        let mut msg =
                            format!("Saved {phrase} \"{}\" ({} memory)", title, tier.as_str());
                        if !suffix.is_empty() {
                            msg.push_str(" — ");
                            msg.push_str(suffix);
                        }
                        msg.push_str(&format!(
                            " — knowledge record at .coding/knowledge/{rel} (files are the truth; \
                             the memory row is a budgeted digest)"
                        ));
                        // A run-all lane agent is rooted in its worktree, but the
                        // knowledge store is factory-wide (the shared main-tree
                        // corpus by design) — the path above is main-tree-relative,
                        // so say so explicitly: the lane's own file tools cannot
                        // read it (no not-found detour). The root comparison is
                        // exact component equality and assumes both sides share
                        // the same source spelling (both derive from the same
                        // project-root source; verbatim-prefixed or 8.3 short-name
                        // spellings would compare unequal — unreachable here).
                        let store_root =
                            support.knowledge.dir().parent().and_then(Path::parent);
                        if let Some(agent_root) = self.agent_root.as_deref() {
                            if Some(agent_root) != store_root {
                                msg.push_str(
                                    " — landed in the MAIN project tree, not this worktree \
                                     (this lane's file tools are worktree-rooted and cannot \
                                     read that path)",
                                );
                            }
                        }
                        ToolResult::success(msg).with_data(json!({
                            "id": row_id,
                            "tier": tier.as_str(),
                            "title": title,
                            "record_type": record_type.as_str(),
                            "knowledge": format!(".coding/knowledge/{rel}"),
                        }))
                    }
                    Err(e) => ToolResult::error(format!("failed to index knowledge record: {e}")),
                },
                Err(e) => ToolResult::error(format!("failed to write knowledge record: {e}")),
            }
        } else {
            let now = (self.now)();
            let memory = Memory::new(tier, title.clone(), args.content, now);
            match self.store.write(memory).await {
                Ok(id) => {
                    // Human-readable confirmation (backlog 2026-08-20: "wrote
                    // semantic memory (id: …) is a meaningless message") — name
                    // WHAT was saved (the title) and what happens to it, per
                    // tier. The write id is machine-relevant only (recall is
                    // query-based; nothing addresses memories by id), so it goes
                    // in structured `data` instead of the human text.
                    let (phrase, suffix) = tier_summary(tier);
                    let mut msg =
                        format!("Saved {phrase} \"{}\" ({} memory)", title, tier.as_str());
                    if !suffix.is_empty() {
                        msg.push_str(" — ");
                        msg.push_str(suffix);
                    }
                    ToolResult::success(msg).with_data(json!({
                        "id": id,
                        "tier": tier.as_str(),
                        "title": title,
                    }))
                }
                Err(e) => ToolResult::error(format!("failed to write memory: {e}")),
            }
        }
    }
}

/// The human-facing phrase + explanation for each memory tier, used by
/// `memory_write`'s confirmation message so the sentence says what kind of
/// thing was saved and what happens to it — instead of the old machine-flavored
/// `wrote {tier} memory (id: <uuid>)`.
///
/// Returns `(phrase, suffix)`; an empty `suffix` means no trailing clause.
fn tier_summary(tier: MemoryTier) -> (&'static str, &'static str) {
    match tier {
        MemoryTier::Working => (
            "a session event",
            "consolidation will distill it into summaries and facts later",
        ),
        MemoryTier::Episodic => ("a session summary", ""),
        MemoryTier::Semantic => (
            "a durable fact",
            "it will be recalled in future sessions when relevant",
        ),
        MemoryTier::Procedural => (
            "a reusable workflow",
            "it will be recalled when a matching task comes up",
        ),
    }
}

/// Wall-clock seconds since the Unix epoch — the `now` source for the
/// writing tools (`memory_write` / `memory_supersede`) so both stamp
/// `created_at` the same way.
fn utc_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The knowledge-file backing shared by the memory tools that write typed
/// records (`memory_write`, `memory_update`, `memory_supersede`,
/// `memory_delete`). When wired, a typed write becomes a FILE write to
/// `.coding/knowledge/<type>/` (the truth) plus a targeted reindex (the DB
/// row); when `None`, the tools keep their historical DB-only behavior —
/// which is also the test-store path.
///
/// Supersede always ends with a full `index_derived` run: the successor
/// FILE flips an *unchanged* predecessor, and only the full scan performs
/// that reconciliation (the same invariant `index_derived` documents).
struct KnowledgeSupport {
    /// The file-backed writer (rooted at `.coding/knowledge/`).
    knowledge: Arc<KnowledgeStore>,
    /// The memory store driving the reindex (trait object — the same one
    /// the tool holds).
    store: Arc<dyn MemoryStoreTrait>,
    /// `.coding/plans` — locates the corpus for the full post-supersede
    /// reindex (the indexer's other source families).
    plans_dir: PathBuf,
}

/// The knowledge-file metadata for a derived memory id — what
/// [`KnowledgeSupport::knowledge_for_id`] resolves: the rel path plus the
/// record's title/tier, so tool results can name the amended record
/// instead of a bare id (backlog d779060a: "Amended [semantic] 'SPEC:
/// storage' (id …)").
struct KnowledgeRef {
    rel: String,
    title: String,
    tier: MemoryTier,
}

impl KnowledgeSupport {
    fn knowledge_dir(&self) -> &Path {
        self.knowledge.dir()
    }

    /// The knowledge-file metadata for a derived memory id — the rel path
    /// (`.coding/knowledge/<type>/<slug>.md`, read from the row's
    /// `data.rel_path`) plus the record's title/tier, surfaced so tool
    /// results can name the amended record instead of a bare id (backlog
    /// d779060a). `None` for DB-native rows, missing rows, and rows without
    /// the field.
    async fn knowledge_for_id(&self, id: &str) -> Option<KnowledgeRef> {
        let m = self.store.get_memory(id).await.ok()??;
        if m.record_class != MemoryClass::Derived {
            return None;
        }
        let rel = m.data.get("rel_path")?.as_str()?.to_string();
        // Only knowledge rows carry rel_path (plans/reviews/backlog rows
        // don't have the field at all, so this is belt-and-braces).
        if rel.starts_with("spec/")
            || rel.starts_with("decision/")
            || rel.starts_with("bug/")
            || rel.starts_with("how/")
        {
            Some(KnowledgeRef {
                rel,
                title: m.title,
                tier: m.tier,
            })
        } else {
            None
        }
    }

    /// A targeted reindex of the given knowledge files.
    async fn reindex(&self, rels: &[String]) -> Result<(), String> {
        let report = reindex_knowledge_files(self.store.as_ref(), self.knowledge_dir(), rels)
            .await
            .map_err(|e| e.to_string())?;
        if !report.errors.is_empty() {
            return Err(format!(
                "knowledge reindex reported {} error(s): {}",
                report.errors.len(),
                report.errors.join("; ")
            ));
        }
        Ok(())
    }

    /// A full derived-index run — used after supersede (the successor file
    /// changes an unchanged predecessor's row, which only the full scan
    /// reconciles). Best-effort: a failure surfaces as a warning, never
    /// fails the tool (the FILE is the truth; the row catches up next run).
    async fn full_index(&self) {
        if let Err(e) = index_derived(
            self.store.as_ref(),
            &self.plans_dir,
            &self
                .plans_dir
                .parent()
                .map(|p| p.join("backlog.jsonl"))
                .unwrap_or_else(|| PathBuf::from(".coding/backlog.jsonl")),
            &(|_: usize, _: usize| {}),
        )
        .await
        {
            eprintln!("knowledge post-supersede index failed: {e}");
        }
    }
}

/// Format epoch seconds as a UTC calendar date (`YYYY-MM-DD`) for
/// `memory_list`'s per-memory lines. The crate has no chrono dependency, so
/// this is Howard Hinnant's `civil_from_days` algorithm (exact for the full
/// day range, negative days included).
pub(crate) fn format_epoch_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// The `memory_recall` tool.
pub struct MemoryRecallTool {
    store: Arc<dyn MemoryStoreTrait>,
}

impl MemoryRecallTool {
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_recall",
            "Recall memories matching a query, ranked by strength + relevance. Use BEFORE \
             starting non-trivial work on existing code (bug fixes, refactors, 'how did we…' \
             questions) and whenever the auto-recalled context looks insufficient — \
             auto-recall only covers the latest user message. Searches across all tiers by \
             default. Read-only (auto-run).",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Natural-language query; ranked by semantic + keyword relevance."},
                    "tier": {"type": "string", "enum": ["working", "episodic", "semantic", "procedural"], "description": "Optional: restrict to one tier. Omit to search all tiers."},
                    "limit": {"type": "integer", "description": "Optional: max results."},
                    "include_superseded": {"type": "boolean", "description": "Optional: include superseded memories (history), annotated [superseded]. Default false — superseded records are excluded, not merely downranked."}
                },
                "required": ["query"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemoryRecallArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let mut filter = MemoryFilter::new();
        if let Some(tier_str) = &args.tier {
            match MemoryTier::from_str(tier_str) {
                Some(t) => filter = filter.tier(t),
                None => return ToolResult::error(format!("unknown tier '{tier_str}'")),
            }
        }
        if let Some(limit) = args.limit {
            filter = filter.limit(limit);
        }
        if args.include_superseded {
            // Opt into history: superseded rows return with an annotation.
            filter = filter.include_superseded();
        }
        match self.store.recall(&args.query, &filter).await {
            Ok(results) => {
                if results.is_empty() {
                    return ToolResult::success("no memories matched the query");
                }
                let mut out = format!("{} memories matched:\n\n", results.len());
                for sm in &results {
                    // Superseded rows only appear when the caller opted into
                    // history — flag them so a stale fact is never mistaken
                    // for live knowledge. The suffix sits AFTER the pinned
                    // "(id/score/strength)" group so the frontend parser's
                    // tier/title/snippet regexes are unaffected. The id rides
                    // FIRST in the group (stable ids, Phase 1) so existing
                    // score/strength matches keep working.
                    let superseded = if sm.memory.superseded_by.is_some() {
                        " [superseded]"
                    } else {
                        ""
                    };
                    out.push_str(&format!(
                        "[{}] {} (id: {}, score: {:.2}, strength: {:.2}){}\n  {}\n\n",
                        sm.memory.tier,
                        sm.memory.title,
                        sm.memory.id,
                        sm.score,
                        sm.memory.strength,
                        superseded,
                        sm.memory.content.chars().take(200).collect::<String>()
                    ));
                }
                ToolResult::success(out)
            }
            Err(e) => ToolResult::error(format!("failed to recall: {e}")),
        }
    }
}

/// The `memory_consolidate` tool.
pub struct MemoryConsolidateTool {
    store: Arc<dyn MemoryStoreTrait>,
    /// Optional swappable provider handle. When set, consolidation runs the
    /// full pipeline (working → episodic → semantic → procedural extraction).
    /// When `None`, it stops at synthetic episodic compression — the historical
    /// behavior. The automatic session-end path always passes the live provider;
    /// this slot lets the manual tool match it.
    provider: Option<Arc<SwappableProvider>>,
    /// The project's plans dir. When non-empty, the plan/review corpus digest
    /// is built from it (parent dir's `reviews/` subdir) and fed to
    /// consolidation — matching the automatic session-end path.
    plans_dir: std::path::PathBuf,
}

impl MemoryConsolidateTool {
    /// Create a tool with no LLM provider — consolidation stops at synthetic
    /// episodic compression (the historical behavior). Used by tests.
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self {
            store,
            provider: None,
            plans_dir: std::path::PathBuf::new(),
        }
    }

    /// Create a tool backed by a swappable provider handle, so manual
    /// consolidation runs the full extraction pipeline (semantic + procedural)
    /// matching the automatic session-end path. The handle is shared with the
    /// factory, so a Settings model swap takes effect on the next call.
    /// `plans_dir` locates the plan/review corpus for the
    /// [`corpus_digest`](crate::memory::consolidation::corpus_digest).
    pub fn with_provider(
        store: Arc<dyn MemoryStoreTrait>,
        provider: Arc<SwappableProvider>,
        plans_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            store,
            provider: Some(provider),
            plans_dir: plans_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for MemoryConsolidateTool {
    fn name(&self) -> &str {
        "memory_consolidate"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_consolidate",
            "Run the consolidation pipeline for a session: compress working memory into an \
             episodic summary, then extract semantic facts + procedural workflows. Requires \
             an LLM for full extraction; without one, stops at episodic.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {"type": "string", "description": "The session whose working-memory events to compress into an episodic summary."}
                },
                "required": ["session_id"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Consolidation only rewrites the project's own memory store
        // (sandboxed bookkeeping) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemoryConsolidateArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Snapshot the provider (if a swappable handle was wired in) so manual
        // consolidation runs the full extraction pipeline — matching the
        // automatic session-end path. Without a handle, falls back to
        // synthetic episodic compression only.
        let provider: Option<Arc<dyn LlmClient>> =
            self.provider.as_ref().and_then(|slot| slot.get());
        let provider_ref = provider.as_deref();
        // Digest the plan/review corpus (same source as the automatic
        // session-end path) so manual consolidation learns from accumulated
        // plans + reviews too. Empty plans_dir → empty corpus (tests).
        let reviews_dir = if self.plans_dir.as_os_str().is_empty() {
            std::path::PathBuf::new()
        } else {
            self.plans_dir
                .parent()
                .map(|p| p.join("reviews"))
                .unwrap_or_else(|| std::path::PathBuf::from(".coding/reviews"))
        };
        let corpus = if self.plans_dir.as_os_str().is_empty() {
            String::new()
        } else {
            crate::memory::consolidation::corpus_digest(&self.plans_dir, &reviews_dir)
        };
        match consolidate_session(self.store.as_ref(), &args.session_id, provider_ref, &corpus)
            .await
        {
            Ok(id) => {
                if id.is_empty() {
                    ToolResult::success("no working-memory events to consolidate for this session")
                } else if provider_ref.is_some() {
                    ToolResult::success(format!(
                        "consolidated session {} into episodic + extracted semantic/procedural memories (id: {id})",
                        args.session_id
                    ))
                } else {
                    ToolResult::success(format!(
                        "consolidated session {} into episodic memory (id: {id})",
                        args.session_id
                    ))
                }
            }
            Err(e) => ToolResult::error(format!("consolidation failed: {e}")),
        }
    }
}

// ── Hygiene tools: update / supersede / delete / list ────────────────────

/// Arguments for `memory_update`.
#[derive(Debug, Deserialize)]
struct MemoryUpdateArgs {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    /// Targeted-repair mode (backlog 488248ce): the exact text to replace. It
    /// must occur exactly once in the record body.
    #[serde(default)]
    find: Option<String>,
    /// Targeted-repair mode: what `find` is replaced with (empty deletes it).
    #[serde(default)]
    replace_with: Option<String>,
}

/// The `memory_update` tool — refine an existing memory in place by id, or
/// repair stray text in a record body via its `find` / `replace_with` mode
/// (backlog 488248ce).
pub struct MemoryUpdateTool {
    store: Arc<dyn MemoryStoreTrait>,
    /// The knowledge-file backing — `None` keeps the historical DB-only
    /// behavior (and is the test-store path).
    knowledge: Option<KnowledgeSupport>,
}

impl MemoryUpdateTool {
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self {
            store,
            knowledge: None,
        }
    }

    /// `memory_update`'s targeted-repair mode (backlog 488248ce): a
    /// knowledge-backed id rewrites the record FILE (the truth) + a targeted
    /// reindex, so stray text is repairable in-tool while the file tools keep
    /// refusing `.coding/knowledge/**`; a DB-only id repairs the stored
    /// content directly.
    async fn repair_by_id(&self, id: &str, find: &str, replace_with: &str) -> ToolResult {
        let rel = match &self.knowledge {
            Some(support) => support.knowledge_for_id(id).await.map(|kr| kr.rel),
            None => None,
        };
        if let Some(rel) = rel {
            let Some(support) = &self.knowledge else {
                return ToolResult::error(format!(
                    "knowledge record {id} exists but the file backing is not wired"
                ));
            };
            if let Err(e) = support.knowledge.replace_in_body(&rel, find, replace_with) {
                return ToolResult::error(format!("failed to repair knowledge record: {e}"));
            }
            return match support.reindex(&[rel.clone()]).await {
                Ok(()) => ToolResult::success(format!(
                    "Repaired knowledge record {id} — replaced 1 occurrence in \
                     .coding/knowledge/{rel}; the memory row re-indexed"
                ))
                .with_data(json!({ "id": id, "knowledge": format!(".coding/knowledge/{rel}") })),
                Err(e) => ToolResult::error(format!("failed to index knowledge record: {e}")),
            };
        }
        let current = match self.store.get_memory(id).await {
            Ok(Some(m)) => m.content,
            Ok(None) => {
                return ToolResult::error(format!(
                    "no memory with id '{id}' — nothing was updated (find ids via \
                     memory_list / memory_recall)"
                ))
            }
            Err(e) => return ToolResult::error(format!("failed to read memory: {e}")),
        };
        let repaired = match knowledge::replace_unique(&current, find, replace_with) {
            Ok(body) => body,
            Err(e) => return ToolResult::error(format!("failed to repair memory: {e}")),
        };
        match self
            .store
            .update_memory(id, None, Some(repaired.as_str()))
            .await
        {
            Ok(true) => ToolResult::success(format!(
                "Repaired memory {id} — replaced 1 occurrence of the find text"
            ))
            .with_data(json!({ "id": id })),
            Ok(false) => ToolResult::error(format!(
                "no memory with id '{id}' — nothing was updated (find ids via \
                 memory_list / memory_recall)"
            )),
            Err(e) => ToolResult::error(format!("failed to update memory: {e}")),
        }
    }

    /// Wire the knowledge-file backing: updates to knowledge records then
    /// rewrite the FILE (the truth) + a targeted reindex.
    pub fn with_knowledge(
        store: Arc<dyn MemoryStoreTrait>,
        knowledge: Arc<KnowledgeStore>,
        plans_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            knowledge: Some(KnowledgeSupport {
                knowledge,
                store: store.clone(),
                plans_dir: plans_dir.into(),
            }),
            store,
        }
    }
}

#[async_trait]
impl Tool for MemoryUpdateTool {
    fn name(&self) -> &str {
        "memory_update"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_update",
            "Refine an existing memory in place by id (new title and/or content). Use to \
             correct or tighten a fact that is still CURRENT — when a memory is obsolete or \
             contradicted, supersede it instead (memory_supersede); never edit history. A \
             content change re-embeds; a title change re-classifies typed prefixes. Ids come \
             from memory_recall / memory_list output. Targeted repair: pass find + \
             replace_with instead (alone, no title/content) to replace ONE occurrence of \
             stray text in the record body — find must match exactly once (widen it if it \
             repeats). That is the in-tool repair for .coding/knowledge/** text, which the \
             file tools refuse by design.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The memory id to update (from recall/list output)."},
                    "title": {"type": "string", "description": "Optional new title — a typed prefix (SPEC:/DECISION:/BUG:/PLAN:/HOW:) re-classifies the record."},
                    "content": {"type": "string", "description": "Optional new content — re-embeds the memory. For \
                     knowledge-backed records this replaces the truth file's FULL body: pass the complete corrected \
                     body (or full body + a dated amendment paragraph); digest-shaped content (shorter + a \
                     knowledge-path pointer tail) is refused."},
                    "find": {"type": "string", "description": "Targeted repair: the exact text to \
                     replace — it must occur EXACTLY ONCE in the record body (front matter is never \
                     matched). Pass with replace_with, and nothing else."},
                    "replace_with": {"type": "string", "description": "Targeted repair: what find is \
                     replaced with; an empty string deletes it. The result must not leave the body \
                     empty."}
                },
                "required": ["id"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Writes only to the project's own memory store (sandboxed
        // bookkeeping) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemoryUpdateArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let repair = match (&args.find, &args.replace_with) {
            (Some(find), Some(replace_with)) => Some((find.as_str(), replace_with.as_str())),
            (None, None) => None,
            _ => {
                return ToolResult::error(
                    "pass find and replace_with together — find says what to replace, \
                     replace_with says what to put there (an empty replace_with deletes it)",
                )
            }
        };
        if repair.is_some() && (args.title.is_some() || args.content.is_some()) {
            return ToolResult::error(
                "find/replace_with is a targeted repair — pass it alone, with no title or \
                 content: one call, one intent",
            );
        }
        if repair.is_none() && args.title.is_none() && args.content.is_none() {
            return ToolResult::error(
                "nothing to update: pass title and/or content, or find + replace_with",
            );
        }
        let changed: Vec<&str> = [
            args.title.as_ref().map(|_| "title"),
            args.content.as_ref().map(|_| "content"),
            repair.map(|_| "find + replace_with"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let id = args.id;
        // Targeted repair (backlog 488248ce): knowledge-backed ids rewrite the
        // file (the truth), DB-only ids the stored content.
        if let Some((find, replace_with)) = repair {
            return self.repair_by_id(&id, find, replace_with).await;
        }
        // Knowledge-backed update: the id is the deterministic row id of a
        // knowledge record — edit the FILE (the truth) + targeted reindex.
        let rel = match &self.knowledge {
            Some(support) => support.knowledge_for_id(&id).await.map(|kr| kr.rel),
            None => None,
        };
        if let Some(rel) = rel {
            let Some(support) = &self.knowledge else {
                return ToolResult::error(format!(
                    "knowledge record {id} exists but the file backing is not wired"
                ));
            };
            match support
                .knowledge
                .update(&rel, args.title.as_deref(), args.content.as_deref())
            {
                Ok(_) => match support.reindex(&[rel.clone()]).await {
                    Ok(()) => ToolResult::success(format!(
                        "Updated knowledge record {} ({} changed) — file .coding/knowledge/{rel} \
                         rewritten; the memory row re-indexed",
                        id,
                        changed.join(" + ")
                    ))
                    .with_data(
                        json!({ "id": id, "knowledge": format!(".coding/knowledge/{rel}") }),
                    ),
                    Err(e) => ToolResult::error(format!("failed to index knowledge record: {e}")),
                },
                Err(e) => ToolResult::error(format!("failed to update knowledge record: {e}")),
            }
        } else {
            match self
                .store
                .update_memory(&id, args.title.as_deref(), args.content.as_deref())
                .await
            {
                Ok(true) => ToolResult::success(format!(
                    "Updated memory {id} ({} changed)",
                    changed.join(" + ")
                ))
                .with_data(json!({ "id": id })),
                Ok(false) => ToolResult::error(format!(
                    "no memory with id '{id}' — nothing was updated (find ids via \
                     memory_list / memory_recall)"
                )),
                Err(e) => ToolResult::error(format!("failed to update memory: {e}")),
            }
        }
    }
}

/// Arguments for `memory_amend`.
#[derive(Debug, Deserialize)]
struct MemoryAmendArgs {
    id: String,
    paragraph: String,
}

/// The `memory_amend` tool — append a dated amendment paragraph to a
/// knowledge record's file (plan be16ea36 step 7): the sanctioned writer for
/// ADDING information to `.coding/knowledge/**` records. Append-only — the
/// existing body is preserved verbatim, so the row-digest guard can never
/// fire. Requires the knowledge-file backing (registered only when it is
/// wired); refuses unknown ids and superseded records. A leading
/// self-supplied heading ("Amended …:" / "Amendment (…):") inside the
/// paragraph is stripped before the tool's own dated stamp (backlog 488248ce),
/// so exactly one heading ever lands and the tool's date is authoritative.
pub struct MemoryAmendTool {
    support: KnowledgeSupport,
}

impl MemoryAmendTool {
    /// Wire the knowledge-file backing: amendments append to the FILE (the
    /// truth) + a targeted reindex of the derived row.
    pub fn new(
        store: Arc<dyn MemoryStoreTrait>,
        knowledge: Arc<KnowledgeStore>,
        plans_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            support: KnowledgeSupport {
                knowledge,
                store,
                plans_dir: plans_dir.into(),
            },
        }
    }
}

#[async_trait]
impl Tool for MemoryAmendTool {
    fn name(&self) -> &str {
        "memory_amend"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_amend",
            "Append a dated amendment paragraph to a knowledge record's file \
             (.coding/knowledge/**) by id — the sanctioned way to ADD information to a \
             knowledge-backed memory without replacing it. Append-only: the existing body \
             is preserved verbatim, so the digest guard can never fire. A leading \
             'Amended …:' / 'Amendment (…):' heading inside the paragraph is stripped before \
             the tool's own dated stamp — exactly one heading ever lands, dated by the tool \
             (a date you supply is dropped). For corrections or contradictions use \
             memory_update (full body, or its find/replace_with repair mode) or \
             memory_supersede (successor) instead. Refuses unknown ids and superseded \
             records.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The memory id to amend (from recall/list output)."},
                    "paragraph": {"type": "string", "description": "The amendment paragraph — appended to the record's file body under the tool's own dated 'Amended <date>:' heading. A leading heading you supply is stripped; the tool's date is authoritative."}
                },
                "required": ["id", "paragraph"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Writes only to the project's own memory store (sandboxed
        // bookkeeping) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemoryAmendArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let id = args.id;
        // The id is the deterministic row id of a knowledge record — resolve
        // it to the record's file, append the dated paragraph, reindex the
        // row (the file is the truth; the row re-derives from it).
        let record = match self.support.knowledge_for_id(&id).await {
            Some(record) => record,
            None => {
                return ToolResult::error(format!(
                    "no knowledge record with id '{id}' — memory_amend amends \
                     knowledge-backed records only (find ids via memory_list / \
                     memory_recall)"
                ))
            }
        };
        // A self-supplied "Amended …" heading inside the paragraph is stripped
        // by the store (backlog 488248ce) — say so, so the caller knows the
        // text it wrote is not byte-for-byte what landed.
        let (_, stripped) = KnowledgeStore::strip_self_headings(&args.paragraph);
        let note = if stripped > 0 {
            format!(
                "; NOTE: stripped {stripped} self-supplied amendment heading(s) ('Amended …' / \
                 'Amendment (…)') from the paragraph — the tool stamps its own date"
            )
        } else {
            String::new()
        };
        match self.support.knowledge.amend(&record.rel, &args.paragraph) {
            Ok(_) => match self.support.reindex(&[record.rel.clone()]).await {
                Ok(()) => ToolResult::success(format!(
                    "Amended [{}] '{}' (id {id}) — dated paragraph appended to \
                     .coding/knowledge/{}; the memory row re-indexed{note}",
                    record.tier.as_str(),
                    record.title,
                    record.rel
                ))
                .with_data(
                    json!({ "id": id, "knowledge": format!(".coding/knowledge/{}", record.rel) }),
                ),
                Err(e) => ToolResult::error(format!("failed to index knowledge record: {e}")),
            },
            Err(e) => ToolResult::error(format!("failed to amend knowledge record: {e}")),
        }
    }
}

/// Arguments for `memory_supersede`.
#[derive(Debug, Deserialize)]
struct MemorySupersedeArgs {
    id: String,
    tier: String,
    title: String,
    content: String,
}

/// The `memory_supersede` tool — replace a memory with its successor,
/// keeping the old one as recall-excluded history (supersede-not-delete).
pub struct MemorySupersedeTool {
    store: Arc<dyn MemoryStoreTrait>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    /// The knowledge-file backing — `None` keeps the historical DB-only
    /// behavior (and is the test-store path).
    knowledge: Option<KnowledgeSupport>,
}

impl MemorySupersedeTool {
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self {
            store,
            now: Box::new(utc_now_secs),
            knowledge: None,
        }
    }

    /// Wire the knowledge-file backing: superseding a knowledge record then
    /// writes the successor FILE + flips the old file (the truth), followed
    /// by a full index run (only the full scan reconciles an unchanged
    /// predecessor's `superseded_by`).
    pub fn with_knowledge(
        store: Arc<dyn MemoryStoreTrait>,
        knowledge: Arc<KnowledgeStore>,
        plans_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            knowledge: Some(KnowledgeSupport {
                knowledge,
                store: store.clone(),
                plans_dir: plans_dir.into(),
            }),
            store,
            now: Box::new(utc_now_secs),
        }
    }
}

/// F12: reconcile hand-written `File: .coding/knowledge/<…>.md` pointer
/// lines in a successor body against the successor's ACTUAL rel path. The
/// caller cannot know the tool-chosen filename (slug truncation, dedup
/// suffixes), so an assumed pointer can dangle as a self-reference to a
/// file that does not exist. Only leading `File: .coding/knowledge/`
/// pointer lines are touched; the pointer token runs to the first
/// whitespace or parenthetical. Returns the corrected body plus the stale
/// paths that were rewritten.
fn reconcile_file_pointers(body: &str, canonical_rel: &str) -> (String, Vec<String>) {
    let mut stale: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("File: .coding/knowledge/") else {
            out.push(line.to_string());
            continue;
        };
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        let path = &rest[..end];
        if path == canonical_rel {
            out.push(line.to_string());
            continue;
        }
        stale.push(format!(".coding/knowledge/{path}"));
        out.push(line.replacen(
            &format!(".coding/knowledge/{path}"),
            &format!(".coding/knowledge/{canonical_rel}"),
            1,
        ));
    }
    // Preserve the body's line-ending style (project rule) — a CRLF body
    // rejoins with CRLF so a pointer rewrite never silently normalizes the
    // whole file (review 2026-09-17 finding 4).
    let sep = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let mut corrected = out.join(sep);
    if body.ends_with('\n') {
        corrected.push_str(sep);
    }
    (corrected, stale)
}

#[async_trait]
impl Tool for MemorySupersedeTool {
    fn name(&self) -> &str {
        "memory_supersede"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_supersede",
            "MANDATORY when a new decision or fact contradicts or obsoletes a stored one — \
             write the successor here and the old memory becomes history (excluded from \
             recall by default, retrievable via include_superseded); NEVER leave both live. \
             For edits to a still-current fact use memory_update.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The id of the memory being superseded (from recall/list output)."},
                    "tier": {"type": "string", "enum": ["working", "episodic", "semantic", "procedural"], "description": "The successor's tier (same tiers as memory_write)."},
                    "title": {"type": "string", "description": "The successor's title — keep the same typed prefix when the topic is unchanged."},
                    "content": {"type": "string", "description": "The successor's content."}
                },
                "required": ["id", "tier", "title", "content"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Writes only to the project's own memory store (sandboxed
        // bookkeeping) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemorySupersedeArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let tier = match MemoryTier::from_str(&args.tier) {
            Some(t) => t,
            None => return ToolResult::error(format!("unknown tier '{}'", args.tier)),
        };
        let old_id = args.id;
        let title = args.title;
        let tier = tier;
        // Knowledge-backed supersede: the record's FILE is the truth. The
        // successor's tier must be a distilled tier (knowledge records are
        // semantic rows — an episodic/working successor has no file home).
        let knowledge_rel = match &self.knowledge {
            Some(support) => support.knowledge_for_id(&old_id).await.map(|kr| kr.rel),
            None => None,
        };
        if let Some(old_rel) = knowledge_rel {
            if tier != MemoryTier::Semantic {
                return ToolResult::error(format!(
                    "knowledge record {old_id} lives in a file — the successor must be \
                     semantic (the record file carries the truth)"
                ));
            }
            let Some(support) = &self.knowledge else {
                return ToolResult::error(format!(
                    "knowledge record {old_id} exists but the file backing is not wired"
                ));
            };
            match support.knowledge.supersede(&old_rel, &title, &args.content) {
                Ok(new_rel) => {
                    // F12: rewrite hand-written `File:` pointer lines that
                    // assumed a different successor filename than the tool
                    // chose (slug truncation, dedup suffixes) so the body's
                    // self-reference never dangles.
                    let (body, stale_pointers) = reconcile_file_pointers(&args.content, &new_rel);
                    if !stale_pointers.is_empty() {
                        if let Err(e) = support.knowledge.update(&new_rel, None, Some(&body)) {
                            return ToolResult::error(format!(
                                "superseded knowledge record {old_id}, but rewriting its \
                                 {} stale File: pointer line(s) failed: {e} — the canonical \
                                 successor path is .coding/knowledge/{new_rel}",
                                stale_pointers.len()
                            ));
                        }
                    }
                    // The successor FILE flips the unchanged predecessor's
                    // row — only the full scan reconciles that, so run it.
                    support.full_index().await;
                    let new_id = crate::memory::indexer::knowledge_id(&new_rel);
                    let pointer_note = if stale_pointers.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " (rewrote {} stale File: pointer line(s) to the canonical path)",
                            stale_pointers.len()
                        )
                    };
                    let msg = format!(
                        "Superseded knowledge record {old_id} with \"{title}\" — old file \
                         .coding/knowledge/{old_rel} marked superseded (history), successor \
                         at .coding/knowledge/{new_rel}{pointer_note}; memory rows re-indexed"
                    );
                    ToolResult::success(msg).with_data(json!({
                        "id": new_id,
                        "supersedes": old_id,
                        "tier": tier.as_str(),
                        "title": title,
                        "knowledge": format!(".coding/knowledge/{new_rel}"),
                    }))
                }
                Err(e) => ToolResult::error(format!("failed to supersede knowledge record: {e}")),
            }
        } else {
            let memory = Memory::new(tier, title.clone(), args.content, (self.now)());
            match self.store.supersede_memory(&old_id, memory).await {
                Ok(new_id) => {
                    let msg = format!(
                        "Superseded memory {old_id} with \"{title}\" ({} memory) — the old \
                         record stays as history, excluded from recall by default",
                        tier.as_str()
                    );
                    ToolResult::success(msg).with_data(json!({
                        "id": new_id,
                        "supersedes": old_id,
                        "tier": tier.as_str(),
                        "title": title,
                    }))
                }
                Err(e) => ToolResult::error(format!("failed to supersede: {e}")),
            }
        }
    }
}

/// Arguments for `memory_delete`.
#[derive(Debug, Deserialize)]
struct MemoryDeleteArgs {
    id: String,
}

/// The `memory_delete` tool — hard-delete junk/duplicate rows (never stale
/// knowledge — that is superseded, not deleted).
pub struct MemoryDeleteTool {
    store: Arc<dyn MemoryStoreTrait>,
    /// The knowledge-file backing — `None` keeps the historical DB-only
    /// behavior (and is the test-store path).
    knowledge: Option<KnowledgeSupport>,
}

impl MemoryDeleteTool {
    pub fn new(store: Arc<dyn MemoryStoreTrait>) -> Self {
        Self {
            store,
            knowledge: None,
        }
    }

    /// Wire the knowledge-file backing: deleting a knowledge record then
    /// removes the FILE (the truth dies), and the derived row is re-indexed
    /// away.
    pub fn with_knowledge(
        store: Arc<dyn MemoryStoreTrait>,
        knowledge: Arc<KnowledgeStore>,
        plans_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            knowledge: Some(KnowledgeSupport {
                knowledge,
                store: store.clone(),
                plans_dir: plans_dir.into(),
            }),
            store,
        }
    }
}

#[async_trait]
impl Tool for MemoryDeleteTool {
    fn name(&self) -> &str {
        "memory_delete"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "memory_delete",
            "Hard-delete a memory by id — for JUNK or DUPLICATE rows only. Stale or \
             contradicted knowledge is history: supersede it (memory_supersede), NEVER \
             delete it — deletion destroys the audit trail. Errors when the id is unknown.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The memory id to delete (from memory_list / memory_recall output)."}
                },
                "required": ["id"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Deletes only from the project's own memory store (sandboxed
        // bookkeeping) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MemoryDeleteArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Knowledge-backed delete: the id is the deterministic row id of a
        // knowledge record — remove the FILE (the truth dies) + targeted
        // reindex (the derived row is dropped by removal detection).
        let rel = match &self.knowledge {
            Some(support) => support.knowledge_for_id(&args.id).await.map(|kr| kr.rel),
            None => None,
        };
        if let Some(rel) = rel {
            let Some(support) = &self.knowledge else {
                return ToolResult::error(format!(
                    "knowledge record {} exists but the file backing is not wired",
                    args.id
                ));
            };
            match support.knowledge.delete(&rel) {
                Ok(_) => match support.reindex(&[rel.clone()]).await {
                    Ok(()) => ToolResult::success(format!(
                        "Deleted knowledge record {} — file .coding/knowledge/{rel} removed \
                         (the truth is gone); the memory row dropped by reindex",
                        args.id
                    ))
                    .with_data(json!({ "id": args.id })),
                    Err(e) => ToolResult::error(format!("failed to index knowledge record: {e}")),
                },
                Err(e) => ToolResult::error(format!("failed to delete knowledge record: {e}")),
            }
        } else {
            match self.store.delete_memory(&args.id).await {
                Ok(true) => ToolResult::success(format!("Deleted memory {}", args.id))
                    .with_data(json!({ "id": args.id })),
                Ok(false) => ToolResult::error(format!(
                    "no memory with id '{}' — nothing was deleted",
                    args.id
                )),
                Err(e) => ToolResult::error(format!("failed to delete memory: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::MemoryStore;
    use crate::tool::memory::retrieval::MemorySearchTool;
    use tempfile::tempdir;

    fn make_store() -> Arc<dyn MemoryStoreTrait> {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
    }

    #[test]
    fn schema_advertises_the_no_zero_argument_rule() {
        // Backlog d9ad618e (the read_files precedent, backlog 26cdbaf8).
        let tool = MemoryWriteTool::new(make_store());
        let schema = tool.schema();
        assert!(
            schema.description.contains("No zero-argument form"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("do not resend the empty shape"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("e.g. {"),
            "the inline example shows the exact call shape: {}",
            schema.description
        );
    }

    #[tokio::test]
    async fn empty_call_error_carries_the_recovery_hint() {
        // Backlog d9ad618e: the recovery hint rides the error itself, so the
        // FIRST retry succeeds instead of waiting for the circuit breaker.
        let tool = MemoryWriteTool::new(make_store());
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("parameter 'tier' is required"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("rewrite the full call"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("do not resend the empty shape"),
            "{}",
            result.output
        );
    }

    /// Build a knowledge-wired writing tool set over a temp project: the
    /// store, the knowledge dir, and the four writing tools.
    fn make_knowledge_tools(
        dir: &tempfile::TempDir,
        store: Arc<dyn MemoryStoreTrait>,
    ) -> (
        Arc<KnowledgeStore>,
        MemoryWriteTool,
        MemoryUpdateTool,
        MemorySupersedeTool,
        MemoryDeleteTool,
    ) {
        let knowledge = Arc::new(KnowledgeStore::new(dir.path().join(".coding/knowledge")));
        let plans_dir = dir.path().join(".coding/plans");
        let write = MemoryWriteTool::with_knowledge(store.clone(), knowledge.clone(), &plans_dir);
        let update = MemoryUpdateTool::with_knowledge(store.clone(), knowledge.clone(), &plans_dir);
        let supersede =
            MemorySupersedeTool::with_knowledge(store.clone(), knowledge.clone(), &plans_dir);
        let delete = MemoryDeleteTool::with_knowledge(store.clone(), knowledge.clone(), &plans_dir);
        (knowledge, write, update, supersede, delete)
    }

    #[tokio::test]
    async fn lane_agent_write_names_the_main_tree() {
        // A run-all lane agent is rooted in its worktree while the knowledge
        // store stays factory-wide (main tree): the result must say the
        // record landed in the MAIN project tree — the lane's own file tools
        // cannot read the reported path. A main-tree agent (no agent root,
        // or a root equal to the store's root) gets no such note.
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, _update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let lane = write.with_agent_root(Some(dir.path().join(".worktrees/runall-x")));

        let result = lane
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: lane note",
                "content": "The lane note test body."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("landed in the MAIN project tree"),
            "{}",
            result.output
        );

        // Control: a main-tree agent (no agent root) gets no lane note.
        let (_knowledge, write, _update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: main note",
                "content": "The main-tree control body."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("MAIN project tree"),
            "{}",
            result.output
        );

        // Control: an agent root equal to the store's root gets no lane note.
        let (_knowledge, write, _update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let same_root = write.with_agent_root(Some(dir.path().to_path_buf()));
        let result = same_root
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: same root note",
                "content": "The same-root control body."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("MAIN project tree"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn knowledge_wired_write_lands_in_a_file_and_derived_row() {
        // The file-backed write path: a typed write creates the knowledge
        // FILE (the truth) and the derived row comes from reindexing. The
        // reported id is the deterministic knowledge row id.
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, update, supersede, delete) =
            make_knowledge_tools(&dir, store.clone());

        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage engine",
                "content": "We use sqlite for storage. Full rationale here — unbounded body."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("structured data");
        let id = data["id"].as_str().unwrap().to_string();
        assert!(
            result
                .output
                .contains("knowledge record at .coding/knowledge/"),
            "{}",
            result.output
        );
        // The file exists and parses — the front-matter title is BARE (the
        // typed prefix lives only on the digest side; `typed_title()` adds
        // it back, so a stored prefix would double it).
        let rel = data["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/");
        let file = dir.path().join(".coding/knowledge").join(rel);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("We use sqlite for storage"), "{text}");
        assert!(
            text.contains("title = \"storage engine\""),
            "front matter holds the BARE title: {text}"
        );
        // The row is the deterministic knowledge row id, derived class.
        assert_eq!(id, crate::memory::indexer::knowledge_id(rel));
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(m.record_class, MemoryClass::Derived);
        assert_eq!(m.record_type, MemoryRecordType::Decision);
        assert_eq!(
            m.title, "DECISION: storage engine",
            "the digest title is prefixed exactly once"
        );
        assert!(
            m.content.contains("path .coding/knowledge/"),
            "pointer-first digest: {}",
            m.content
        );
        assert!(m.content.chars().count() <= 300, "budgeted digest");
        assert!(
            m.content.contains("We use sqlite for storage"),
            "{}",
            m.content
        );

        // Update via the knowledge-backed tool: the FILE rewrites, the slug
        // stays, the row re-indexes in place.
        let result = update
            .execute(json!({"id": id, "content": "We use sqlite for storage. v2."}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Updated knowledge record"),
            "{}",
            result.output
        );
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert!(m.content.contains("v2."), "{}", m.content);
        // The knowledge writer is unused here — assert the file changed too.
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("v2."), "{text}");

        // Supersede via the knowledge-backed tool: successor FILE + the old
        // file flips to superseded; the rows re-index (full scan).
        let result = supersede
            .execute(json!({
                "id": id,
                "tier": "semantic",
                "title": "DECISION: storage engine",
                "content": "We use postgres for storage. v3.",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.unwrap();
        let new_id = data["id"].as_str().unwrap().to_string();
        assert_ne!(new_id, id, "a new record id");
        let new_rel = data["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/");
        let new_file = dir.path().join(".coding/knowledge").join(new_rel);
        assert!(new_file.exists());
        let successor_text = std::fs::read_to_string(&new_file).unwrap();
        assert!(
            successor_text.contains("title = \"storage engine\""),
            "successor front matter holds the BARE title: {successor_text}"
        );
        let m = store.get_memory(&new_id).await.unwrap().unwrap();
        assert_eq!(
            m.title, "DECISION: storage engine",
            "successor digest title is prefixed exactly once"
        );
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("status = \"superseded\""), "{text}");
        // The old row is excluded from recall (superseded_by set).
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(m.superseded_by.as_deref(), Some(new_id.as_str()));
        let hits = store.recall("v2", &MemoryFilter::new()).await.unwrap();
        assert!(
            hits.iter().all(|sm| sm.memory.id != id),
            "superseded row excluded from recall"
        );

        // F1 regression: memory_update on the SUCCESSOR reindexes just the
        // touched file — its `supersedes` must resolve against the corpus
        // (predecessor context), not error "supersedes '<slug>' not found"
        // and leave the digest stale until startup reconciliation.
        let result = update
            .execute(json!({"id": new_id, "content": "We use postgres for storage. v4."}))
            .await;
        assert!(result.success, "{}", result.output);
        let m = store.get_memory(&new_id).await.unwrap().unwrap();
        assert!(m.content.contains("v4."), "{}", m.content);
        // The predecessor row still points at its successor.
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(m.superseded_by.as_deref(), Some(new_id.as_str()));

        // Delete via the knowledge-backed tool removes the FILE.
        let result = delete.execute(json!({"id": new_id})).await;
        assert!(result.success, "{}", result.output);
        assert!(!new_file.exists(), "the truth file is gone");
    }

    #[tokio::test]
    async fn update_refuses_digest_style_content_that_would_shrink_the_file() {
        // Regression (backlog 63f882c9, incident 2027-01-09): memory_update
        // with digest-style content — strictly shorter than the file's body
        // plus a knowledge-path pointer tail — must REFUSE instead of
        // replacing the truth file's body (a full spec once collapsed to a
        // one-paragraph digest; the detail was recoverable only from git).
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());

        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage engine",
                "content": "We use sqlite for storage.\n\nFull rationale paragraph one — the trade-offs considered, the rejected alternatives, and why the embedded store won.\n\nFull rationale paragraph two — operational notes, the backup story, and the migration path from the previous engine."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("structured data");
        let id = data["id"].as_str().unwrap().to_string();
        let rel = data["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/")
            .to_string();
        let file = dir.path().join(".coding/knowledge").join(&rel);
        let before = std::fs::read_to_string(&file).unwrap();
        let row_before = store.get_memory(&id).await.unwrap().unwrap();

        // Digest-style content: one condensed paragraph, strictly shorter,
        // ending in a self-referential knowledge-path pointer — the
        // row-digest convention leaking into a file-body update.
        let digest = format!(
            "We use sqlite for storage (rationale: simplicity, zero-ops). path .coding/knowledge/{rel}"
        );
        let result = update.execute(json!({"id": id, "content": digest})).await;
        assert!(
            !result.success,
            "digest-shaped replacement must be refused: {}",
            result.output
        );
        assert!(
            result.output.contains("refused"),
            "the refusal teaches the contract: {}",
            result.output
        );
        // The truth file is byte-unchanged — the body was NOT shrunk.
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, before, "the file is untouched by the refusal");
        // The derived row is unchanged too (no reindex on refusal).
        let row_after = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(row_after.content, row_before.content);

        // Control: a shorter replacement WITHOUT a path tail is a
        // legitimate update — the guard catches only the digest shape.
        let result = update
            .execute(json!({
                "id": id,
                "content": "We use sqlite for storage. v2 — revised after review, still a full body, no pointer tail."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("v2 — revised"), "{text}");
    }

    #[test]
    fn reconcile_file_pointers_rewrites_only_stale_leading_pointers() {
        // F12 helper: canonical pointer untouched; a stale LEADING pointer
        // rewritten in place (parenthetical preserved); a lowercase `file:`
        // line is not the digest pointer shape and passes through.
        let body = "Intro line.\nFile: .coding/knowledge/decision/old-name.md (supersedes x).\nFile: .coding/knowledge/decision/canonical.md\nfile: .coding/knowledge/decision/case.md\n";
        let (out, stale) = reconcile_file_pointers(body, "decision/canonical.md");
        assert_eq!(
            stale,
            vec![".coding/knowledge/decision/old-name.md".to_string()]
        );
        assert!(out.starts_with("Intro line.\n"), "{out}");
        assert!(
            out.contains("File: .coding/knowledge/decision/canonical.md (supersedes x)."),
            "stale pointer rewritten in place: {out}"
        );
        assert!(
            out.contains("file: .coding/knowledge/decision/case.md"),
            "non-pointer lines untouched: {out}"
        );
        assert!(!out.contains("old-name"), "{out}");
        // The trailing-newline shape of the source body is preserved.
        assert!(out.ends_with('\n'), "{out}");

        // Review 2026-09-17 finding 4: a CRLF body keeps CRLF — a pointer
        // rewrite must not normalize the whole file's line endings.
        let crlf_body =
            "Intro line.\r\nFile: .coding/knowledge/decision/old-name.md (history).\r\nTail.\r\n";
        let (out, stale) = reconcile_file_pointers(crlf_body, "decision/canonical.md");
        assert_eq!(
            stale,
            vec![".coding/knowledge/decision/old-name.md".to_string()]
        );
        assert!(
            out.contains("File: .coding/knowledge/decision/canonical.md (history).\r\n"),
            "rewritten in place with CRLF preserved: {out:?}"
        );
        assert!(out.ends_with("\r\n"), "trailing CRLF preserved: {out:?}");
        assert!(
            !out.contains("\r\n\n") && !out.contains("\n\r"),
            "no mixed endings introduced: {out:?}"
        );
    }

    #[tokio::test]
    async fn supersede_rewrites_stale_file_pointers_to_the_canonical_path() {
        // F12: a hand-written "File:" pointer in the successor body assumed
        // a filename the caller cannot know (same base slug → the real
        // successor takes a dedup suffix the caller never guessed) — the
        // tool rewrites it to the canonical successor path.
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, _update, supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: pointer hygiene",
                "content": "Original body."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let old_id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        let result = supersede
            .execute(json!({
                "id": old_id,
                "tier": "semantic",
                "title": "DECISION: pointer hygiene",
                "content": "Successor body.\nFile: .coding/knowledge/decision/2026-08-20-wrong-name.md (history pointer)."
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result
                .output
                .contains("rewrote 1 stale File: pointer line(s)"),
            "{}",
            result.output
        );
        let data = result.data.unwrap();
        let new_rel = data["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/")
            .to_string();
        assert!(
            new_rel.contains("-2")
                || !dir
                    .path()
                    .join(".coding/knowledge/decision/2026-08-20-wrong-name.md")
                    .exists(),
            "the canonical rel is the tool-chosen one: {new_rel}"
        );
        let text =
            std::fs::read_to_string(dir.path().join(".coding/knowledge").join(&new_rel)).unwrap();
        assert!(
            text.contains(&format!("File: .coding/knowledge/{new_rel}")),
            "canonical pointer in the successor file: {text}"
        );
        assert!(!text.contains("wrong-name"), "{text}");
        assert!(text.contains("Successor body."), "{text}");
    }

    #[tokio::test]
    async fn knowledge_wired_write_accepts_unbounded_bodies() {
        // Knowledge-as-files (review finding, 2026-08-23): the FILE body is
        // unbounded — the digest budget applies to the derived row, not the
        // tool's content. A >300-char DECISION body (the DB-only budget) must
        // land in the file, with the derived digest still ≤ budget.
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, _update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());

        let long_body = format!("The full rationale, unbounded — {}", "x".repeat(600));
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage engine",
                "content": long_body,
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("structured data");
        let id = data["id"].as_str().unwrap().to_string();
        // The file carries the FULL body; the digest row is budgeted.
        let rel = data["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/");
        let file = dir.path().join(".coding/knowledge").join(rel);
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.matches('x').count(), 600, "full body in the file");
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert!(m.content.chars().count() <= 300, "digest stays budgeted");
    }

    #[tokio::test]
    async fn update_targeted_replace_repairs_a_knowledge_record() {
        // Regression (backlog 488248ce): the in-tool repair path. The file
        // tools refuse .coding/knowledge/** by design, so memory_update must
        // fix stray text — here a doubled amendment heading — in place, with
        // no full-body rewrite (transcription-risky on long records, and the
        // row-digest guard refuses the shrink).
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());

        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "SPEC: storage",
                "content": "the store is sqlite\n\nAmended 2027-01-11: Amended 2027-01-14: stale\n\nfull detail: .coding/knowledge/spec/2027-01-11-spec-storage.md",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("structured data");
        let id = data["id"].as_str().unwrap().to_string();
        let rel = data["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/")
            .to_string();

        let result = update
            .execute(json!({
                "id": id,
                "find": "Amended 2027-01-14: ",
                "replace_with": "",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Repaired knowledge record"),
            "{}",
            result.output
        );

        // The FILE (the truth) collapsed the doubled heading; the rest stayed
        // — including the pointer tail the shrink left in place.
        let text =
            std::fs::read_to_string(dir.path().join(".coding/knowledge").join(&rel)).unwrap();
        assert!(text.contains("Amended 2027-01-11: stale"), "{text}");
        assert!(!text.contains("Amended 2027-01-14:"), "{text}");
        assert!(text.contains("the store is sqlite"), "{text}");
        assert!(
            text.contains("full detail: .coding/knowledge/spec/2027-01-11-spec-storage.md"),
            "{text}"
        );

        // The derived row re-indexed from the repaired file (its content is a
        // first-line digest by design — assert the pointer, as amend's test
        // does).
        let row = store
            .get_memory(&id)
            .await
            .unwrap()
            .expect("repaired row exists");
        assert_eq!(
            row.data.get("rel_path").and_then(|v| v.as_str()),
            Some(rel.as_str())
        );
    }

    #[tokio::test]
    async fn update_targeted_replace_argument_rules() {
        let dir = tempdir().unwrap();
        let store = make_store();
        let (_knowledge, write, update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "SPEC: storage",
                "content": "the store is sqlite and sqlite is fast",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        // find without replace_with, and replace_with without find.
        for args in [
            json!({"id": id, "find": "sqlite"}),
            json!({"id": id, "replace_with": "postgres"}),
        ] {
            let result = update.execute(args).await;
            assert!(!result.success);
            assert!(
                result.output.contains("pass find and replace_with together"),
                "{}",
                result.output
            );
        }
        // A targeted repair is one call, one intent — no title/content.
        let result = update
            .execute(json!({
                "id": id,
                "title": "SPEC: x",
                "find": "sqlite",
                "replace_with": "postgres",
            }))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("pass it alone"), "{}", result.output);
        // Nothing at all still errors.
        let result = update.execute(json!({"id": id})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("nothing to update"),
            "{}",
            result.output
        );
        // An ambiguous find names the count and changes nothing.
        let result = update
            .execute(json!({"id": id, "find": "sqlite", "replace_with": "postgres"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("occurs 2 times"),
            "{}",
            result.output
        );
        // An absent find says so.
        let result = update
            .execute(json!({"id": id, "find": "mysql", "replace_with": "postgres"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("not present"), "{}", result.output);
        // Unknown id (no knowledge record, no DB row).
        let result = update
            .execute(json!({"id": "nope", "find": "a", "replace_with": "b"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("no memory with id 'nope'"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn update_targeted_replace_works_on_db_only_records() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let update = MemoryUpdateTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "auth fact",
                "content": "we use jose for JWT",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        // The same repair mode against a row with no file backing.
        let result = update
            .execute(json!({"id": id, "find": "jose", "replace_with": "jsonwebtoken"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Repaired memory"),
            "{}",
            result.output
        );
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(m.content, "we use jsonwebtoken for JWT");

        // A repair that would empty the body is refused — nothing is written.
        let result = update
            .execute(json!({
                "id": id,
                "find": "we use jsonwebtoken for JWT",
                "replace_with": "",
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("would empty the record body"),
            "{}",
            result.output
        );
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(m.content, "we use jsonwebtoken for JWT");
    }

    #[tokio::test]
    async fn db_only_typed_write_accepts_long_content() {
        // With the hard budget reject removed, a DB-only (no knowledge backing)
        // typed write accepts unbounded content — the file is the truth, and
        // shortness is encouraged by the prompt + the indexer's auto-truncation,
        // not enforced by a hard reject.
        let store = make_store();
        let tool = MemoryWriteTool::new(store.clone());
        let result = tool
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage engine",
                "content": "y".repeat(600),
            }))
            .await;
        assert!(
            result.success,
            "DB-only typed write accepts long content: {}",
            result.output
        );
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();
        let m = store.get_memory(&id).await.unwrap().unwrap();
        assert_eq!(m.content.chars().count(), 600, "full content stored");
    }

    #[tokio::test]
    async fn recall_lines_carry_stable_ids_first() {
        // Phase 1: the memory id is the FIRST parenthesized field of every
        // recall line, so existing score/strength parsing keeps working and
        // the agent can address memories (memory_update / memory_supersede /
        // memory_delete) straight from recall output.
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let recall = MemorySearchTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "auth fact",
                "content": "this project uses jose for JWT"
            }))
            .await;
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        let result = recall.execute(json!({"query": "JWT auth"})).await;
        assert!(result.success, "{}", result.output);
        let marker = format!("[semantic] auth fact (id: {id}, score:");
        assert!(
            result.output.contains(&marker),
            "recall line carries the stable id first: {}",
            result.output
        );
    }

    /// Backlog 2026-08-20 regression: "wrote semantic memory (id: …) is a
    /// meaningless message". The confirmation must be a human sentence naming
    /// WHAT was saved (the title) + the tier, and must NOT leak the write id
    /// into the human text (the id rides in structured `data` instead).
    #[tokio::test]
    async fn write_message_is_human_readable() {
        let store = make_store();
        let tool = MemoryWriteTool::new(store);
        let result = tool
            .execute(json!({
                "tier": "semantic",
                "title": "auth fact",
                "content": "this project uses jose for JWT"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Saved a durable fact \"auth fact\""),
            "message should name what was saved: {}",
            result.output
        );
        assert!(
            result.output.contains("(semantic memory)"),
            "message should name the tier: {}",
            result.output
        );
        assert!(
            !result.output.contains("(id:"),
            "the write id must not leak into the human message: {}",
            result.output
        );
        // The id survives machine-readably in structured data.
        let data = result.data.expect("structured data carries the id");
        assert!(data["id"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(data["tier"], "semantic");
        assert_eq!(data["title"], "auth fact");
    }

    /// Every tier gets its own human phrase (+ its tier word), and none of
    /// them leak the id.
    #[tokio::test]
    async fn write_message_covers_every_tier() {
        let store = make_store();
        let tool = MemoryWriteTool::new(store);
        for (tier, phrase) in [
            ("working", "a session event"),
            ("episodic", "a session summary"),
            ("semantic", "a durable fact"),
            ("procedural", "a reusable workflow"),
        ] {
            let result = tool
                .execute(json!({"tier": tier, "title": "T", "content": "C"}))
                .await;
            assert!(result.success, "{tier}: {}", result.output);
            assert!(
                result.output.contains(&format!("Saved {phrase} \"T\"")),
                "{tier}: {}",
                result.output
            );
            assert!(
                result.output.contains(&format!("({tier} memory)")),
                "{tier}: {}",
                result.output
            );
            assert!(
                !result.output.contains("(id:"),
                "{tier}: id leaked: {}",
                result.output
            );
        }
    }

    #[tokio::test]
    async fn write_unknown_tier_errors() {
        let store = make_store();
        let tool = MemoryWriteTool::new(store);
        let result = tool
            .execute(json!({"tier": "bogus", "title": "t", "content": "c"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("unknown tier"));
    }

    #[tokio::test]
    async fn recall_no_matches() {
        let store = make_store();
        let tool = MemorySearchTool::new(store);
        let result = tool.execute(json!({"query": "nothing here"})).await;
        assert!(result.success);
        assert!(result.output.contains("no memories"));
    }

    #[tokio::test]
    async fn recall_with_tier_filter() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        write
            .execute(json!({"tier": "working", "title": "w", "content": "jwt auth content"}))
            .await;
        write
            .execute(json!({"tier": "semantic", "title": "s", "content": "jwt auth fact"}))
            .await;
        let recall = MemorySearchTool::new(store);
        let result = recall
            .execute(json!({"query": "jwt", "tier": "semantic"}))
            .await;
        assert!(result.success);
        assert!(result.output.contains("[semantic]"));
        assert!(!result.output.contains("[working]"));
    }

    #[tokio::test]
    async fn consolidate_empty_session() {
        let store = make_store();
        let tool = MemoryConsolidateTool::new(store);
        let result = tool.execute(json!({"session_id": "empty"})).await;
        assert!(result.success);
        assert!(result.output.contains("no working-memory events"));
    }

    #[tokio::test]
    async fn consolidate_with_provider_runs_full_extraction() {
        // When a swappable provider is wired in, the tool must pass it to
        // consolidate_session so the full pipeline (semantic + procedural
        // extraction) runs — not just synthetic episodic compression. We
        // assert the success message distinguishes the two paths.
        let store = make_store();
        // Seed a working-tier event so consolidation has input.
        store
            .record_tool_event(
                Some("sess-prov"),
                "file_edit",
                json!({"path": "src/lib.rs"}),
                "edited",
                None,
            )
            .await
            .unwrap();
        // A provider slot with no client configured: the tool snapshots None
        // and falls back to synthetic (the message must NOT claim full
        // extraction).
        let slot = SwappableProvider::new(None);
        let tool = MemoryConsolidateTool::with_provider(store.clone(), slot, "");
        let result = tool.execute(json!({"session_id": "sess-prov"})).await;
        assert!(result.success);
        assert!(
            result.output.contains("episodic memory"),
            "no-provider path should say episodic only, got: {}",
            result.output
        );
        assert!(
            !result.output.contains("semantic/procedural"),
            "no-provider path must not claim full extraction, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn consolidate_without_provider_is_synthetic_only() {
        // The back-compat constructor (no provider) must still work and report
        // the synthetic-only path.
        let store = make_store();
        store
            .record_tool_event(
                Some("sess-syn"),
                "file_write",
                json!({"path": "a.rs"}),
                "wrote",
                None,
            )
            .await
            .unwrap();
        let tool = MemoryConsolidateTool::new(store);
        let result = tool.execute(json!({"session_id": "sess-syn"})).await;
        assert!(result.success);
        assert!(
            result.output.contains("episodic memory"),
            "should report episodic-only, got: {}",
            result.output
        );
        assert!(
            !result.output.contains("semantic/procedural"),
            "must not claim full extraction, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn recall_annotates_superseded_entries() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage",
                "content": "we use postgres for storage"
            }))
            .await;
        let old_id = result.data.unwrap()["id"].as_str().unwrap().to_string();
        // Supersede at store level — the annotation keys off superseded_by
        // regardless of how the row got superseded (the memory_supersede
        // tool's own path is covered by its dedicated tests below).
        store
            .supersede_memory(
                &old_id,
                Memory::new(
                    MemoryTier::Semantic,
                    "DECISION: storage",
                    "we use sqlite for storage",
                    1001,
                ),
            )
            .await
            .unwrap();

        let recall = MemorySearchTool::new(store);
        // Default: the superseded row is invisible — no annotation, no
        // stale content.
        let result = recall.execute(json!({"query": "storage"})).await;
        assert!(result.success);
        assert!(!result.output.contains("[superseded]"), "{}", result.output);
        assert!(!result.output.contains("postgres"), "{}", result.output);
        assert!(result.output.contains("sqlite"), "{}", result.output);

        // Opt into history: the old row resurfaces, annotated.
        let result = recall
            .execute(json!({"query": "storage", "include_superseded": true}))
            .await;
        assert!(result.success);
        assert!(result.output.contains("postgres"), "{}", result.output);
        assert!(result.output.contains("[superseded]"), "{}", result.output);
    }

    #[tokio::test]
    async fn typed_write_keeps_confirmation_and_data_shape() {
        // Frontend parser compat: a typed (budgeted) write returns the same
        // confirmation sentence + structured data shape as an untyped one.
        let store = make_store();
        let tool = MemoryWriteTool::new(store);
        let result = tool
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage",
                "content": "use sqlite",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result
                .output
                .contains("Saved a durable fact \"DECISION: storage\""),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("(semantic memory)"),
            "{}",
            result.output
        );
        let data = result.data.expect("structured data carries the id");
        assert!(data["id"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(data["tier"], "semantic");
        assert_eq!(data["title"], "DECISION: storage");
    }

    // ── Hygiene tools: update / supersede / delete / list ────────────────

    #[tokio::test]
    async fn update_changes_content_and_title_and_reclassifies() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let update = MemoryUpdateTool::new(store.clone());
        let recall = MemorySearchTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "auth fact",
                "content": "we use jose for JWT",
            }))
            .await;
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        // Refine both fields in one call.
        let result = update
            .execute(json!({
                "id": id,
                "title": "DECISION: jwt lib",
                "content": "we use jsonwebtoken for JWT",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains(&format!("Updated memory {id}")),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("title + content"),
            "{}",
            result.output
        );
        assert_eq!(result.data.unwrap()["id"], id);

        // Recall sees the NEW text, not the old.
        let result = recall.execute(json!({"query": "jwt"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("jsonwebtoken"), "{}", result.output);
        assert!(!result.output.contains("jose"), "{}", result.output);

        // The title change re-classified the record (DECISION: prefix).
        let list = MemorySearchTool::new(store);
        let result = list.execute(json!({"record_type": "decision"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("DECISION: jwt lib"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn update_unknown_id_errors() {
        let store = make_store();
        let tool = MemoryUpdateTool::new(store);
        let result = tool.execute(json!({"id": "nope", "content": "x"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("no memory with id 'nope'"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn update_with_nothing_to_change_errors() {
        let store = make_store();
        let tool = MemoryUpdateTool::new(store);
        let result = tool.execute(json!({"id": "any"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("nothing to update"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn amend_appends_dated_paragraph_and_reindexes() {
        let dir = tempdir().unwrap();
        let store = make_store();
        let (knowledge, write, _update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let plans_dir = dir.path().join(".coding/plans");
        let amend = MemoryAmendTool::new(store.clone(), knowledge.clone(), &plans_dir);

        // Write a knowledge-backed record.
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "SPEC: storage",
                "content": "the store is sqlite",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        // Amend it: a dated paragraph is appended, the original body intact.
        let result = amend
            .execute(json!({
                "id": id,
                "paragraph": "the store is sqlite with WAL",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // Self-describing result (backlog d779060a): the output names the
        // amended record's tier + title, not just the id.
        assert!(
            result.output.contains("Amended [semantic] 'SPEC: storage'"),
            "output must name the amended record (tier + title), got: {}",
            result.output
        );

        // The FILE carries the amendment (the truth), original body intact.
        // The reported path is relative to the project root.
        let reported = result.data.unwrap()["knowledge"]
            .as_str()
            .unwrap()
            .to_string();
        let text = std::fs::read_to_string(dir.path().join(&reported)).unwrap();
        assert!(text.contains("the store is sqlite\n"), "{text}");
        assert!(text.contains("Amended "), "{text}");
        assert!(text.contains("the store is sqlite with WAL"), "{text}");

        // Recall still finds the record, and the row re-indexed: still
        // present, still pointing at the amended file. (The row's content is
        // a first-line digest by design — the pointer-first indexer keeps
        // the gist stable; the FILE carries the amendment, asserted above.)
        let recall = MemorySearchTool::new(store.clone());
        let result = recall.execute(json!({"query": "storage"})).await;
        assert!(result.success, "{}", result.output);
        let rel = reported
            .strip_prefix(".coding/knowledge/")
            .unwrap()
            .to_string();
        let row = store
            .get_memory(&id)
            .await
            .unwrap()
            .expect("amended row exists");
        assert_eq!(
            row.data.get("rel_path").and_then(|v| v.as_str()),
            Some(rel.as_str())
        );
    }

    #[tokio::test]
    async fn amend_result_reports_a_stripped_self_heading() {
        // Backlog 488248ce: the store strips a self-supplied heading, and the
        // result says so — a silent strip would be a silent content change
        // (the caller's date is dropped for the tool's stamp).
        let dir = tempdir().unwrap();
        let store = make_store();
        let (knowledge, write, _update, _supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let plans_dir = dir.path().join(".coding/plans");
        let amend = MemoryAmendTool::new(store.clone(), knowledge, &plans_dir);

        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "SPEC: steering",
                "content": "the store is sqlite",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        let result = amend
            .execute(json!({
                "id": id,
                "paragraph": "Amended 2027-01-14: the tails are imperatives",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("NOTE: stripped 1 self-supplied"),
            "{}",
            result.output
        );
        // Exactly one heading landed, and the caller's date is gone (the tool
        // stamps its own — asserted date-independently).
        let rel = result.data.unwrap()["knowledge"]
            .as_str()
            .unwrap()
            .trim_start_matches(".coding/knowledge/")
            .to_string();
        let text =
            std::fs::read_to_string(dir.path().join(".coding/knowledge").join(&rel)).unwrap();
        assert_eq!(text.matches("Amended ").count(), 1, "{text}");
        assert!(text.contains(": the tails are imperatives"), "{text}");
        assert!(!text.contains("Amended 2027-01-14"), "{text}");

        // A plain paragraph reports no strip.
        let result = amend
            .execute(json!({"id": id, "paragraph": "a plain paragraph"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("NOTE: stripped"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn amend_refuses_unknown_id_and_superseded_records() {
        let dir = tempdir().unwrap();
        let store = make_store();
        let (knowledge, write, _update, supersede, _delete) =
            make_knowledge_tools(&dir, store.clone());
        let plans_dir = dir.path().join(".coding/plans");
        let amend = MemoryAmendTool::new(store, knowledge, &plans_dir);

        // Unknown id.
        let result = amend.execute(json!({"id": "nope", "paragraph": "x"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("no knowledge record with id 'nope'"),
            "{}",
            result.output
        );

        // A superseded record is history — refused; amend the successor.
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "SPEC: old",
                "content": "old body",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();
        let result = supersede
            .execute(json!({
                "id": id,
                "tier": "semantic",
                "title": "SPEC: new",
                "content": "new body",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let result = amend.execute(json!({"id": id, "paragraph": "x"})).await;
        assert!(!result.success);
        assert!(result.output.contains("superseded"), "{}", result.output);
    }

    #[tokio::test]
    async fn supersede_hides_old_and_returns_successor() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let supersede = MemorySupersedeTool::new(store.clone());
        let recall = MemorySearchTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: storage",
                "content": "we use postgres for storage",
            }))
            .await;
        let old_id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        // Supersede with the successor record.
        let result = supersede
            .execute(json!({
                "id": old_id,
                "tier": "semantic",
                "title": "DECISION: storage",
                "content": "we use sqlite for storage",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result
                .output
                .contains(&format!("Superseded memory {old_id}")),
            "{}",
            result.output
        );
        let data = result.data.unwrap();
        let new_id = data["id"].as_str().unwrap().to_string();
        assert_ne!(new_id, old_id);
        assert_eq!(data["supersedes"], old_id);
        assert_eq!(data["tier"], "semantic");

        // Default recall: only the successor, never the stale fact.
        let result = recall.execute(json!({"query": "storage"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("sqlite"), "{}", result.output);
        assert!(!result.output.contains("postgres"), "{}", result.output);
        assert!(!result.output.contains("[superseded]"), "{}", result.output);

        // Opted-in recall: the old row resurfaces, annotated.
        let result = recall
            .execute(json!({"query": "storage", "include_superseded": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("postgres"), "{}", result.output);
        assert!(result.output.contains("[superseded]"), "{}", result.output);
    }

    #[tokio::test]
    async fn supersede_unknown_id_errors() {
        let store = make_store();
        let tool = MemorySupersedeTool::new(store);
        let result = tool
            .execute(json!({
                "id": "missing",
                "tier": "semantic",
                "title": "DECISION: x",
                "content": "y",
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("no memory with id 'missing'"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn supersede_twice_errors_naming_existing_superseder() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let supersede = MemorySupersedeTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: cfg",
                "content": "v1",
            }))
            .await;
        let old_id = result.data.unwrap()["id"].as_str().unwrap().to_string();
        let result = supersede
            .execute(json!({
                "id": old_id,
                "tier": "semantic",
                "title": "DECISION: cfg",
                "content": "v2",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // A second supersede must fail and name the already-live successor.
        let result = supersede
            .execute(json!({
                "id": old_id,
                "tier": "semantic",
                "title": "DECISION: cfg",
                "content": "v3",
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("already superseded"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn delete_removes_memory_and_unknown_id_errors() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let delete = MemoryDeleteTool::new(store.clone());
        let recall = MemorySearchTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "junk fact",
                "content": "temporary junk to delete",
            }))
            .await;
        let id = result.data.unwrap()["id"].as_str().unwrap().to_string();

        let result = delete.execute(json!({"id": id})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains(&format!("Deleted memory {id}")),
            "{}",
            result.output
        );
        assert_eq!(result.data.unwrap()["id"], id);

        // Gone from recall; deleting again errors (nothing was deleted).
        let result = recall.execute(json!({"query": "junk"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("no memories"), "{}", result.output);
        let result = delete.execute(json!({"id": id})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("no memory with id"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn list_output_shape_and_filters() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let list = MemorySearchTool::new(store.clone());
        // Two typed records + one working-tier event (excluded by default).
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: db",
                "content": "sqlite",
            }))
            .await;
        let decision_id = result.data.unwrap()["id"].as_str().unwrap().to_string();
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "BUG: login crash",
                "content": "panic in auth",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let result = write
            .execute(json!({
                "tier": "working",
                "title": "raw event",
                "content": "edited a file",
            }))
            .await;
        assert!(result.success, "{}", result.output);

        // Unfiltered: one line per memory (id, tier, record_type, date,
        // title), newest first, working excluded.
        let result = list.execute(json!({})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("2 memories"), "{}", result.output);
        for line in result.output.lines() {
            if line.contains("DECISION: db") {
                assert!(
                    line.starts_with(&format!("{decision_id}  semantic  decision  ")),
                    "line shape: {line}"
                );
            }
        }
        assert!(
            result.output.contains("BUG: login crash"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("raw event"), "{}", result.output);

        // record_type filter narrows to the decision.
        let result = list.execute(json!({"record_type": "decision"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("1 memories"), "{}", result.output);
        assert!(result.output.contains("DECISION: db"), "{}", result.output);
        assert!(
            !result.output.contains("BUG: login crash"),
            "{}",
            result.output
        );

        // prefix filter browses a title namespace.
        let result = list.execute(json!({"prefix": "BUG:"})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("BUG: login crash"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("DECISION: db"), "{}", result.output);

        // tier filter, plus a working tier opt-in.
        let result = list.execute(json!({"tier": "working"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("raw event"), "{}", result.output);
        assert!(!result.output.contains("DECISION: db"), "{}", result.output);

        // limit bounds the browse.
        let result = list.execute(json!({"limit": 1})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("1 memories"), "{}", result.output);
    }

    #[tokio::test]
    async fn list_annotates_superseded_history_rows() {
        let store = make_store();
        let write = MemoryWriteTool::new(store.clone());
        let list = MemorySearchTool::new(store.clone());
        let result = write
            .execute(json!({
                "tier": "semantic",
                "title": "DECISION: engine",
                "content": "v1",
            }))
            .await;
        let old_id = result.data.unwrap()["id"].as_str().unwrap().to_string();
        store
            .supersede_memory(
                &old_id,
                Memory::new(MemoryTier::Semantic, "DECISION: engine", "v2", 1001),
            )
            .await
            .unwrap();

        // Default: history hidden.
        let result = list.execute(json!({})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("1 memories"), "{}", result.output);
        assert!(!result.output.contains("[superseded]"), "{}", result.output);

        // Opted in: the old row resurfaces, annotated.
        let result = list.execute(json!({"include_superseded": true})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("2 memories"), "{}", result.output);
        assert!(
            result
                .output
                .contains(&format!("{old_id}  semantic  decision")),
            "{}",
            result.output
        );
        assert!(result.output.contains("[superseded]"), "{}", result.output);
    }

    #[tokio::test]
    async fn list_unknown_record_type_errors() {
        let store = make_store();
        let tool = MemorySearchTool::new(store);
        let result = tool.execute(json!({"record_type": "bogus"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("unknown record_type 'bogus'"),
            "{}",
            result.output
        );
    }
}

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Consolidation pipeline — working → episodic → semantic → procedural.
//!
//! At session end (when an LLM is available), the pipeline runs:
//! 1. Working → Episodic: compress the session's raw events into a summary.
//! 2. Episodic → Semantic: extract cross-session facts.
//! 3. Semantic → Procedural: detect recurring workflows.
//!
//! Without an LLM, the pipeline stops at episodic (synthetic compression).

use std::path::Path;

use crate::error::Result;
use crate::memory::types::{EpisodicData, Memory, MemoryData, MemoryTier};
use crate::memory::MemoryStoreTrait;
use crate::provider::{LlmClient, Message};

/// How many files are read from each of the plans + reviews dirs when
/// building a corpus digest (newest first).
const CORPUS_MAX_FILES_PER_DIR: usize = 10;

/// How many chars are read from each corpus file (plans and reviews start
/// with their most decision-dense content; the tail is usually step
/// checklists).
const CORPUS_FILE_CHAR_CAP: usize = 1500;

/// Total char cap for the assembled corpus digest, keeping the extraction
/// prompts bounded.
const CORPUS_TOTAL_CHAR_CAP: usize = 30_000;

/// How many existing semantic facts are listed in the extraction prompt
/// (newest by `last_accessed_at` first) so the LLM can update in place
/// instead of duplicating.
const EXISTING_FACTS_LIMIT: usize = 200;

/// Char cap per existing-fact line in the extraction prompt.
const EXISTING_FACT_CHAR_CAP: usize = 200;

/// Per-tool-output char cap in the synthesis prompt (raised from 200: error
/// messages and decision summaries carry signal beyond the first line).
const TOOL_OUTPUT_CHAR_CAP: usize = 800;

/// Total char cap for the synthesis prompt's tool-event listing.
const EVENTS_TEXT_CHAR_CAP: usize = 24_000;

/// Build a digest of the project's accumulated plans and review reports —
/// the richest learning material the agent produces (goals, locked
/// decisions, step recipes, reviewer findings with file/line refs).
///
/// Walks both dirs for `*.md` files, newest-first by mtime, taking up to
/// [`CORPUS_MAX_FILES_PER_DIR`] files per dir and the first
/// [`CORPUS_FILE_CHAR_CAP`] chars of each, under a total
/// [`CORPUS_TOTAL_CHAR_CAP`] (a `[...truncated...]` marker is appended when
/// the cap cuts the digest short). Sections are headed
/// `=== PLAN/REVIEW: {filename} ===` so downstream prompts can cite them.
/// Returns an empty string when BOTH dirs are missing/empty (one present dir
/// still contributes its files) — callers treat that as "no corpus".
pub fn corpus_digest(plans_dir: &Path, reviews_dir: &Path) -> String {
    let mut out = String::new();
    for (dir, label) in [(plans_dir, "PLAN"), (reviews_dir, "REVIEW")] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        // (mtime, path) pairs so the newest files win the per-dir cap.
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter_map(|e| {
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((mtime, e.path()))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, path) in files.into_iter().take(CORPUS_MAX_FILES_PER_DIR) {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let capped: String = content.chars().take(CORPUS_FILE_CHAR_CAP).collect();
            let section = format!("=== {label}: {name} ===\n{capped}\n\n");
            if out.chars().count() + section.chars().count() > CORPUS_TOTAL_CHAR_CAP {
                out.push_str("[...truncated...]\n");
                return out;
            }
            out.push_str(&section);
        }
    }
    out
}

/// Consolidate a session: generate an episodic summary from working memory,
/// then (if an LLM is available) extract semantic facts + procedural workflows.
///
/// `corpus` is a [`corpus_digest`] of the project's plans + review reports.
/// When non-empty it is appended to the extraction prompts so distillation
/// can merge this session's knowledge with what earlier plans/reviews
/// established instead of re-deriving duplicates.
///
/// This is a free function taking `&dyn MemoryStoreTrait` so it works for any
/// store implementation (including mocks).
pub async fn consolidate_session(
    store: &dyn MemoryStoreTrait,
    session_id: &str,
    provider: Option<&dyn LlmClient>,
    corpus: &str,
) -> Result<String> {
    // 1. Gather this session's working-memory events.
    let working = store.list_by_tier(MemoryTier::Working).await?;
    let session_events: Vec<&Memory> = working
        .iter()
        .filter(|m| m.source_session_ids.iter().any(|s| s == session_id))
        .collect();

    consolidate_session_with_events(store, session_id, &session_events, provider, corpus).await
}

/// The consolidation core, fed a pre-filtered event list.
///
/// [`consolidate_session`] loads the working tier itself; callers that
/// already hold a snapshot (e.g. the manual cleanup in
/// `memory::maintenance`, which loads it once for all sessions) pass the
/// per-session slice here so the full tier is never re-read per session.
///
/// Returns an empty id when `events` is empty (nothing to consolidate) and
/// otherwise runs the full pipeline — episodic summary, semantic/procedural
/// extraction when an LLM is available, and the raw-event deletion.
pub(crate) async fn consolidate_session_with_events(
    store: &dyn MemoryStoreTrait,
    session_id: &str,
    events: &[&Memory],
    provider: Option<&dyn LlmClient>,
    corpus: &str,
) -> Result<String> {
    if events.is_empty() {
        return Ok(String::new());
    }
    let session_events = events;

    // 2. Generate the episodic summary (synthetic or LLM-assisted).
    let (narrative, key_decisions, files_modified, concepts) = if let Some(llm) = provider {
        synthesize_with_llm(&session_events, llm)
            .await
            .unwrap_or_else(|_| synthesize_synthetic(&session_events))
    } else {
        synthesize_synthetic(&session_events)
    };

    // 3. Store the episodic memory.
    let now = session_events
        .iter()
        .map(|m| m.last_accessed_at)
        .max()
        .unwrap_or(0);
    let data = EpisodicData {
        narrative: narrative.clone(),
        key_decisions: key_decisions.clone(),
        files_modified: files_modified.clone(),
        concepts: concepts.clone(),
    };
    let mut episodic = Memory::new(
        MemoryTier::Episodic,
        format!("session {session_id} summary"),
        data.narrative.clone(),
        now,
    );
    episodic.data = serde_json::to_value(&data).unwrap_or(serde_json::Value::Null);
    episodic.source_session_ids = vec![session_id.to_string()];
    let id = store.write(episodic).await?;

    // 4. If we have an LLM, extract semantic facts + procedural workflows.
    //    The corpus digest is appended so extraction can merge with (rather
    //    than duplicate) knowledge established by earlier plans/reviews.
    //    `now` (the session's latest event time) stamps extracted memories so
    //    freshly distilled facts rank by recency instead of decaying from 0.
    if let Some(llm) = provider {
        // Fire-and-forget stays (a failed extraction must not fail the
        // consolidation — the episodic summary is already durable), but a
        // failed distillation must not vanish silently — log it (quality
        // review LOW 6, class-closure).
        if let Err(e) =
            extract_semantic_facts(store, &narrative, session_id, now, corpus, llm).await
        {
            eprintln!("mnemo: semantic fact extraction failed: {e}");
        }
        if let Err(e) =
            extract_procedural_workflows(store, &narrative, session_id, now, corpus, llm).await
        {
            eprintln!("mnemo: procedural workflow extraction failed: {e}");
        }
    }

    // 5. Clean up the raw working-memory events for this session. They have
    // now been compressed into the episodic summary (and any extracted
    // semantic/procedural memories), so keeping them would let working memory
    // grow unboundedly across sessions — slowing every recall. We delete only
    // working-tier events tagged with this session; the episodic/semantic/
    // procedural outputs are preserved. Fire-and-forget stays (the episodic
    // summary is already written — a failed cleanup must not fail the
    // consolidation), but a silently growing working tier is its own
    // failure — log it (quality review LOW 6, class-closure).
    if let Err(e) = store.delete_working_for_session(session_id).await {
        eprintln!("mnemo: failed to delete working events for session {session_id}: {e}");
    }

    Ok(id)
}

/// Synthesize an episodic summary without an LLM (synthetic compression).
fn synthesize_synthetic(events: &[&Memory]) -> (String, Vec<String>, Vec<String>, Vec<String>) {
    let tool_count = events.len();
    let tools_used: Vec<String> = events
        .iter()
        .filter_map(|m| {
            let data: MemoryData = serde_json::from_value(m.data.clone()).ok()?;
            match data {
                MemoryData::Working(w) => Some(w.tool_name),
                _ => None,
            }
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let files_modified: Vec<String> = events
        .iter()
        .filter_map(|m| {
            let data: MemoryData = serde_json::from_value(m.data.clone()).ok()?;
            match data {
                MemoryData::Working(w) => {
                    if w.tool_name.contains("write") || w.tool_name.contains("edit") {
                        w.tool_input
                            .get("path")
                            .and_then(|p| p.as_str())
                            .map(String::from)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let files_modified_str = if files_modified.is_empty() {
        "none".to_string()
    } else {
        files_modified.join(", ")
    };

    let narrative = format!(
        "Session involved {} tool calls across tools: {}. Files modified: {}.",
        tool_count,
        tools_used.join(", "),
        files_modified_str
    );

    let concepts = tools_used.clone();
    (narrative, Vec::new(), files_modified, concepts)
}

/// Synthesize an episodic summary with an LLM for richer narrative.
async fn synthesize_with_llm(
    events: &[&Memory],
    llm: &dyn LlmClient,
) -> Result<(String, Vec<String>, Vec<String>, Vec<String>)> {
    let events_text = events
        .iter()
        .filter_map(|m| {
            let data: MemoryData = serde_json::from_value(m.data.clone()).ok()?;
            match data {
                MemoryData::Working(w) => Some(format!(
                    "- {}({}): {}{}",
                    w.tool_name,
                    w.tool_input,
                    w.tool_output
                        .chars()
                        .take(TOOL_OUTPUT_CHAR_CAP)
                        .collect::<String>(),
                    w.error
                        .map(|e| format!(" [ERROR: {e}]"))
                        .unwrap_or_default()
                )),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Cap the total listing so a long session can't blow up the prompt.
    let events_text: String = events_text.chars().take(EVENTS_TEXT_CHAR_CAP).collect();

    let prompt = format!(
        "Summarize this coding session as JSON with fields: narrative (string), key_decisions (array of strings), files_modified (array of strings), concepts (array of strings).\n\nTool events:\n{events_text}"
    );

    let messages = vec![Message::user_text(prompt)];

    let stream = llm.complete(&messages, &[], None).await?;
    use futures::StreamExt;
    let mut text = String::new();
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        match event {
            crate::provider::LlmEvent::TextDelta { text: t } => text.push_str(&t),
            crate::provider::LlmEvent::Error { .. } => break,
            _ => {}
        }
    }

    // Try to parse the JSON response.
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or(serde_json::json!({"narrative": text, "key_decisions": [], "files_modified": [], "concepts": []}));
    let narrative = json
        .get("narrative")
        .and_then(|v| v.as_str())
        .unwrap_or(&text)
        .to_string();
    let key_decisions = json_array_of_strings(&json, "key_decisions");
    let files_modified = json_array_of_strings(&json, "files_modified");
    let concepts = json_array_of_strings(&json, "concepts");
    Ok((narrative, key_decisions, files_modified, concepts))
}

/// Extract cross-session semantic facts from an episodic narrative.
///
/// `corpus` (the plans/reviews digest) is appended to the prompt when
/// non-empty so extraction sees knowledge earlier plans established.
/// Existing facts are listed with their ids so the LLM can refine them
/// in place (merged via the optional `id` field, not duplicated).
async fn extract_semantic_facts(
    store: &dyn MemoryStoreTrait,
    narrative: &str,
    session_id: &str,
    now: i64,
    corpus: &str,
    llm: &dyn LlmClient,
) -> Result<()> {
    let corpus_section = if corpus.is_empty() {
        String::new()
    } else {
        format!("\n\nPrior plans and review reports:\n{corpus}")
    };
    // List the newest existing facts so the LLM can refine them in place
    // (via the optional 'id' field) instead of duplicating near-misses.
    let mut existing = store.list_by_tier(MemoryTier::Semantic).await?;
    existing.sort_by(|a, b| b.last_accessed_at.cmp(&a.last_accessed_at));
    existing.truncate(EXISTING_FACTS_LIMIT);
    let existing_section = if existing.is_empty() {
        String::new()
    } else {
        let lines: String = existing
            .iter()
            .map(|m| {
                let fact =
                    serde_json::from_value::<crate::memory::types::SemanticData>(m.data.clone())
                        .map(|d| d.fact)
                        .unwrap_or_else(|_| m.content.clone());
                let capped: String = fact.chars().take(EXISTING_FACT_CHAR_CAP).collect();
                format!("  {}: {capped}\n", m.id)
            })
            .collect();
        format!(
            "\n\nExisting facts (id: text) — set 'id' when an entry refines or corrects one of these:\n{lines}"
        )
    };
    let prompt = format!(
        "Extract distinct, reusable facts about this project from the session summary. Return a JSON array of objects with a 'fact' field, a 'confidence' field (0-1), and optionally an 'id' field — the id of the existing fact this entry refines or corrects. Omit 'id' for genuinely new facts.\n\nSummary:\n{narrative}{corpus_section}{existing_section}"
    );
    let messages = vec![Message::user_text(prompt)];
    let stream = llm.complete(&messages, &[], None).await?;
    use futures::StreamExt;
    let mut text = String::new();
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        match event {
            crate::provider::LlmEvent::TextDelta { text: t } => text.push_str(&t),
            crate::provider::LlmEvent::Error { .. } => break,
            _ => {}
        }
    }

    let facts: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    for fact in facts {
        if let Some(fact_str) = fact.get("fact").and_then(|v| v.as_str()) {
            let confidence = fact
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5);
            let data = crate::memory::types::SemanticData {
                fact: fact_str.to_string(),
                confidence,
            };
            // When the LLM names the fact this entry refines, rewrite that
            // memory in place (same id, bumped strength, preserved history)
            // so reinforcement compounds instead of duplicating near-misses.
            let update_id = fact.get("id").and_then(|v| v.as_str());
            let existing_hit = update_id.and_then(|id| {
                existing
                    .iter()
                    .find(|m| m.id == id && m.tier == MemoryTier::Semantic)
            });
            let mut mem = match existing_hit {
                Some(prior) => {
                    let mut m = prior.clone();
                    m.title = fact_str.chars().take(60).collect::<String>();
                    m.content = fact_str.to_string();
                    m.strength = (prior.strength + 0.1).min(1.0);
                    // Just-reinforced facts rank by recency (last_accessed_at
                    // drives recall ordering + strength decay); history is
                    // preserved via access_count + created_at.
                    m.last_accessed_at = now;
                    // Clear the stale embedding so `write` re-embeds the
                    // updated content instead of persisting the old vector.
                    m.embedding.clear();
                    m
                }
                None => Memory::new(
                    MemoryTier::Semantic,
                    fact_str.chars().take(60).collect::<String>(),
                    fact_str.to_string(),
                    now,
                ),
            };
            mem.data = serde_json::to_value(&data).unwrap_or(serde_json::Value::Null);
            if !mem.source_session_ids.iter().any(|s| s == session_id) {
                mem.source_session_ids.push(session_id.to_string());
            }
            // Fire-and-forget stays (one failed fact must not abort the
            // remaining facts), but a lost fact is lost knowledge — log it
            // (quality review LOW 6, class-closure).
            if let Err(e) = store.write(mem).await {
                eprintln!("mnemo: failed to write semantic fact: {e}");
            }
        }
    }
    Ok(())
}

/// Extract recurring procedural workflows from episodic narratives.
///
/// `corpus` (the plans/reviews digest) is appended to the prompt when
/// non-empty so extraction sees workflows earlier plans established.
/// A workflow whose name matches an existing one (case-insensitive) is
/// rewritten in place with `frequency + 1`, so recurring workflows
/// compound instead of duplicating per session.
async fn extract_procedural_workflows(
    store: &dyn MemoryStoreTrait,
    narrative: &str,
    session_id: &str,
    now: i64,
    corpus: &str,
    llm: &dyn LlmClient,
) -> Result<()> {
    let corpus_section = if corpus.is_empty() {
        String::new()
    } else {
        format!("\n\nPrior plans and review reports:\n{corpus}")
    };
    // Fetch existing workflows once; name matches below compound in place.
    let existing = store.list_by_tier(MemoryTier::Procedural).await?;
    let prompt = format!(
        "Identify any recurring workflow or procedure from this session summary — a named sequence of steps with a trigger condition and expected outcome. Return a JSON array of objects with fields: name, steps (array), trigger_condition, expected_outcome, frequency (integer). If none, return [].\n\nSummary:\n{narrative}{corpus_section}"
    );
    let messages = vec![Message::user_text(prompt)];
    let stream = llm.complete(&messages, &[], None).await?;
    use futures::StreamExt;
    let mut text = String::new();
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        match event {
            crate::provider::LlmEvent::TextDelta { text: t } => text.push_str(&t),
            crate::provider::LlmEvent::Error { .. } => break,
            _ => {}
        }
    }

    let procs: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    for proc in procs {
        let mut data = crate::memory::types::ProceduralData {
            name: proc
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            steps: json_array_of_strings(&proc, "steps"),
            trigger_condition: proc
                .get("trigger_condition")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            expected_outcome: proc
                .get("expected_outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            frequency: proc.get("frequency").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
        };
        if data.name.is_empty() {
            continue;
        }
        // A name match (case-insensitive) means this workflow recurred:
        // rewrite the existing memory in place — same id, refreshed steps,
        // frequency + 1, bumped strength — so recurrence compounds.
        let existing_hit = existing
            .iter()
            .find(|m| m.title.eq_ignore_ascii_case(&data.name));
        let mut mem = match existing_hit {
            Some(prior) => {
                // Saturating: a corrupt/oversized stored frequency must not
                // truncate (u32) or overflow (+1 panics in debug builds).
                data.frequency = u32::try_from(
                    prior
                        .data
                        .get("frequency")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                )
                .unwrap_or(u32::MAX)
                .saturating_add(1);
                let mut m = prior.clone();
                m.content = data.steps.join("; ");
                m.strength = (prior.strength + 0.1).min(1.0);
                // Just-reinforced workflows rank by recency (last_accessed_at
                // drives recall ordering + strength decay); history is
                // preserved via access_count + created_at.
                m.last_accessed_at = now;
                // Clear the stale embedding so `write` re-embeds the
                // refreshed steps instead of persisting the old vector.
                m.embedding.clear();
                m
            }
            None => Memory::new(
                MemoryTier::Procedural,
                data.name.clone(),
                data.steps.join("; "),
                now,
            ),
        };
        mem.data = serde_json::to_value(&data).unwrap_or(serde_json::Value::Null);
        if !mem.source_session_ids.iter().any(|s| s == session_id) {
            mem.source_session_ids.push(session_id.to_string());
        }
        // Fire-and-forget stays (one failed workflow must not abort the
        // remaining workflows), but a lost workflow is lost knowledge —
        // log it (quality review LOW 6, class-closure).
        if let Err(e) = store.write(mem).await {
            eprintln!("mnemo: failed to write procedural workflow: {e}");
        }
    }
    Ok(())
}

fn json_array_of_strings(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::MemoryStore;
    use crate::provider::{Capabilities, LlmEvent, ProviderKind, ToolChoice, ToolSchema};
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// A mock LLM client that returns a sequence of canned text responses.
    /// Each `complete` call pops the next response from the queue and streams
    /// it as a single TextDelta + Finish. Used to test consolidation's three
    /// LLM calls (synthesize, extract_semantic, extract_procedural) without a
    /// real provider. Also records every prompt it receives so tests can
    /// assert what consolidation fed the LLM.
    struct MockLlm {
        responses: Arc<Mutex<std::collections::VecDeque<String>>>,
        prompts: Arc<Mutex<Vec<String>>>,
        caps: Capabilities,
    }

    impl MockLlm {
        /// Create a mock that returns the given responses in order.
        fn sequence(responses: Vec<String>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                prompts: Arc::new(Mutex::new(Vec::new())),
                caps: Capabilities::openai(),
            }
        }

        /// The prompts received so far, in call order.
        async fn prompts(&self) -> Vec<String> {
            self.prompts.lock().await.clone()
        }
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-consolidation"
        }
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            // Record the first user message text so tests can assert what
            // consolidation fed the LLM (e.g. the corpus marker).
            if let Some(msg) = messages.first() {
                self.prompts.lock().await.push(msg.content.as_text());
            }
            let text = self.responses.lock().await.pop_front().unwrap_or_default();
            let events = vec![
                LlmEvent::TextDelta { text },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// An LLM whose every `complete` call fails — pins that extraction
    /// failures never propagate: consolidation still returns Ok, falls
    /// back to the synthetic episodic summary, and cleans up the working
    /// tier (fire-and-forget semantics, quality review LOW 6).
    struct FailingLlm {
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for FailingLlm {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "failing-mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            Err(crate::error::Error::Memory(
                "mock LLM failure".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn consolidates_with_llm_creates_all_tiers() {
        // With an LLM provider, consolidation should:
        // 1. synthesize_with_llm → an episodic summary (narrative + decisions +
        //    files + concepts).
        // 2. extract_semantic_facts → one or more semantic memories.
        // 3. extract_procedural_workflows → one or more procedural memories.
        // And then clean up the working-tier events for the session.
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();

        // Record working-memory events for the session.
        store
            .record_tool_event(
                Some("sess-llm"),
                "file_write",
                serde_json::json!({"path": "src/main.rs"}),
                "written",
                None,
            )
            .await
            .unwrap();

        // Canned LLM responses, in call order:
        // 1. synthesize_with_llm → episodic summary JSON.
        // 2. extract_semantic_facts → JSON array of facts.
        // 3. extract_procedural_workflows → JSON array of workflows.
        let llm = MockLlm::sequence(vec![
            serde_json::json!({
                "narrative": "The agent wrote src/main.rs to add a main function.",
                "key_decisions": ["used fn main()"],
                "files_modified": ["src/main.rs"],
                "concepts": ["rust", "main"]
            })
            .to_string(),
            serde_json::json!([
                {"fact": "The project uses Rust 2021 edition.", "confidence": 0.9}
            ])
            .to_string(),
            serde_json::json!([
                {
                    "name": "write-file-then-test",
                    "steps": ["write file", "run cargo test"],
                    "trigger_condition": "when adding a new module",
                    "expected_outcome": "tests pass",
                    "frequency": 3
                }
            ])
            .to_string(),
        ]);

        let id = consolidate_session(&store, "sess-llm", Some(&llm), "")
            .await
            .unwrap();
        assert!(!id.is_empty());

        // Episodic memory created with the LLM narrative.
        let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
        assert_eq!(episodic.len(), 1);
        assert!(episodic[0].content.contains("wrote src/main.rs"));
        let epi_data: EpisodicData = serde_json::from_value(episodic[0].data.clone()).unwrap();
        assert!(epi_data.files_modified.contains(&"src/main.rs".to_string()));
        assert!(epi_data.concepts.contains(&"rust".to_string()));

        // Semantic memory created from the extracted fact.
        let semantic = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
        assert_eq!(semantic.len(), 1);
        assert!(semantic[0].content.contains("Rust 2021 edition"));
        let sem_data: crate::memory::types::SemanticData =
            serde_json::from_value(semantic[0].data.clone()).unwrap();
        assert!((sem_data.confidence - 0.9).abs() < 1e-9);

        // Procedural memory created from the extracted workflow.
        let procedural = store.list_by_tier(MemoryTier::Procedural).await.unwrap();
        assert_eq!(procedural.len(), 1);
        assert_eq!(procedural[0].title, "write-file-then-test");
        let proc_data: crate::memory::types::ProceduralData =
            serde_json::from_value(procedural[0].data.clone()).unwrap();
        assert_eq!(proc_data.steps, vec!["write file", "run cargo test"]);
        assert_eq!(proc_data.frequency, 3);

        // Working-tier events for the session are cleaned up.
        let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
        assert!(
            working
                .iter()
                .all(|m| !m.source_session_ids.iter().any(|s| s == "sess-llm")),
            "working events for sess-llm should be cleaned up after consolidation"
        );
    }

    #[tokio::test]
    async fn consolidates_with_llm_falls_back_on_bad_json() {
        // If the LLM returns non-JSON for the synthesis, consolidation falls
        // back to synthetic compression (the narrative is the raw text) rather
        // than failing. The semantic/procedural extraction also degrades
        // gracefully (empty arrays).
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();
        store
            .record_tool_event(
                Some("sess-bad"),
                "file_read",
                serde_json::json!({"path": "a.rs"}),
                "contents",
                None,
            )
            .await
            .unwrap();

        // All three responses are non-JSON garbage.
        let llm = MockLlm::sequence(vec![
            "not json at all".to_string(),
            "also not json".to_string(),
            "nope".to_string(),
        ]);

        let id = consolidate_session(&store, "sess-bad", Some(&llm), "")
            .await
            .unwrap();
        assert!(!id.is_empty());

        // Episodic memory still created (synthetic fallback).
        let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
        assert_eq!(episodic.len(), 1);
        // No semantic/procedural memories (extraction got no valid JSON arrays).
        assert!(store
            .list_by_tier(MemoryTier::Semantic)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .list_by_tier(MemoryTier::Procedural)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn consolidates_working_to_episodic_without_llm() {
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();

        // Record some working-memory events.
        store
            .record_tool_event(
                Some("sess-1"),
                "file_read",
                serde_json::json!({"path": "src/main.rs"}),
                "fn main() {}",
                None,
            )
            .await
            .unwrap();
        store
            .record_tool_event(
                Some("sess-1"),
                "file_write",
                serde_json::json!({"path": "src/main.rs"}),
                "written",
                None,
            )
            .await
            .unwrap();

        // Consolidate without an LLM.
        let id = consolidate_session(&store, "sess-1", None, "")
            .await
            .unwrap();
        assert!(!id.is_empty());

        // An episodic memory should now exist.
        let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
        assert_eq!(episodic.len(), 1);
        assert!(episodic[0].content.contains("tool calls"));
        // files_modified should include src/main.rs.
        let data: EpisodicData = serde_json::from_value(episodic[0].data.clone()).unwrap();
        assert!(data.files_modified.contains(&"src/main.rs".to_string()));

        // Consolidation must clean up the raw working-memory events for the
        // session — they've been compressed into the episodic summary above.
        let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
        let sess1_working: Vec<_> = working
            .iter()
            .filter(|m| m.source_session_ids.iter().any(|s| s == "sess-1"))
            .collect();
        assert!(
            sess1_working.is_empty(),
            "working-tier events for sess-1 should be deleted after consolidation"
        );
    }

    #[tokio::test]
    async fn consolidate_preserves_other_sessions_working_memory() {
        // Cleanup must be scoped to the consolidated session only — working
        // events from a different session must survive.
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();

        store
            .record_tool_event(
                Some("sess-1"),
                "file_read",
                serde_json::json!({"path": "a.rs"}),
                "a",
                None,
            )
            .await
            .unwrap();
        store
            .record_tool_event(
                Some("sess-2"),
                "file_read",
                serde_json::json!({"path": "b.rs"}),
                "b",
                None,
            )
            .await
            .unwrap();

        // Consolidate only sess-1.
        consolidate_session(&store, "sess-1", None, "")
            .await
            .unwrap();

        let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
        // sess-1's events are gone; sess-2's survive.
        assert!(
            working
                .iter()
                .all(|m| !m.source_session_ids.iter().any(|s| s == "sess-1")),
            "sess-1 working events should be deleted"
        );
        assert!(
            working
                .iter()
                .any(|m| m.source_session_ids.iter().any(|s| s == "sess-2")),
            "sess-2 working events should be preserved"
        );
    }

    #[tokio::test]
    async fn consolidate_empty_session_is_noop() {
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();
        let id = consolidate_session(&store, "empty", None, "")
            .await
            .unwrap();
        assert!(id.is_empty());
    }

    #[test]
    fn corpus_digest_includes_recent_plans_and_reviews() {
        let dir = tempfile::tempdir().unwrap();
        let plans = dir.path().join("plans");
        let reviews = dir.path().join("reviews");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::create_dir_all(&reviews).unwrap();
        std::fs::write(plans.join("plan-a.md"), "GOAL: ship the corpus digest.").unwrap();
        std::fs::write(
            reviews.join("rev-b.md"),
            "FINDING: truncation was too aggressive.",
        )
        .unwrap();
        // A non-markdown file must be ignored.
        std::fs::write(plans.join("stack.json"), "{}").unwrap();

        let digest = corpus_digest(&plans, &reviews);
        assert!(digest.contains("=== PLAN: plan-a.md ==="));
        assert!(digest.contains("GOAL: ship the corpus digest."));
        assert!(digest.contains("=== REVIEW: rev-b.md ==="));
        assert!(digest.contains("FINDING: truncation was too aggressive."));
        assert!(!digest.contains("stack.json"));

        // Missing dirs → empty digest (no corpus).
        assert!(corpus_digest(&dir.path().join("nope"), &dir.path().join("nore")).is_empty());
    }

    #[tokio::test]
    async fn corpus_is_fed_to_extraction_prompts() {
        // The corpus digest must reach BOTH extraction prompts (semantic +
        // procedural), not just the synthesis — that's how consolidation
        // merges with knowledge established by earlier plans/reviews.
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();
        store
            .record_tool_event(
                Some("sess-corpus"),
                "file_write",
                serde_json::json!({"path": "src/a.rs"}),
                "written",
                None,
            )
            .await
            .unwrap();

        let llm = MockLlm::sequence(vec![
            serde_json::json!({
                "narrative": "agent wrote src/a.rs",
                "key_decisions": [],
                "files_modified": ["src/a.rs"],
                "concepts": []
            })
            .to_string(),
            "[]".to_string(),
            "[]".to_string(),
        ]);
        let corpus = "=== PLAN: earlier.md ===\nGOAL: learn from plans.";
        consolidate_session(&store, "sess-corpus", Some(&llm), corpus)
            .await
            .unwrap();

        let prompts = llm.prompts().await;
        assert_eq!(prompts.len(), 3, "synthesize + semantic + procedural");
        // Prompt 0 is the synthesis (no corpus there — it summarizes events).
        // Prompts 1 and 2 are the extractions; both must carry the corpus.
        for idx in [1, 2] {
            assert!(
                prompts[idx].contains("Prior plans and review reports:"),
                "extraction prompt {idx} must carry the corpus section"
            );
            assert!(
                prompts[idx].contains("GOAL: learn from plans."),
                "extraction prompt {idx} must carry the corpus content"
            );
        }
    }

    #[tokio::test]
    async fn semantic_extraction_merges_existing_facts() {
        // When the LLM refines an existing fact (entry carries its 'id') the
        // memory is rewritten in place — same id, bumped strength, preserved
        // history — instead of duplicating. New facts still create rows.
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();

        // Seed one existing semantic fact with known id + strength.
        let mut seed = Memory::new(
            MemoryTier::Semantic,
            "old title",
            "The build uses cargo workspaces.",
            0,
        );
        seed.id = "fact-known".to_string();
        seed.strength = 0.5;
        seed.data = serde_json::to_value(crate::memory::types::SemanticData {
            fact: "The build uses cargo workspaces.".to_string(),
            confidence: 0.6,
        })
        .unwrap();
        store.write(seed).await.unwrap();

        store
            .record_tool_event(
                Some("sess-merge"),
                "file_read",
                serde_json::json!({"path": "Cargo.toml"}),
                "contents",
                None,
            )
            .await
            .unwrap();

        // Canned responses: synthesis, then facts (one update via id + one
        // genuinely new), then no procedural workflows.
        let llm = MockLlm::sequence(vec![
            serde_json::json!({
                "narrative": "agent inspected Cargo.toml",
                "key_decisions": [],
                "files_modified": ["Cargo.toml"],
                "concepts": []
            })
            .to_string(),
            serde_json::json!([
                {"id": "fact-known", "fact": "The build uses a cargo workspace with two crates.", "confidence": 0.9},
                {"fact": "Tests run via cargo test unpiped.", "confidence": 0.7}
            ])
            .to_string(),
            "[]".to_string(),
        ]);
        consolidate_session(&store, "sess-merge", Some(&llm), "")
            .await
            .unwrap();

        // The extraction prompt listed the existing fact with its id.
        let prompts = llm.prompts().await;
        assert!(
            prompts[1].contains("fact-known"),
            "existing fact id must be listed"
        );
        assert!(
            prompts[1].contains("cargo workspaces"),
            "existing fact text must be listed"
        );

        // Exactly two semantic memories: the refined one (same id, updated
        // content, strength 0.5 + 0.1) and the new one.
        let semantic = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
        assert_eq!(semantic.len(), 2, "update + new = 2 rows, not 3");
        let merged = semantic
            .iter()
            .find(|m| m.id == "fact-known")
            .expect("merged fact keeps its id");
        assert!(merged.content.contains("two crates"));
        assert!(
            (merged.strength - 0.6).abs() < 1e-9,
            "strength bumps 0.5 → 0.6"
        );
        assert_eq!(merged.access_count, 0, "history preserved");
        assert!(
            merged.source_session_ids.iter().any(|s| s == "sess-merge"),
            "session appended to source ids"
        );
        let fresh = semantic
            .iter()
            .find(|m| m.id != "fact-known")
            .expect("new fact written");
        assert!(fresh.content.contains("cargo test unpiped"));
    }

    #[tokio::test]
    async fn procedural_extraction_compounds_by_name() {
        // A recurring workflow (name match, case-insensitive) rewrites the
        // existing memory in place: same id, frequency + 1, bumped strength —
        // one row, not one per session.
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();

        // Seed 'review protocol' with frequency 3 and a known id.
        let mut seed = Memory::new(
            MemoryTier::Procedural,
            "review protocol",
            "spawn reviewer; fix findings",
            0,
        );
        seed.id = "proc-known".to_string();
        seed.strength = 0.7;
        seed.data = serde_json::to_value(crate::memory::types::ProceduralData {
            name: "review protocol".to_string(),
            steps: vec!["spawn reviewer".to_string(), "fix findings".to_string()],
            trigger_condition: "end of implementation plan".to_string(),
            expected_outcome: "review report closed".to_string(),
            frequency: 3,
        })
        .unwrap();
        store.write(seed).await.unwrap();

        store
            .record_tool_event(
                Some("sess-proc"),
                "file_write",
                serde_json::json!({"path": "src/b.rs"}),
                "written",
                None,
            )
            .await
            .unwrap();

        // LLM returns the SAME workflow with different casing + refreshed
        // steps — must compound onto the existing row, not duplicate.
        let llm = MockLlm::sequence(vec![
            serde_json::json!({
                "narrative": "agent ran the review protocol again",
                "key_decisions": [],
                "files_modified": ["src/b.rs"],
                "concepts": []
            })
            .to_string(),
            "[]".to_string(),
            serde_json::json!([
                {
                    "name": "Review Protocol",
                    "steps": ["spawn reviewer", "fix findings", "re-run tests"],
                    "trigger_condition": "end of implementation plan",
                    "expected_outcome": "all findings fixed",
                    "frequency": 1
                }
            ])
            .to_string(),
        ]);
        consolidate_session(&store, "sess-proc", Some(&llm), "")
            .await
            .unwrap();

        // Single procedural row, same id, frequency 4 (3 + 1 — the LLM's
        // frequency field is ignored on merge), strength 0.7 → 0.8.
        let procedural = store.list_by_tier(MemoryTier::Procedural).await.unwrap();
        assert_eq!(procedural.len(), 1, "name match compounds, no duplicate");
        assert_eq!(procedural[0].id, "proc-known", "same id rewritten");
        let data: crate::memory::types::ProceduralData =
            serde_json::from_value(procedural[0].data.clone()).unwrap();
        assert_eq!(data.frequency, 4, "frequency 3 + 1");
        assert_eq!(
            data.steps,
            vec![
                "spawn reviewer".to_string(),
                "fix findings".to_string(),
                "re-run tests".to_string()
            ],
            "steps refreshed from extraction"
        );
        assert!(
            (procedural[0].strength - 0.8).abs() < 1e-9,
            "strength 0.7 → 0.8"
        );
        assert!(
            procedural[0]
                .source_session_ids
                .iter()
                .any(|s| s == "sess-proc"),
            "session appended to source ids"
        );
    }

    /// Locate a top-level fn's body in this file's source WITHOUT the
    /// asserted literals matching this test module's own text: the needles
    /// below (log lines, swallow shapes) also appear in these assertions,
    /// so scoping to the owning fn's body keeps the test's own source out
    /// of the match. Returns the text from the signature through the fn's
    /// closing brace (the column-0 `}`).
    fn fn_body(src: &str, name: &str) -> String {
        let needle = format!("fn {name}(");
        let start = src
            .find(&needle)
            .unwrap_or_else(|| panic!("{name} not found in source"));
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        src[start..end].to_string()
    }

    /// Source-contract regression test (quality review LOW 6 class-closure,
    /// backlog 93eee4a3): every durable-write swallow in the memory
    /// subsystem either logs on Err or is a documented best-effort. Fails if
    /// a site reverts to a bare `let _ =` swallow — the log lines ARE the
    /// fix, so removing or rewording one must be a conscious act that
    /// updates this test.
    #[test]
    fn durable_write_failures_log_instead_of_vanishing() {
        // consolidation.rs: the five swallow sites are now log-on-Err,
        // scoped to the owning fn bodies so this test's own needles never
        // self-match.
        let src = include_str!("consolidation.rs");
        let core = fn_body(&src, "consolidate_session_with_events");
        for needle in [
            "mnemo: semantic fact extraction failed",
            "mnemo: procedural workflow extraction failed",
            "mnemo: failed to delete working events for session",
        ] {
            assert!(core.contains(needle), "missing log line: {needle}");
        }
        for banned in [
            "let _ = extract_semantic_facts",
            "let _ = extract_procedural_workflows",
            "let _ = store.delete_working_for_session",
        ] {
            assert!(
                !core.contains(banned),
                "a swallow crept back into consolidate_session_with_events: {banned}"
            );
        }
        let semantic = fn_body(&src, "extract_semantic_facts");
        assert!(
            semantic.contains("mnemo: failed to write semantic fact"),
            "the semantic-fact write must log its failure"
        );
        assert!(
            !semantic.contains("let _ = store.write"),
            "a store.write swallow crept back into extract_semantic_facts"
        );
        let procedural = fn_body(&src, "extract_procedural_workflows");
        assert!(
            procedural.contains("mnemo: failed to write procedural workflow"),
            "the procedural-workflow write must log its failure"
        );
        assert!(
            !procedural.contains("let _ = store.write"),
            "a store.write swallow crept back into extract_procedural_workflows"
        );

        // agent.rs: the user-correction capture logs on Err.
        let agent = include_str!("../runtime/agent.rs");
        assert!(
            agent.contains("mnemo: failed to record user-correction event"),
            "the user-correction capture must log its failure"
        );

        // indexer/knowledge.rs: the migrated-row delete logs on Err; the
        // marker stays best-effort WITH its documented rationale.
        let indexer = include_str!("indexer/knowledge.rs");
        assert!(
            indexer.contains("mnemo: failed to delete migrated row"),
            "the migrated-row delete must log its failure"
        );
        assert!(
            indexer.contains("idempotency keeps re-runs safe"),
            "the marker's best-effort rationale must stay documented"
        );

        // memory/mod.rs: the advisory access bump stays swallowed WITH its
        // documented rationale.
        let mem = include_str!("mod.rs");
        assert!(
            mem.contains("advisory and deliberately swallowed"),
            "the batch_access swallow rationale must stay documented"
        );
    }

    #[tokio::test]
    async fn extraction_failure_does_not_fail_consolidation() {
        // A failing LLM must not fail consolidation: the episodic summary
        // falls back to synthetic compression, the (now logged, LOW 6)
        // extraction failures stay fire-and-forget, and the working tier
        // is still cleaned up.
        let embedder: Arc<dyn crate::memory::embedder::Embedder> = Arc::new(HashEmbedder::new());
        let store = MemoryStore::open_in_memory(embedder).unwrap();
        store
            .record_tool_event(
                Some("sess-fail"),
                "file_read",
                serde_json::json!({"path": "a.rs"}),
                "a",
                None,
            )
            .await
            .unwrap();

        let llm = FailingLlm {
            caps: Capabilities::openai(),
        };
        let id = consolidate_session(&store, "sess-fail", Some(&llm), "")
            .await
            .unwrap();
        assert!(!id.is_empty(), "consolidation must succeed");

        // Synthetic episodic summary exists (the LLM synthesis failed →
        // fallback)...
        let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
        assert_eq!(episodic.len(), 1, "synthetic fallback wrote the summary");
        // ...no semantic/procedural rows (extraction failed)...
        assert!(
            store
                .list_by_tier(MemoryTier::Semantic)
                .await
                .unwrap()
                .is_empty(),
            "no semantic rows from a failed extraction"
        );
        assert!(
            store
                .list_by_tier(MemoryTier::Procedural)
                .await
                .unwrap()
                .is_empty(),
            "no procedural rows from a failed extraction"
        );
        // ...and the working tier is still cleaned up.
        let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
        assert!(
            working
                .iter()
                .all(|m| !m.source_session_ids.iter().any(|s| s == "sess-fail")),
            "the working tier is still cleaned up"
        );
    }
}

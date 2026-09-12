// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Cross-tool steering helpers — structural nudges that surface memory at
//! tool-choice points.
//!
//! [`recalled_context_block`] builds the passive `RECALLED CONTEXT` rider
//! shared by `create_plan` (appended to the create result) and `spawn_agent`
//! (appended to the task, which becomes the spawned agent's FIRST PROMPT).
//! Both riders are best-effort: no store, no hits, or any store error yields
//! `None` — the steering must never block or fail the host tool.
//!
//! Recency gate: the riders surface CURRENT project context, so broad-query
//! hits older than [`STEERING_MAX_AGE_DAYS`] are silently dropped — a
//! month-old plan/review digest matched by keyword overlap would otherwise
//! ride every plan for months (observed with 2026-08-10/08-14 PLAN digests),
//! and old knowledge stays discoverable via `memory_search`. Bug records are
//! exempt wherever they surface: a recorded root cause is durable knowledge
//! regardless of age, and it is exactly the reuse a rider exists for.

use std::sync::Arc;

use crate::memory::{MemoryFilter, MemoryRecordType, MemoryStoreTrait, MemoryTier};

/// Max age (days) of a rider hit from the BROAD query. The riders surface
/// project context that should be consulted NOW; stale digests matched by
/// keyword overlap are noise riding every plan. Bug records are exempt: a
/// recorded root cause is durable knowledge regardless of age.
const STEERING_MAX_AGE_DAYS: u64 = 14;

/// Wall-clock "now" in unix seconds — the riders' age-gate reference. A
/// single helper so the call sites read cleanly; tests needing a
/// deterministic age boundary use [`recalled_context_block_at`] instead of
/// racing `SystemTime` against this.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Seconds per day (`STEERING_MAX_AGE_DAYS` is in days).
const SECS_PER_DAY: i64 = 86_400;

/// F8: significant title terms for the rider boost — lowercase
/// alphanumeric runs of ≥4 chars (short runs match nearly every title).
fn title_terms(title: &str) -> Vec<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 4)
        .map(String::from)
        .collect()
}

/// F11: the working-memory event count at which the consolidation-due note
/// fires (once per session). The 2026-09-15/16 sessions accumulated ~18
/// working events with no surface ever suggesting distillation; 15 is late
/// enough to mark a genuinely long session and early enough to act on.
const CONSOLIDATION_NUDGE_THRESHOLD: usize = 15;

/// F11: fired-once-per-session gate — sessions that already saw (or were
/// due for) the consolidation note never pay for the count query again.
fn nudged_sessions() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static NUDGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    NUDGED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Reset the fired-once gate (tests only — the gate is process-global).
#[cfg(test)]
pub(crate) fn reset_consolidation_gate() {
    match nudged_sessions().lock() {
        Ok(mut set) => set.clear(),
        Err(poisoned) => poisoned.into_inner().clear(),
    }
}

/// F11: when the session's working-memory row count crosses
/// [`CONSOLIDATION_NUDGE_THRESHOLD`], return the fired-only note suggesting
/// `memory_consolidate`. The count mirrors consolidation's own session
/// filter (`source_session_ids` contains the id). Best-effort: no store, no
/// session id, or any store error yields `None`; the count query stops for
/// a session once the note has fired (the static gate). Advisory only.
pub(crate) async fn consolidation_due_note(
    store: Option<&Arc<dyn MemoryStoreTrait>>,
    session_id: Option<&str>,
) -> Option<String> {
    let session_id = session_id?;
    let store = store?;
    // Reserve FIRST and decide under the reservation (hold-and-decide, not
    // check-then-act — review 2026-09-17 finding 2): a concurrent dispatch
    // for the same session sees the reservation and stays silent, so the
    // note fires at most once per session even under interleaving.
    {
        let mut set = match nudged_sessions().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !set.insert(session_id.to_string()) {
            return None; // already nudged — or reserved by an in-flight check
        }
    }
    let n = store
        .list_by_tier(MemoryTier::Working)
        .await
        .ok()
        .map(|working| {
            working
                .iter()
                .filter(|m| m.source_session_ids.iter().any(|s| s == session_id))
                .count()
        });
    let note = match n {
        Some(n) if n >= CONSOLIDATION_NUDGE_THRESHOLD => Some(format!(
            "NOTE: {n} working-memory events accumulated this session — consider \
             memory_consolidate(session_id) to distill them"
        )),
        _ => None,
    };
    if note.is_none() {
        // Below the threshold (or the store errored) — release the
        // reservation so a later dispatch retries the count.
        match nudged_sessions().lock() {
            Ok(mut set) => {
                set.remove(session_id);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(session_id);
            }
        }
    }
    note
}

/// Best-effort passive recall, shared by the `create_plan` and `spawn_agent`
/// riders (each tool's `memory` field). Queries the store PASSIVELY —
/// `recall_peek`, no access bump: being surfaced by a rider is not evidence
/// the knowledge was useful, and a bump here would feed the
/// self-reinforcement loop (diagnosed 2026-08-20). One broad query (8
/// candidates, 3 shown — see `RIDER_CANDIDATES`), plus one targeted
/// `record_type: Bug` query (limit 2) when `bug_query` is given (bug plans)
/// — a recorded root cause is the strongest reuse signal there is. When
/// `title` is given (create_plan), candidates whose TITLE shares a
/// significant term with the plan title are stably promoted before the cap
/// (F8: the broad query alone ranked an on-point SPEC below generic hits).
/// Hits are deduped by id, age-gated (broad hits of the last
/// [`STEERING_MAX_AGE_DAYS`] only; bug records exempt — see the module
/// doc), capped at 5, rendered as `- [<tier>] <title> (id: ...) — <gist>`
/// (gist = first content line, ≤140 chars), and wrapped in the stable
/// `RECALLED CONTEXT (...)` header. Returns `None` on no store, no hits,
/// or any store error — a rider must never block or fail its host tool.
pub(crate) async fn recalled_context_block(
    store: Option<&Arc<dyn MemoryStoreTrait>>,
    title: Option<&str>,
    query: &str,
    bug_query: Option<&str>,
) -> Option<String> {
    recalled_context_block_at(store, title, query, bug_query, now_secs()).await
}

/// [`recalled_context_block`] with an injected clock — the internal shape,
/// kept separate so tests can pin the age boundary deterministically
/// (`now` = the unix-seconds reference for the [`STEERING_MAX_AGE_DAYS`]
/// gate; production goes through [`recalled_context_block`]).
async fn recalled_context_block_at(
    store: Option<&Arc<dyn MemoryStoreTrait>>,
    title: Option<&str>,
    query: &str,
    bug_query: Option<&str>,
    now: i64,
) -> Option<String> {
    /// Rider rows cap — enough to point at prior work, not enough to bloat
    /// the host result.
    const RIDER_HITS: usize = 5;
    /// Max chars of a hit's gist line (pointer-first records carry the gist
    /// in line 1; long bodies must not ride along).
    const GIST_CHARS: usize = 140;
    /// F8: the broad pass fetches MORE candidates than it shows — the
    /// title-term boost needs room to promote an on-point hit ranked below
    /// generic noise.
    const RIDER_CANDIDATES: usize = 8;
    /// F8: how many broad candidates survive into the rider (the historical
    /// limit-3 broad cap; bug-pass hits ride alongside).
    const BROAD_HITS: usize = 3;

    let store = store?;
    let mut hits = store
        .recall_peek(
            query,
            &MemoryFilter {
                limit: Some(RIDER_CANDIDATES),
                ..MemoryFilter::default()
            },
        )
        .await
        .ok()?;
    // Recency gate: drop broad hits older than STEERING_MAX_AGE_DAYS (>=
    // semantics — a hit at exactly the cutoff age is kept). The riders
    // surface *current* project context; a month-old digest matched by
    // keyword overlap is noise riding every plan. Applied HERE, before the
    // bug pass appends its hits, so the bug exemption is structural.
    let min_created = now - (STEERING_MAX_AGE_DAYS as i64) * SECS_PER_DAY;
    hits.retain(|h| h.memory.created_at >= min_created);
    // F8 title-term boost: a candidate whose TITLE shares a significant
    // term with the plan title is stably promoted above the rest before
    // the broad cap. None-title callers (spawn_agent) keep pure rank order.
    if let Some(title) = title {
        let terms = title_terms(title);
        if !terms.is_empty() {
            hits.sort_by_key(|h| {
                let hit_title = h.memory.title.to_lowercase();
                !terms.iter().any(|t| hit_title.contains(t))
            });
        }
    }
    hits.truncate(BROAD_HITS);
    // Bug plans get one extra targeted recall: a recorded root cause may
    // already exist (record_type BUG) — the strongest reuse signal there is.
    // Exempt from the age gate on purpose: durable knowledge never stales,
    // and the exemption needs no special-casing here because the gate ran
    // before these hits were appended.
    if let Some(bug_query) = bug_query {
        if let Ok(mut bug_hits) = store
            .recall_peek(
                bug_query,
                &MemoryFilter {
                    limit: Some(2),
                    record_type: Some(MemoryRecordType::Bug),
                    ..MemoryFilter::default()
                },
            )
            .await
        {
            hits.append(&mut bug_hits);
        }
    }
    // Dedupe by id (a memory can win both queries), cap the rider.
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.memory.id.clone()));
    hits.truncate(RIDER_HITS);
    if hits.is_empty() {
        return None;
    }
    let lines: Vec<String> = hits
        .iter()
        .map(|h| {
            let gist: String = h
                .memory
                .content
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(GIST_CHARS)
                .collect();
            format!(
                "- [{}] {} (id: {}) — {gist}",
                h.memory.tier.as_str(),
                h.memory.title,
                h.memory.id
            )
        })
        .collect();
    Some(format!(
        "\n\nRECALLED CONTEXT ({} hit(s)) — prior knowledge matched this plan; consult it before \
         executing (memory_search for full detail):\n{}\n",
        lines.len(),
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::{Memory, MemoryStore, MemoryTier};
    use std::sync::Arc;

    fn make_store() -> Arc<MemoryStore> {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
    }

    /// The test clock `now`, the fresh/stale/boundary seeds, and the day.
    const NOW: i64 = 1_800_000_000;
    const ONE_DAY: i64 = 86_400;
    /// Exactly at the cutoff (`created_at == now - 14d`) — kept (>=).
    const BOUNDARY: i64 = NOW - 14 * ONE_DAY;
    const STALE: i64 = NOW - 30 * ONE_DAY;
    const FRESH: i64 = NOW - ONE_DAY;

    async fn write(store: &MemoryStore, title: &str, content: &str, created: i64) -> String {
        let m = Memory::new(MemoryTier::Semantic, title, content, created);
        let id = m.id.clone();
        store.write(m).await.unwrap();
        id
    }

    fn harness(store: Arc<MemoryStore>) -> Option<Arc<dyn MemoryStoreTrait>> {
        Some(store as Arc<dyn MemoryStoreTrait>)
    }

    #[tokio::test]
    async fn fresh_broad_hit_rides_and_stale_is_dropped() {
        let store = make_store();
        write(
            &store,
            "PLAN: fresh plan digest",
            "Fresh plan summary — query match needle",
            FRESH,
        )
        .await;
        write(
            &store,
            "PLAN: month-old plan digest",
            "A month-old summary — query match needle",
            STALE,
        )
        .await;

        // Only the fresh hit survives the age gate; the output is unchanged
        // in shape (no dropped-hit marker, no rider at all when stale-only).
        let block = recalled_context_block_at(
            harness(store.clone()).as_ref(),
            None,
            "query match needle",
            None,
            NOW,
        )
        .await
        .expect("fresh hit rides");
        assert!(
            block.contains("PLAN: fresh plan digest"),
            "fresh hit surfaced: {block}"
        );
        assert!(
            !block.contains("month-old"),
            "stale digest silently dropped: {block}"
        );

        // A store holding ONLY a stale broad hit → rider silently absent
        // (None), exactly like a no-hit store.
        let stale_only = make_store();
        write(
            &stale_only,
            "PLAN: ancient digest",
            "ancient summary — query match needle",
            STALE,
        )
        .await;
        assert!(
            recalled_context_block_at(
                harness(stale_only).as_ref(),
                None,
                "query match needle",
                None,
                NOW,
            )
            .await
            .is_none(),
            "stale-only store must not produce a rider"
        );
    }

    #[tokio::test]
    async fn stale_bug_hit_still_rides_from_the_bug_pass() {
        let store = make_store();
        // A month-old BUG record: exempt from the age gate (durable
        // knowledge — a recorded root cause never stales).
        write(
            &store,
            "BUG: kettle crashes when water is empty",
            "SYMPTOM: kettle crashes on empty water → ROOT CAUSE: divide-by-zero in boil()",
            STALE,
        )
        .await;
        // A stale broad-class hit the bug query does not match (proves the
        // exemption is specific to the bug pass, not the whole gate).
        write(
            &store,
            "PLAN: unrelated old digest",
            "unrelated old summary — query match needle",
            STALE,
        )
        .await;

        let block = recalled_context_block_at(
            harness(store).as_ref(),
            None,
            "query match needle",
            Some("SYPTOM kettle crash root cause"),
            NOW,
        )
        .await
        .expect("bug-pass hit rides even when only stale hits match");
        assert!(
            block.contains("BUG: kettle crashes"),
            "stale bug hit surfaced via the bug pass: {block}"
        );
        assert!(
            !block.contains("unrelated old digest"),
            "stale broad hit still dropped alongside the bug hit: {block}"
        );
    }

    #[tokio::test]
    async fn age_boundary_keeps_hit_at_exactly_the_cutoff() {
        let store = make_store();
        write(
            &store,
            "PLAN: boundary digest",
            "boundary summary — query match needle",
            BOUNDARY,
        )
        .await;
        let block = recalled_context_block_at(
            harness(store).as_ref(),
            None,
            "query match needle",
            None,
            NOW,
        )
        .await
        .expect(">= semantics: hit at exactly the cutoff age is kept");
        assert!(
            block.contains("PLAN: boundary digest"),
            "boundary hit kept: {block}"
        );

        // One second past the cutoff is dropped.
        let store = make_store();
        write(
            &store,
            "PLAN: past cutoff digest",
            "past cutoff summary — query match needle",
            BOUNDARY - 1,
        )
        .await;
        assert!(
            recalled_context_block_at(
                harness(store).as_ref(),
                None,
                "query match needle",
                None,
                NOW,
            )
            .await
            .is_none(),
            "hit one second past the cutoff is dropped"
        );
    }

    #[tokio::test]
    async fn production_wrapper_uses_the_wall_clock_gate() {
        // The production entry point (wall-clock now) gates age the same
        // way: a hit written with created_at = now (created moments before
        // the call) rides; a 2001 hit would not.
        let store = make_store();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        write(
            &store,
            "PLAN: just created digest",
            "just created summary — query match needle",
            now,
        )
        .await;
        let block =
            recalled_context_block(harness(store).as_ref(), None, "query match needle", None)
                .await
                .expect("fresh production hit rides");
        assert!(
            block.contains("PLAN: just created digest"),
            "production rider surfaced: {block}"
        );
    }

    #[tokio::test]
    async fn consolidation_note_fires_once_at_the_threshold() {
        // F11: crossing the working-event threshold fires the note exactly
        // once per session; below the threshold it stays silent, and other
        // sessions are unaffected.
        reset_consolidation_gate();
        let store = make_store();
        for i in 0..14 {
            let mut m = Memory::new(
                MemoryTier::Working,
                format!("event {i}"),
                "tool event body",
                NOW - i as i64,
            );
            m.source_session_ids = vec!["session-f11".to_string()];
            store.write(m).await.unwrap();
        }
        assert!(
            consolidation_due_note(harness(store.clone()).as_ref(), Some("session-f11"))
                .await
                .is_none(),
            "below the threshold the note stays silent"
        );
        // The 15th event crosses it — the note fires with the count.
        let mut m = Memory::new(MemoryTier::Working, "event 14", "tool event body", NOW);
        m.source_session_ids = vec!["session-f11".to_string()];
        store.write(m).await.unwrap();
        let note = consolidation_due_note(harness(store.clone()).as_ref(), Some("session-f11"))
            .await
            .expect("crossing the threshold fires the note");
        assert!(note.contains("15 working-memory events"), "{note}");
        assert!(note.contains("memory_consolidate"), "{note}");
        // Fired once — the gate holds for subsequent calls.
        assert!(
            consolidation_due_note(harness(store).as_ref(), Some("session-f11"))
                .await
                .is_none(),
            "the note fires at most once per session"
        );
    }

    #[tokio::test]
    async fn title_term_overlap_promotes_an_on_point_hit() {
        // F8: the broad query alone ranked an on-point SPEC below generic
        // fresh hits (the limit-3 rider missed it) despite title-term
        // overlap with the plan title. The boost stably promotes
        // title-matching rows into the broad cap.
        let store = make_store();
        // Three generic fresh hits (recency decay ranks the SPEC — the
        // oldest fresh row — last among equal-FTS candidates).
        for (i, age) in [100, 200, 300].iter().enumerate() {
            write(
                &store,
                &format!("PLAN: unrelated digest {i}"),
                "query match needle stacked asks",
                NOW - age,
            )
            .await;
        }
        // The on-point SPEC: oldest fresh hit (ranks 4th), but its TITLE
        // shares the plan title's significant terms.
        write(
            &store,
            "SPEC: expandable memory-search matches",
            "query match needle stacked asks",
            NOW - 400,
        )
        .await;
        let block = recalled_context_block(
            harness(store.clone()).as_ref(),
            Some("Expandable memory-search matches — ship the stacked asks"),
            "expandable memory-search matches stacked asks",
            None,
        )
        .await
        .expect("the title-boosted SPEC rides");
        assert!(
            block.contains("expandable memory-search matches"),
            "title-boosted hit surfaced: {block}"
        );

        // None-title callers (spawn_agent) keep pure rank order.
        let block = recalled_context_block(
            harness(store).as_ref(),
            None,
            "expandable memory-search matches stacked asks",
            None,
        )
        .await
        .expect("rider");
        assert!(
            !block.contains("expandable memory-search matches"),
            "without a title, rank order is untouched: {block}"
        );
    }
}

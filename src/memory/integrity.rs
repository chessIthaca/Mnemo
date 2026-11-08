// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Digest-integrity sweep (backlog c68a8c3b) — the startup safety net for
//! records corrupted at write/index time.
//!
//! The 2027-01-14 session surfaced the failure class: derived digests
//! truncated MID-TOKEN at the byte budget (the ".c" class — a
//! `.coding/…` path reference sliced right after the dot) surfaced in the
//! auto-recall block turn after turn, and self-contradicted records
//! (bodies containing "this claim is NO LONGER TRUE") stayed live at full
//! strength. The sweep scans the recall-eligible rows at startup and
//! QUARANTINES — never deletes — the malformed ones: they drop out of
//! recall, listing and the session primer, and stay retrievable via
//! [`MemoryFilter::include_quarantined`](crate::memory::types::MemoryFilter::include_quarantined)
//! (the same recoverable-history semantics as supersession). Quarantine is
//! reversible by design: the reason string makes every case inspectable,
//! and a false positive costs one `include_quarantined` lookup, not a lost
//! record.

use crate::error::Result;
use crate::memory::{
    Memory, MemoryClass, MemoryFilter, MemoryRecordType, MemoryStoreTrait, MemoryTier, SemanticData,
};

/// Heuristic for the observed corruption class (the ".c"-truncated
/// digests): a gist cut MID-TOKEN at the byte budget, leaving a
/// whitespace-preceded dot + up to 3 word chars dangling at the very end
/// of the content — a `.coding/…`-style path reference sliced right after
/// the introducing dot. A legitimate sentence ends in a COMPLETE dot-file
/// name (4+ chars, e.g. "…via .docs") — the ≤3 window deliberately leaves
/// those alone. 1–3-char dot-files at the very end ("…via .env") land
/// inside the net on purpose: they are rarer than real cuts, and
/// quarantine is recoverable (never a delete), so the net errs toward
/// catching.
fn is_truncated_digest(content: &str) -> bool {
    let trimmed = content.trim_end();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    // Walk back over ≤4 trailing word chars: more than 3 = a complete
    // dot-file name → legitimate.
    let mut i = bytes.len();
    let mut frag = 0usize;
    while i > 0
        && frag < 4
        && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' || bytes[i - 1] == b'-')
    {
        i -= 1;
        frag += 1;
    }
    if frag > 3 {
        return false;
    }
    if i == 0 || bytes[i - 1] != b'.' {
        return false;
    }
    i -= 1;
    if i == 0 {
        return false;
    }
    // The dot must START a token (whitespace before it) — the head of a
    // path like ".coding/…". "main.c" style endings (word chars before
    // the dot) are legitimate file names.
    matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'\r')
}

/// A record whose body self-marks as no-longer-true (e.g. an amendment
/// note "… this claim is NO LONGER TRUE …") — left live it resurfaces
/// forever with a live-looking digest that contradicts itself.
/// Case-insensitive, the observed marker phrase.
fn is_self_marked_stale(content: &str) -> bool {
    content.to_ascii_uppercase().contains("NO LONGER TRUE")
}

/// Scan the store's recall-eligible rows (working-tier and superseded
/// rows are already excluded by the default filter — they never inject
/// anyway) and QUARANTINE the malformed ones. Idempotent: quarantined
/// rows drop out of the scan's default filter, so a re-run finds nothing
/// new. Returns the ids newly quarantined this run (empty = clean store).
pub async fn quarantine_malformed(store: &dyn MemoryStoreTrait) -> Result<Vec<String>> {
    let rows = store.list_filtered(&MemoryFilter::new()).await?;
    let mut quarantined = Vec::new();
    for row in rows {
        let reason = if is_truncated_digest(&row.content) {
            "digest truncated mid-path (the .c class — corrupted at write/index time)"
        } else if is_self_marked_stale(&row.content) {
            "body self-marks as no-longer-true"
        } else {
            continue;
        };
        store.quarantine_memory(&row.id, reason).await?;
        quarantined.push(row.id);
    }
    Ok(quarantined)
}

/// How old a synthetic turn-boundary summary must be before the startup
/// sweep retires it (plan 373dec00 step 2). Thirty days is long past any
/// resume window, so the row has done its bounded-working-tier job.
pub const SYNTHETIC_SUMMARY_MAX_AGE_SECS: i64 = 30 * 86_400;

/// Retire the synthetic turn-boundary summaries older than
/// [`SYNTHETIC_SUMMARY_MAX_AGE_SECS`]: QUARANTINE — never delete — the
/// `"Session involved N tool calls"` episodic rows the compressor writes to
/// bound the working tier (plan 373dec00 step 2). They are not knowledge; at
/// a month old they rank nothing, and `include_quarantined` keeps them
/// recoverable like every other quarantine.
///
/// Fetched through the explicit episodic-tier browse: the default listing now
/// hides synthetic rows, and that browse is the one deliberate way back to
/// them. It carries no limit (`MemoryFilter::new()` sets none), so the sweep
/// sees every episodic row. Idempotent — quarantined rows drop out of the
/// browse's default filter.
pub async fn quarantine_old_synthetic(store: &dyn MemoryStoreTrait) -> Result<Vec<String>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cutoff = now - SYNTHETIC_SUMMARY_MAX_AGE_SECS;
    let rows = store
        .list_filtered(&MemoryFilter::new().tier(MemoryTier::Episodic))
        .await?;
    let mut quarantined = Vec::new();
    for row in rows {
        if row.tier == MemoryTier::Episodic
            && row.content.starts_with(crate::memory::SYNTHETIC_SUMMARY_PREFIX)
            && row.created_at < cutoff
        {
            store
                .quarantine_memory(&row.id, "synthetic turn-boundary summary older than 30 days")
                .await?;
            quarantined.push(row.id);
        }
    }
    Ok(quarantined)
}

/// The literal phrases that restate git or plan state (plan 373dec00 step 3),
/// all observed in the live store's auto-extracted facts. Lowercase: the
/// matcher lowercases the body once (see [`restates_repo_state`]).
const STATE_PHRASES: &[&str] = &["was committed", "committed as", "merged into", "was completed"];

/// Does `haystack` contain `phrase` ending on a word boundary — so
/// "on branches" and "was completedness" never match?
fn contains_phrase(haystack: &str, phrase: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(phrase) {
        let end = from + offset + phrase.len();
        let boundary = match haystack[end..].chars().next() {
            Some(c) => !c.is_ascii_alphanumeric(),
            None => true,
        };
        if boundary {
            return true;
        }
        from = end;
    }
    false
}

/// `on branch <name>` — the branch a commit landed on. The name must be
/// backticked ("on branch `wt/agent-storeUi0`") or branch-shaped itself —
/// `main` / `master` / `develop` / `trunk`, or any name containing a `/` — so
/// an ordinary sentence ("tests focus on branch coverage") never matches.
fn has_on_branch(text: &str) -> bool {
    const KNOWN_BRANCHES: &[&str] = &["main", "master", "develop", "trunk"];
    let mut from = 0;
    while let Some(offset) = text[from..].find("on branch") {
        let rest = text[from + offset + "on branch".len()..].trim_start();
        let token_end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | ')' | ']'))
            .unwrap_or(rest.len());
        let token = &rest[..token_end];
        let name = token.trim_matches('`');
        if !name.is_empty()
            && (token.starts_with('`')
                || name.contains('/')
                || KNOWN_BRANCHES.contains(&name))
        {
            return true;
        }
        from += offset + "on branch".len();
    }
    false
}

/// Is `token` a nonempty run of ASCII hex digits (the commit/plan-id shape)?
fn is_hex_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A bare 7-to-40-char hex hash between backticks — a commit id
/// ("committed as `890e3a57`"). Six or forty-one hex chars are not a hash.
fn has_backticked_hash(text: &str) -> bool {
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else {
            return false;
        };
        let token = &after[..close];
        if (7..=40).contains(&token.len()) && is_hex_token(token) {
            return true;
        }
        rest = &after[close + 1..];
    }
    false
}

/// `plan <8 hex>` — a plan id exactly 8 hex chars long, backticked
/// ("plan `fe45dea4`") or bare ("plan fe45dea4"). The word must stand alone
/// (not "planner") and be followed by a single space.
fn has_plan_id(text: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = text[from..].find("plan") {
        let start = from + offset;
        let before_ok = match text[..start].chars().next_back() {
            Some(c) => !c.is_ascii_alphanumeric(),
            None => true,
        };
        if before_ok {
            if let Some(after_space) = text[start + 4..].strip_prefix(' ') {
                let token = if let Some(quoted) = after_space.strip_prefix('`') {
                    quoted.find('`').map(|end| &quoted[..end])
                } else {
                    let end = after_space
                        .find(|c: char| !c.is_ascii_hexdigit())
                        .unwrap_or(after_space.len());
                    Some(&after_space[..end])
                };
                if let Some(token) = token {
                    if token.len() == 8 && is_hex_token(token) {
                        return true;
                    }
                }
            }
        }
        from = start + 4;
    }
    false
}

/// `step N of` / `step N was` — plan-step progress ("Step 2 of plan
/// `fe45dea4` was completed", "step 11 was implemented"). A plural "steps",
/// or a following word that continues ("step 2 offering"), never matches.
fn has_step_marker(text: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = text[from..].find("step") {
        let start = from + offset;
        let before_ok = match text[..start].chars().next_back() {
            Some(c) => !c.is_ascii_alphanumeric(),
            None => true,
        };
        if before_ok {
            if let Some(after_space) = text[start + 4..].strip_prefix(' ') {
                let digits = after_space
                    .bytes()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                if digits > 0 {
                    let tail = &after_space[digits..];
                    for marker in [" of", " was"] {
                        if let Some(rest) = tail.strip_prefix(marker) {
                            let boundary = match rest.chars().next() {
                                Some(c) => !c.is_ascii_alphanumeric(),
                                None => true,
                            };
                            if boundary {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        from = start + 4;
    }
    false
}

/// Does `text` restate repository state that git, the plan board, or the
/// knowledge files already show? (plan 373dec00 step 3)
///
/// The shapes, all observed in the live store's auto-extracted facts
/// (case-insensitive):
///
/// - `was committed` / `committed as` — a commit restated;
/// - a bare 7-to-40-char hex hash in backticks — a commit id;
/// - `on branch <name>` — the branch a commit landed on (the name must be
///   backticked or branch-shaped: `main`/`master`/`develop`/`trunk`, or any
///   name containing a `/`);
/// - `wt/agent-` — a worker branch name;
/// - `plan <8 hex>` — a plan id;
/// - `step N of` / `step N was` — plan-step progress;
/// - `merged into` — a merge result;
/// - `was completed` — plan-step completion.
///
/// Deliberately narrow: the literal phrases end on a word boundary, the hash
/// shape needs backticks and a hash-length token, and a plan id is exactly 8
/// hex chars after the standalone word `plan` — so an untyped root cause in
/// ordinary prose ("the draft was cleared before the reply landed") is left
/// alone. A root cause that *quotes* a commit id in backticks is still
/// caught by the hash shape; that is the one trade-off the step's sample
/// check audits (a wrong call costs one `include_quarantined` lookup, never
/// a lost record).
pub fn restates_repo_state(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    STATE_PHRASES
        .iter()
        .any(|phrase| contains_phrase(&lower, phrase))
        || lower.contains("wt/agent-")
        || has_on_branch(&lower)
        || has_backticked_hash(&lower)
        || has_plan_id(&lower)
        || has_step_marker(&lower)
}

/// Does the row's semantic payload already record how the fact is known
/// ([`SemanticData::how_known`])? A fact whose evidence is stated — a test, a
/// file, a run — is knowledge, never a bare state restatement.
fn has_how_known(row: &Memory) -> bool {
    serde_json::from_value::<SemanticData>(row.data.clone())
        .ok()
        .and_then(|data| data.how_known)
        .is_some_and(|how| !how.trim().is_empty())
}

/// The full step-3 predicate: ALL of
///
/// * `tier` semantic — only distilled facts are in scope;
/// * class authored — knowledge-file rows are DERIVED and never match (they
///   are re-derived from their file, so the file stays the truth);
/// * no typed title prefix — `SPEC:`/`DECISION:`/`BUG:`/`PLAN:`/`HOW:`/
///   `REVIEW:` records are pointers to on-disk truth, whatever their body
///   restates (the stored column and the title are checked together);
/// * `access_count == 0` — an explicit memory_search never returned it (the
///   auto-recall and primer peek paths deliberately do not bump it), so
///   nobody has actually asked for this row;
/// * no `how_known` — stated evidence makes it knowledge;
/// * the body matches [`restates_repo_state`].
fn is_git_state_fact(row: &Memory) -> bool {
    row.tier == MemoryTier::Semantic
        && row.record_class == MemoryClass::Authored
        && row.record_type == MemoryRecordType::None
        && MemoryRecordType::from_title(&row.title) == MemoryRecordType::None
        && row.access_count == 0
        && !has_how_known(row)
        && restates_repo_state(&row.content)
}

/// Retire the existing facts that only restate git or plan state (plan
/// 373dec00 step 3): the extractor used to record what a commit or the plan
/// board already shows ("Step 2 of plan `fe45dea4` was completed on branch
/// `wt/agent-storeUi0` and committed as `890e3a57`"), which is re-derivable
/// from the repository and never was knowledge.
///
/// QUARANTINE — never delete — under the same recoverable terms as the other
/// startup sweeps: `include_quarantined` keeps every row and its reason.
/// Typed records, derived (knowledge-file) rows, searched rows and rows with
/// stated evidence are untouched (see [`is_git_state_fact`]). Idempotent:
/// quarantined rows drop out of the scan's default filter, so a re-run finds
/// nothing new. Returns the ids newly quarantined this run.
pub async fn quarantine_git_state_facts(store: &dyn MemoryStoreTrait) -> Result<Vec<String>> {
    let rows = store
        .list_filtered(&MemoryFilter::new().tier(MemoryTier::Semantic))
        .await?;
    let mut quarantined = Vec::new();
    for row in rows {
        if is_git_state_fact(&row) {
            store
                .quarantine_memory(
                    &row.id,
                    "fact restates git or plan state the repository already shows",
                )
                .await?;
            quarantined.push(row.id);
        }
    }
    Ok(quarantined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryStore, MemoryTier};
    use std::sync::Arc;

    fn store() -> Arc<MemoryStore> {
        let embedder: Arc<dyn crate::memory::Embedder> =
            Arc::new(crate::memory::embedder::HashEmbedder::new());
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
    }

    #[test]
    fn is_truncated_digest_matches_the_observed_corruption_class() {
        // The ".c"-truncated class (observed 2027-01-14): a gist cut
        // MID-TOKEN at the byte budget, right after a path-introducing
        // dot — leaving the dot + up to 3 word chars dangling at the
        // very end with no terminator.
        assert!(is_truncated_digest(
            "PLAN: ship the merge — the full rationale lives in .c"
        ));
        assert!(is_truncated_digest("the design doc is at ."));
        assert!(is_truncated_digest("the design doc is at .co"));
        assert!(is_truncated_digest("the design doc is at .cod"));
        // A COMPLETE dot-file name at the end (4+ chars) is legitimate —
        // outside the net. 1-3-char dot-files at the very end
        // ("…via .env") are deliberately INSIDE the net: rarer than real
        // cuts, and quarantine is recoverable (never a delete), so the
        // net errs toward catching.
        assert!(!is_truncated_digest("the endpoint keys live in .docs"));
        assert!(!is_truncated_digest("see PLAN.md for the design"));
        assert!(!is_truncated_digest(
            "merge instructions: rebase, test, land through the approved path"
        ));
        assert!(!is_truncated_digest(""));
    }

    #[test]
    fn is_self_marked_stale_matches_case_insensitive() {
        assert!(is_self_marked_stale(
            "the old trigger formula; this claim is no longer true — see the successor record"
        ));
        assert!(is_self_marked_stale("THIS CLAIM IS No Longer TRUE"));
        assert!(!is_self_marked_stale("still true, still live"));
    }

    #[tokio::test]
    async fn sweep_quarantines_malformed_rows_never_deletes_and_recall_excludes() {
        // RED first (backlog c68a8c3b): pre-fix, quarantine_memory writes
        // the flag but the recall/list/primer SQL ignores it — the
        // malformed rows still inject.
        let store = store();
        let bad_digest = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "PLAN: merge dial",
                "PLAN: merge dial — the rationale lives in .c",
                1_700_000_000,
            ))
            .await
            .unwrap();
        let bad_body = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "DECISION: old trigger",
                "the trigger caps at the ceiling; this claim is NO LONGER TRUE",
                1_700_000_100,
            ))
            .await
            .unwrap();
        let good = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "merge instructions",
                "how to merge feature branches into main safely: rebase, run the suite, land through the approved path",
                1_700_000_200,
            ))
            .await
            .unwrap();

        let quarantined = quarantine_malformed(store.as_ref()).await.unwrap();
        assert_eq!(quarantined.len(), 2, "exactly the two malformed rows");
        assert!(quarantined.contains(&bad_digest));
        assert!(quarantined.contains(&bad_body));

        // Quarantine NEVER deletes: the rows exist with their reasons.
        let flagged = store.get_memory(&bad_digest).await.unwrap().unwrap();
        assert!(
            flagged.quarantined.as_deref().unwrap().contains(".c"),
            "the truncation reason is recorded: {:?}",
            flagged.quarantined
        );
        let flagged = store.get_memory(&bad_body).await.unwrap().unwrap();
        assert!(
            flagged
                .quarantined
                .as_deref()
                .unwrap()
                .contains("no-longer-true"),
            "the self-marked reason is recorded: {:?}",
            flagged.quarantined
        );

        // Recall NEVER returns a quarantined row (the RED assert
        // pre-fix); the good row still injects where the query matches it.
        for (query, expect_good) in [("merge dial", true), ("trigger", false), ("merge", true)] {
            let hits = store
                .recall_peek(query, &MemoryFilter::new().limit(10))
                .await
                .unwrap();
            assert!(
                hits.iter()
                    .all(|sm| sm.memory.id != bad_digest && sm.memory.id != bad_body),
                "quarantined rows never inject (query {query:?}): {hits:?}"
            );
            if expect_good {
                assert!(
                    hits.iter().any(|sm| sm.memory.id == good),
                    "the good row still injects (query {query:?}): {hits:?}"
                );
            }
        }

        // The default listing excludes them; include_quarantined
        // retrieves history (the recovery path — never deleted).
        let default_list = store.list_filtered(&MemoryFilter::new()).await.unwrap();
        assert!(
            default_list
                .iter()
                .all(|m| m.id != bad_digest && m.id != bad_body),
            "the default listing excludes quarantined rows"
        );
        let full_list = store
            .list_filtered(&MemoryFilter::new().include_quarantined())
            .await
            .unwrap();
        assert!(full_list.iter().any(|m| m.id == bad_digest));
        assert!(full_list.iter().any(|m| m.id == bad_body));
        assert!(full_list.iter().any(|m| m.id == good));

        // Idempotent: a re-run finds nothing new (quarantined rows are
        // excluded from the scan by the default filter).
        let again = quarantine_malformed(store.as_ref()).await.unwrap();
        assert!(again.is_empty(), "idempotent: {again:?}");
    }

    #[tokio::test]
    async fn the_sweep_quarantines_old_synthetic_rows_only() {
        // Plan 373dec00 step 2: the startup sweep retires synthetic
        // turn-boundary summaries after 30 days — only rows carrying the
        // compressor's content prefix, only on the episodic tier, only when
        // old, and always as a recoverable quarantine.
        let store = store();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap();
        let old = now - SYNTHETIC_SUMMARY_MAX_AGE_SECS - 86_400;
        let synthetic_old = store
            .write(Memory::new(
                MemoryTier::Episodic,
                "session old summary",
                "Session involved 2 tool calls across tools: file_read.",
                old,
            ))
            .await
            .unwrap();
        let synthetic_fresh = store
            .write(Memory::new(
                MemoryTier::Episodic,
                "session fresh summary",
                "Session involved 1 tool call across tools: file_write.",
                now - 86_400,
            ))
            .await
            .unwrap();
        let episodic_old = store
            .write(Memory::new(
                MemoryTier::Episodic,
                "session decision",
                "We decided the login route moves into the router module.",
                old,
            ))
            .await
            .unwrap();
        let semantic_old = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "DECISION: quoted call line",
                "Session involved 2 tool calls — a quote inside a real decision record.",
                old,
            ))
            .await
            .unwrap();

        let quarantined = quarantine_old_synthetic(store.as_ref()).await.unwrap();
        assert_eq!(
            quarantined,
            vec![synthetic_old.clone()],
            "exactly the old synthetic row"
        );
        let flagged = store.get_memory(&synthetic_old).await.unwrap().unwrap();
        let reason = flagged.quarantined.as_deref().unwrap_or_default();
        assert!(
            reason.contains("synthetic") && reason.contains("30 days"),
            "the reason says what happened: {reason:?}"
        );

        // Quarantine is recoverable, never a delete.
        let full = store
            .list_filtered(
                &MemoryFilter::new()
                    .include_quarantined()
                    .tier(MemoryTier::Episodic),
            )
            .await
            .unwrap();
        assert!(full.iter().any(|m| m.id == synthetic_old), "recoverable");
        let default_list = store.list_filtered(&MemoryFilter::new()).await.unwrap();
        assert!(
            default_list.iter().all(|m| m.id != synthetic_old),
            "quarantined rows leave the default listing"
        );

        // The other rows are untouched.
        for id in [&synthetic_fresh, &episodic_old, &semantic_old] {
            let row = store.get_memory(id).await.unwrap().unwrap();
            assert!(
                row.quarantined.is_none(),
                "a fresh, non-synthetic, or non-episodic row is never swept: {row:?}"
            );
        }

        // Idempotent: the next startup finds nothing left.
        let again = quarantine_old_synthetic(store.as_ref()).await.unwrap();
        assert!(again.is_empty(), "idempotent: {again:?}");
    }

    /// The content shape the live extractor produced for a finished plan
    /// step (observed 2026-11-08) — the row step 3 retires.
    const GIT_STATE_BODY: &str = "Step 2 of plan `fe45dea4` was completed on branch \
                                  `wt/agent-storeUi0` and committed as `890e3a57`.";

    #[tokio::test]
    async fn a_git_state_fact_is_quarantined() {
        // Plan 373dec00 step 3: a fact that only restates what git or the
        // plan board already shows is retired — recoverably, like every
        // other quarantine.
        let store = store();
        let state = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "Step 2 of plan `fe45dea4` was completed on branch `wt/agent-storeUi0`",
                GIT_STATE_BODY,
                1_700_000_000,
            ))
            .await
            .unwrap();

        let quarantined = quarantine_git_state_facts(store.as_ref()).await.unwrap();
        assert_eq!(quarantined, vec![state.clone()], "exactly the state row");

        // Quarantine never deletes — the row and its reason are still there.
        let flagged = store.get_memory(&state).await.unwrap().unwrap();
        let reason = flagged.quarantined.as_deref().unwrap_or_default();
        assert!(
            reason.contains("git or plan state"),
            "the reason says what happened: {reason:?}"
        );

        // Recall and the default listing stop returning it.
        let hits = store
            .recall_peek("Step 2 of plan fe45dea4", &MemoryFilter::new().limit(10))
            .await
            .unwrap();
        assert!(
            hits.iter().all(|sm| sm.memory.id != state),
            "a quarantined row never injects: {hits:?}"
        );
        let default_list = store.list_filtered(&MemoryFilter::new()).await.unwrap();
        assert!(default_list.iter().all(|m| m.id != state));

        // `include_quarantined` still finds it: recoverable history, never a
        // delete.
        let full = store
            .list_filtered(&MemoryFilter::new().include_quarantined())
            .await
            .unwrap();
        assert!(
            full.iter().any(|m| m.id == state),
            "recoverable, never deleted"
        );

        // Idempotent: a re-run finds nothing new.
        let again = quarantine_git_state_facts(store.as_ref()).await.unwrap();
        assert!(again.is_empty(), "idempotent: {again:?}");
    }

    #[tokio::test]
    async fn a_fact_that_was_searched_is_kept() {
        // `access_count > 0` means an explicit memory_search returned the
        // row: the agent asked for it, so it is not inert state noise. (The
        // auto-recall and primer peek paths deliberately do not bump the
        // count, so this stays a real "someone searched this" signal.)
        let store = store();
        let searched = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "Step 2 of plan `fe45dea4` was completed on branch `wt/agent-storeUi0`",
                GIT_STATE_BODY,
                1_700_000_000,
            ))
            .await
            .unwrap();
        store.access(&searched).await.unwrap();

        let quarantined = quarantine_git_state_facts(store.as_ref()).await.unwrap();
        assert!(quarantined.is_empty(), "searched rows stay: {quarantined:?}");
        let row = store.get_memory(&searched).await.unwrap().unwrap();
        assert!(row.quarantined.is_none(), "kept: {row:?}");
    }

    #[tokio::test]
    async fn a_fact_with_how_known_is_kept() {
        // A fact that states its evidence (plan 373dec00 step 4's field) is
        // knowledge, even when its wording also mentions a commit: the
        // `how_known` field means someone established it.
        let store = store();
        let mut memory = Memory::new(
            MemoryTier::Semantic,
            "Step 2 of plan `fe45dea4` was completed",
            GIT_STATE_BODY,
            1_700_000_000,
        );
        memory.data = serde_json::to_value(SemanticData {
            fact: GIT_STATE_BODY.to_string(),
            confidence: 0.9,
            how_known: Some("checked: git log --oneline shows the step-2 commit".to_string()),
        })
        .unwrap();
        let id = store.write(memory).await.unwrap();

        let quarantined = quarantine_git_state_facts(store.as_ref()).await.unwrap();
        assert!(quarantined.is_empty(), "evidence stays: {quarantined:?}");
        let row = store.get_memory(&id).await.unwrap().unwrap();
        assert!(row.quarantined.is_none(), "kept: {row:?}");
    }

    #[tokio::test]
    async fn a_typed_record_is_never_touched() {
        // Typed records are pointers to on-disk truth whatever their body
        // restates — the prefix wins. Derived knowledge-file rows are the
        // other half: the file is the truth and the row is re-derived, so a
        // derived row never matches even with no prefix.
        let store = store();
        let bug = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "BUG: routing gate used the margin score",
                "The routing gate keyed on the margin instead of the top probability; \
                 fixed and committed as `9cb3eac5` on branch `wt/agent-layaConf`.",
                1_700_000_000,
            ))
            .await
            .unwrap();
        let mut derived = Memory::new(
            MemoryTier::Semantic,
            "knowledge digest without a typed prefix",
            GIT_STATE_BODY,
            1_700_000_100,
        );
        derived.record_class = MemoryClass::Derived;
        let derived_id = store.write(derived).await.unwrap();

        let quarantined = quarantine_git_state_facts(store.as_ref()).await.unwrap();
        assert!(
            quarantined.is_empty(),
            "typed and derived rows are untouched: {quarantined:?}"
        );
        for id in [&bug, &derived_id] {
            let row = store.get_memory(id).await.unwrap().unwrap();
            assert!(row.quarantined.is_none(), "kept: {row:?}");
        }
    }

    #[tokio::test]
    async fn a_root_cause_without_state_words_is_kept() {
        // The failure mode the matcher must never hit: an untyped root cause
        // in ordinary prose. The cause is not in git; quarantine must leave
        // it live.
        let store = store();
        let id = store
            .write(Memory::new(
                MemoryTier::Semantic,
                "sheet errors vanish when the answer lands",
                "Root cause: answering cleared the sheet's draft state before the reply \
                 landed, so the edit form re-rendered empty. The fix keeps the draft \
                 until the reply is applied.",
                1_700_000_000,
            ))
            .await
            .unwrap();

        let quarantined = quarantine_git_state_facts(store.as_ref()).await.unwrap();
        assert!(quarantined.is_empty(), "the cause stays live: {quarantined:?}");
        let row = store.get_memory(&id).await.unwrap().unwrap();
        assert!(row.quarantined.is_none(), "kept: {row:?}");
    }

    #[test]
    fn restates_repo_state_matches_the_listed_shapes() {
        // Every shape the step lists, in the wording the live store uses.
        assert!(restates_repo_state(
            "Step 2 of plan `fe45dea4` was completed on branch `wt/agent-storeUi0` \
             and committed as `890e3a57`."
        ));
        // The shapes one at a time.
        assert!(restates_repo_state("The routing fix was committed yesterday."));
        assert!(restates_repo_state("It landed, committed as a pure move."));
        assert!(restates_repo_state("The change is `890e3a57`.")); // 8-hex commit id
        assert!(restates_repo_state(&format!("The tree is `{}`.", "a".repeat(40))));
        assert!(restates_repo_state("It landed on branch main."));
        assert!(restates_repo_state("Pushed to wt/agent-storeUi0."));
        assert!(restates_repo_state("Step 2 of plan `fe45dea4` was completed."));
        assert!(restates_repo_state("plan fe45dea4")); // bare 8-hex plan id
        assert!(restates_repo_state("step 11 was implemented"));
        assert!(restates_repo_state("The worker was merged into the lead branch."));
        assert!(restates_repo_state("The migration was completed."));
        // Case-insensitive, like the store's extractor output.
        assert!(restates_repo_state("STEP 2 OF PLAN `FE45DEA4` WAS COMPLETED"));

        // Near-misses stay out: a word boundary, a non-hash token, a
        // too-short or too-long hex token, a plural, and a plan id that is
        // not exactly 8 hex chars.
        assert!(!restates_repo_state(""));
        assert!(!restates_repo_state("Tests focus on branch coverage."));
        assert!(!restates_repo_state("The rule fires on branch selection."));
        assert!(!restates_repo_state("The suite was completedness-adjacent."));
        assert!(!restates_repo_state("Run `cargo test` before landing."));
        assert!(!restates_repo_state("The id is `abcdef`.")); // 6 hex chars
        assert!(!restates_repo_state(&format!("The id is `{}`.", "a".repeat(41))));
        assert!(!restates_repo_state("steps 2 of the migration"));
        assert!(!restates_repo_state("The planner 20260101 ran."));
        assert!(!restates_repo_state(
            "Root cause: the draft state was cleared before the reply landed."
        ));
    }
}
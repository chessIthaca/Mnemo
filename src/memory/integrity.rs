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
use crate::memory::{MemoryFilter, MemoryStoreTrait};

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
}
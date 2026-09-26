// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Recall-delta cache — the `recall_delta` token-optimizer lever (backlog
//! 30bacfa2).
//!
//! Auto-recall re-renders its memory block into the volatile tail on every
//! loop iteration, and the tail is popped from the conversation right after
//! each request (see `install_system_messages` in `turn.rs`), so a memory that
//! was already surfaced is re-sent as its full ~160-char snippet every single
//! time. This cache remembers what the tail last injected, so a repeat can be
//! rendered as a compact reference instead.
//!
//! The identity of an injection is `(memory id, content hash)` — never the id
//! alone, so an `update`/`supersede` between two recalls changes the hash and
//! forces a full re-injection. [`RecallDeltaCache::invalidate`] retires every
//! entry at once when the earlier injection leaves the model's context
//! (compaction rewrote the conversation): a reference to text that is nowhere
//! in the request is worse than no reference at all.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// What the volatile tail should render for one recalled memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeltaDecision {
    /// First sighting in this epoch, changed content, or an injection that
    /// predates a compaction: render the entry in full. The cache records it
    /// so a later repeat can be elided.
    Full,
    /// This exact memory (same id AND same content hash, same epoch) was
    /// already injected in full — render the compact reference, stamped with
    /// the turn whose request carried the full snippet.
    Elide {
        /// The session turn whose request carried the full snippet.
        injected_turn: u64,
    },
}

/// One memory's most recent full injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecallEntry {
    /// Hash of the rendered text at injection time (see [`content_hash`]).
    content_hash: u64,
    /// The cache epoch that injection belonged to.
    epoch: u64,
    /// The session turn the full snippet was rendered in.
    full_turn: u64,
}

/// Per-session recall-delta state: what the volatile tail has already
/// injected, so a repeat recall renders as a reference instead of a snippet.
///
/// Lives in `SessionState` (loop_impl.rs), so it survives a whole session
/// while the per-turn `TurnState` is rebuilt for every `run_turn`.
#[derive(Debug, Default)]
pub(crate) struct RecallDeltaCache {
    /// Memory id → its last full injection.
    entries: HashMap<String, RecallEntry>,
    /// Session turn counter, bumped once per turn by
    /// [`begin_turn`](Self::begin_turn).
    turn: u64,
    /// Bumped by [`invalidate`](Self::invalidate); an entry only elides while
    /// its epoch still matches.
    epoch: u64,
}

impl RecallDeltaCache {
    /// Advance to the next turn — called once at the top of `run_turn`, so the
    /// stamp an entry carries means "the Nth turn of this session".
    pub(crate) fn begin_turn(&mut self) {
        self.turn = self.turn.saturating_add(1);
    }

    /// Retire every prior injection: the text the tail sent is no longer in
    /// the model's context (compaction rewrote the conversation), so the next
    /// recall must render in full again.
    pub(crate) fn invalidate(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.entries.clear();
    }

    /// Classify one recalled memory for rendering.
    ///
    /// Returns [`DeltaDecision::Elide`] only when this exact id was injected
    /// with this exact content hash in the current epoch; every other case
    /// records (or refreshes) the entry and returns [`DeltaDecision::Full`].
    pub(crate) fn decide(&mut self, id: &str, hash: u64) -> DeltaDecision {
        if let Some(entry) = self.entries.get(id) {
            if entry.content_hash == hash && entry.epoch == self.epoch {
                return DeltaDecision::Elide {
                    injected_turn: entry.full_turn,
                };
            }
        }
        self.entries.insert(
            id.to_string(),
            RecallEntry {
                content_hash: hash,
                epoch: self.epoch,
                full_turn: self.turn,
            },
        );
        DeltaDecision::Full
    }
}

/// Hash the parts of a recalled memory its rendered entry shows, so a content
/// change between two recalls is detected even when the id is unchanged.
pub(crate) fn content_hash(tier: &str, title: &str, content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    tier.hash(&mut hasher);
    title.hash(&mut hasher);
    content.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sighting_is_full_and_the_repeat_elides_with_the_turn_stamp() {
        let mut cache = RecallDeltaCache::default();
        cache.begin_turn();
        let h = content_hash("semantic", "Title", "body");
        assert_eq!(cache.decide("id-1", h), DeltaDecision::Full);
        cache.begin_turn();
        assert_eq!(
            cache.decide("id-1", h),
            DeltaDecision::Elide { injected_turn: 1 }
        );
    }

    #[test]
    fn a_changed_content_hash_forces_a_full_reinjection() {
        let mut cache = RecallDeltaCache::default();
        cache.begin_turn();
        assert_eq!(
            cache.decide("id-1", content_hash("semantic", "Title", "old body")),
            DeltaDecision::Full
        );
        cache.begin_turn();
        let updated = content_hash("semantic", "Title", "new body");
        assert_eq!(cache.decide("id-1", updated), DeltaDecision::Full);
        cache.begin_turn();
        assert_eq!(
            cache.decide("id-1", updated),
            DeltaDecision::Elide { injected_turn: 2 }
        );
    }

    #[test]
    fn invalidate_retires_every_entry() {
        let mut cache = RecallDeltaCache::default();
        cache.begin_turn();
        let h = content_hash("semantic", "Title", "body");
        assert_eq!(cache.decide("id-1", h), DeltaDecision::Full);
        cache.invalidate();
        cache.begin_turn();
        assert_eq!(cache.decide("id-1", h), DeltaDecision::Full);
    }

    #[test]
    fn ids_are_independent() {
        let mut cache = RecallDeltaCache::default();
        cache.begin_turn();
        assert_eq!(cache.decide("a", 7), DeltaDecision::Full);
        assert_eq!(cache.decide("b", 7), DeltaDecision::Full);
        assert_eq!(cache.decide("a", 7), DeltaDecision::Elide { injected_turn: 1 });
        assert_eq!(cache.decide("b", 7), DeltaDecision::Elide { injected_turn: 1 });
    }

    #[test]
    fn content_hash_covers_tier_title_and_body() {
        let base = content_hash("semantic", "Title", "body");
        assert_ne!(base, content_hash("procedural", "Title", "body"));
        assert_ne!(base, content_hash("semantic", "Other", "body"));
        assert_ne!(base, content_hash("semantic", "Title", "other body"));
        assert_eq!(base, content_hash("semantic", "Title", "body"));
    }
}

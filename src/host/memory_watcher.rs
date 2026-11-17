// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! OS file-watcher for the derived memory index — keeps `memory_search` fresh
//! on every `.coding/` write.
//!
//! [`crate::memory::indexer::index_derived`] is incremental (content-hash
//! gated) but otherwise runs only at startup (the brain's reconcile) and on a
//! manual "Rebuild derived index". A source written any other way — a hand
//! edit, a `git merge`, a plan tool in a second process, an agent writing
//! OpenSpec specs — then waited for the next app start. This module closes
//! that gap with an OS-level file watcher
//! ([`notify::RecommendedWatcher`]) over the project's `.coding/` directory:
//! any create/modify/remove of a source the derived index covers marks the
//! corpus dirty, and a short debounce after the last event of a burst runs ONE
//! reconcile — the same cheap [`crate::memory::indexer::corpus_is_stale`]
//! pre-check the startup pass uses, then `index_derived`.
//!
//! It is deliberately a SEPARATE watcher from
//! [`crate::codegraph::watcher`]: the code graph can be disabled while memory
//! is on, and the code graph's pass refreshes the graph plus the text content
//! index, never the memory store.
//!
//! Three properties are load-bearing:
//!
//! * **Silent.** A live pass sends NO `HostEvent::Reconcile`. In Mnemo1 those
//!   events open the full-screen reconcile splash dialog — it must not pop up
//!   on every plan edit. One log line when a pass indexed or removed
//!   something, one warning on failure. The startup reconcile and its events
//!   are unchanged.
//! * **Serialized.** The pass holds the process-wide maintenance guard
//!   ([`crate::host::maintenance::claim_busy`] / `BusyGuard`) — the same one
//!   the startup reconcile and the manual "Rebuild derived index" hold — so it
//!   never interleaves with them. A change that lands while one of those runs
//!   waits its turn instead of being dropped.
//! * **Narrow.** Only source kinds the derived index actually reads trigger a
//!   pass ([`is_memory_source_path`]): plans, reviews, knowledge records,
//!   OpenSpec current-state specs, and the backlog. The indexer's own outputs
//!   (`knowledge/INDEX.md`, `memory.db*`) stay unwatched — reacting to them
//!   would loop forever.
//!
//! Best-effort throughout: a watcher that fails to start, a burst racing a
//! running maintenance operation, or a reconcile error are logged and skipped
//! — the index stays one pass behind, never a fatal error.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc as tokio_mpsc;

use crate::memory::MemoryStoreTrait;

/// Whether `path` is a source the derived memory index reads — i.e. a
/// `.coding/` write that must trigger a live reconcile pass.
///
/// `state_dir` is the project's `.coding/` directory. Accepted:
///
/// * `plans/*.md`, `reviews/*.md` (and an extension-less name — a removed
///   file or directory arrives without one),
/// * `knowledge/<type>/<file>.md` at depth >= 2 (the corpus layout; a
///   non-dotfile `.md`),
/// * anything under `openspec/specs/` at any depth (a removed capability
///   folder must count, not just its `spec.md`),
/// * `backlog.jsonl` exactly.
///
/// Everything else is rejected, in particular `knowledge/INDEX.md`,
/// `knowledge/HEALTH.md` and the dotfiles beside them, the app's own SQLite
/// store family (`memory.db*`, `codegraph.db*`), `logs/`, `tmp/`,
/// `openspec/changes/**` (archived deltas are deliberately not current-state
/// sources), `openspec/config.yaml`, `openspec/schemas/**`, `board.json`, and
/// `laya/**`. Those are written BY the indexer or are not index material at
/// all; reacting to them would re-index forever.
///
/// Symlink-tolerant the same way [`crate::codegraph::watcher`] is: FSEvents
/// (macOS) delivers event paths with symlinks resolved, so a state dir under a
/// symlinked path (every macOS tempdir: `/var` → `/private/var`) never
/// prefix-matches in its raw form — the canonicalized form is retried.
pub(crate) fn is_memory_source_path(path: &Path, state_dir: &Path) -> bool {
    fn under_base(path: &Path, state_dir: &Path) -> bool {
        let rel = match path.strip_prefix(state_dir) {
            Ok(rel) => rel,
            Err(_) => return false, // outside the state dir — never react
        };
        // Component-wise on purpose: Windows paths use backslashes, so string
        // slicing on '/' would mis-split (or match nothing).
        let parts: Vec<String> = rel
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let Some(first) = parts.first().map(String::as_str) else {
            return false; // the state dir itself
        };
        match first {
            // Plans and reviews: `.md` files, plus an extension-less name for
            // the removed-file / removed-directory event (a removal must
            // trigger, or the row would outlive its file).
            "plans" | "reviews" => {
                parts.len() >= 2
                    && match path.extension() {
                        None => true,
                        Some(ext) => ext.eq_ignore_ascii_case("md"),
                    }
            }
            // The knowledge corpus: `knowledge/<type>/<file>.md`. The indexer
            // (and the health pass) writes `knowledge/INDEX.md`,
            // `knowledge/HEALTH.md` and dotfiles directly in `knowledge/`;
            // those must never trigger, hence the depth and dotfile guards.
            "knowledge" => {
                if parts.len() < 3 {
                    return false;
                }
                let name = parts.last().map(String::as_str).unwrap_or_default();
                !name.starts_with('.')
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            }
            // OpenSpec current-state specs. Any depth so a removed capability
            // folder (whose `spec.md` is already gone) still counts; archived
            // changes and the config/schema files are a different subtree.
            "openspec" => parts.get(1).is_some_and(|second| second == "specs"),
            "backlog.jsonl" => parts.len() == 1,
            _ => false,
        }
    }
    if under_base(path, state_dir) {
        return true;
    }
    // FSEvents (macOS) delivers event paths with every symlink resolved — a
    // watched state dir that itself sits under a symlink then never
    // prefix-matches, and every event would be filtered out (the defect the
    // codegraph watcher hit on the macOS CI leg). Retry against the canonical
    // form; on Windows the raw prefix already matched.
    match std::fs::canonicalize(state_dir) {
        Ok(canonical) => canonical.as_path() != state_dir && under_base(path, &canonical),
        Err(_) => false,
    }
}

/// Whether an event kind represents a content change worth reconciling for
/// (create / modify / remove). Metadata-only and access events are ignored —
/// they cannot change the corpus. A private copy of the codegraph watcher's
/// rule: the two watchers stay independent.
fn is_change_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// A running memory-index file watcher. Holds the OS watcher alive — dropping
/// this struct stops watching. Construct with [`MemoryWatcher::spawn`].
pub struct MemoryWatcher {
    /// The OS watcher. Held only to keep the watch alive; events arrive on the
    /// channel consumed by the spawned debounce task.
    _watcher: RecommendedWatcher,
    /// Handle to the debounce/reconcile task. `Some` when the caller's Tokio
    /// runtime spawned it (dropping the runtime cancels it); `None` when it
    /// runs on a dedicated thread with its own runtime. Either way the loop
    /// ends when this struct is dropped and the channel closes.
    _task: Option<tokio::task::JoinHandle<()>>,
}

impl MemoryWatcher {
    /// Start watching `state_dir` (recursively) and debounce `.coding/` change
    /// bursts into silent derived-index reconciles.
    ///
    /// Events for memory source paths ([`is_memory_source_path`]) set a dirty
    /// flag and reset the debounce timer; when `debounce` elapses with no
    /// further events, one pass runs: claim the shared maintenance guard, the
    /// `corpus_is_stale` pre-check, then `index_derived`. Passes never run
    /// concurrently with the startup reconcile or a manual rebuild, and an
    /// event that arrives while the guard is held is drained and reconciled
    /// afterwards instead of being lost.
    ///
    /// Returns `Err` when the OS watcher cannot be created or `state_dir`
    /// cannot be watched (the caller logs this and continues without live
    /// refresh).
    pub fn spawn(
        store: Arc<dyn MemoryStoreTrait>,
        state_dir: PathBuf,
        debounce: Duration,
    ) -> Result<Self, notify::Error> {
        // Bridge notify's synchronous callback into async via an unbounded
        // tokio channel: `UnboundedSender::send` is a plain sync call (no
        // await), so it is safe to use directly in the watcher callback.
        let (tx, rx) = tokio_mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            // Best-effort: if the debounce task has gone away, drop the event.
            let _ = tx.send(res);
        })?;
        watcher.watch(&state_dir, RecursiveMode::Recursive)?;

        let task_state_dir = state_dir.clone();
        // Run the debounce loop on a Tokio runtime. Most callers are already
        // inside one, but a host may build its brain from a context with NO
        // reactor (e.g. an app's setup hook on the main thread) — a bare
        // `tokio::spawn` panics ("no reactor running"), crashing the app on
        // startup. Fall back to a dedicated thread with its own single-threaded
        // runtime: the loop is one task that exits when the channel closes
        // (watcher dropped), so the thread terminates on its own and never
        // needs a handle.
        let task = match tokio::runtime::Handle::try_current() {
            Ok(handle) => Some(handle.spawn(async move {
                Self::debounce_loop(store, task_state_dir, rx, debounce).await;
            })),
            Err(_) => {
                let thread = std::thread::Builder::new()
                    .name("memory-watcher".to_string())
                    .spawn(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                eprintln!("memory: watcher runtime failed to start: {e}");
                                return;
                            }
                        };
                        rt.block_on(Self::debounce_loop(store, task_state_dir, rx, debounce));
                    });
                match thread {
                    Ok(_) => None,
                    Err(e) => {
                        eprintln!("memory: watcher thread failed to start: {e}");
                        return Err(notify::Error::generic(&format!(
                            "could not spawn watcher thread: {e}"
                        )));
                    }
                }
            }
        };

        Ok(Self {
            _watcher: watcher,
            _task: task,
        })
    }

    /// The debounce loop: consume watcher events, coalesce a burst, then run
    /// one silent reconcile pass.
    async fn debounce_loop(
        store: Arc<dyn MemoryStoreTrait>,
        state_dir: PathBuf,
        mut rx: tokio_mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
        debounce: Duration,
    ) {
        // Derived exactly as the startup reconcile derives them (brain.rs):
        // the plans dir is the anchor and everything else is its sibling.
        let plans_dir = state_dir.join("plans");
        let backlog_path = state_dir.join("backlog.jsonl");
        loop {
            // Wait for the first event of a burst. `recv` returns None when
            // the watcher is dropped (channel closed) — exit the loop.
            let Some(first) = rx.recv().await else {
                return;
            };
            let mut dirty = Self::event_is_memory_source(&first, &state_dir);

            // Coalesce the burst: keep draining until `debounce` passes with
            // no new events. Any event resets the timer so a long edit storm
            // produces ONE reconcile at the end, not N.
            loop {
                tokio::time::sleep(debounce).await;
                let mut saw_more = false;
                while let Ok(ev) = rx.try_recv() {
                    saw_more = true;
                    if Self::event_is_memory_source(&ev, &state_dir) {
                        dirty = true;
                    }
                }
                if !saw_more {
                    break; // quiet period — the burst is over
                }
            }

            if !dirty {
                continue; // only non-source paths changed
            }

            // Claim the process-wide maintenance guard — the same one the
            // startup reconcile and the manual rebuild hold — retrying until
            // it is free. The pass is NEVER dropped: the operation that holds
            // the guard may have scanned the corpus BEFORE this change landed,
            // so dropping it would leave the change unindexed until the next
            // unrelated edit. Keep draining while waiting so the channel
            // cannot grow without bound.
            let _guard = loop {
                match crate::host::maintenance::claim_busy() {
                    Ok(()) => break crate::host::maintenance::BusyGuard,
                    Err(_) => {
                        if !Self::drain_pending(&mut rx) {
                            return; // watcher dropped — nothing left to refresh
                        }
                        tokio::time::sleep(debounce).await;
                    }
                }
            };

            // The staleness pre-check decides whether the reconcile runs at
            // all — a burst that touched a file with identical content (a
            // re-save) costs a scan, not a rewrite. A pre-check failure
            // degrades to "reconcile anyway", exactly like the startup pass.
            let stale = match crate::memory::indexer::corpus_is_stale(
                store.as_ref(),
                &plans_dir,
                &backlog_path,
            )
            .await
            {
                Ok(stale) => stale,
                Err(e) => {
                    eprintln!(
                        "warning: memory watcher staleness check failed ({e}); reconciling anyway"
                    );
                    true
                }
            };
            // SILENT: no HostEvent. The startup reconcile's events open the
            // full-screen wait dialog; a live pass must never show it.
            if stale {
                match crate::memory::indexer::index_derived(
                    store.as_ref(),
                    &plans_dir,
                    &backlog_path,
                    &|_, _| {},
                )
                .await
                {
                    Ok(report) => {
                        if report.indexed > 0 || report.removed > 0 {
                            eprintln!(
                                "memory: live reconcile — {} indexed, {} removed",
                                report.indexed, report.removed
                            );
                        }
                    }
                    Err(e) => eprintln!("warning: memory watcher reconcile failed: {e}"),
                }
            }
            // The guard drops here (end of iteration) and frees the flag.
        }
    }

    /// Drain every queued watcher event without blocking. Returns `false` when
    /// the channel closed (the watcher was dropped), so the caller can stop.
    fn drain_pending(
        rx: &mut tokio_mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    ) -> bool {
        loop {
            match rx.try_recv() {
                Ok(_) => {}
                Err(tokio_mpsc::error::TryRecvError::Empty) => return true,
                Err(tokio_mpsc::error::TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Whether a raw watcher event carries a memory-source change. Error
    /// events (watch overflow / errors) are treated as changes — better to
    /// re-scan than to miss one.
    fn event_is_memory_source(ev: &notify::Result<notify::Event>, state_dir: &Path) -> bool {
        match ev {
            Ok(event) => {
                is_change_kind(&event.kind)
                    && event
                        .paths
                        .iter()
                        .any(|path| is_memory_source_path(path, state_dir))
            }
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Embedder, HashEmbedder, MemoryStore};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn hash_embedder() -> Arc<dyn Embedder> {
        Arc::new(HashEmbedder::new())
    }

    /// The source-path filter is the whole anti-loop contract: it must accept
    /// every source the derived index reads and reject the indexer's own
    /// outputs (INDEX.md, HEALTH.md, the SQLite stores) — reacting to those
    /// would re-index forever.
    #[test]
    fn memory_watcher_source_paths() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path().join(".coding");
        std::fs::create_dir_all(&state_dir).unwrap();
        let path = |rel: &str| state_dir.join(rel);

        for rel in [
            "plans/abc.md",
            "reviews/r.md",
            "knowledge/spec/2026-x.md",
            "openspec/specs/power-grid/spec.md",
            "openspec/specs/power-grid",
            "backlog.jsonl",
        ] {
            assert!(
                is_memory_source_path(&path(rel), &state_dir),
                "{rel} must trigger a live pass"
            );
        }

        for rel in [
            "knowledge/INDEX.md",
            "knowledge/HEALTH.md",
            "knowledge/.health-state.json",
            "memory.db-wal",
            "codegraph.db",
            "logs/s.jsonl",
            "openspec/changes/archive/2026-11-17-x/proposal.md",
            "openspec/config.yaml",
            "board.json",
        ] {
            assert!(
                !is_memory_source_path(&path(rel), &state_dir),
                "{rel} must NOT trigger a live pass"
            );
        }

        // Outside the state dir, however similar the suffix.
        let other = tempdir().unwrap();
        assert!(!is_memory_source_path(
            &other.path().join("plans/abc.md"),
            &state_dir
        ));
    }

    /// A spec written into `.coding/openspec/specs/` while the app is running
    /// becomes searchable without a restart — the defect this watcher exists
    /// for (six FactoryGame specs written live were invisible to
    /// `memory_search`). Deleting the capability folder must remove its rows
    /// again, which proves the removal path triggers too.
    #[tokio::test]
    async fn memory_watcher_indexes_a_new_spec_live() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path().join(".coding");
        std::fs::create_dir_all(state_dir.join("plans")).unwrap();

        let store = Arc::new(MemoryStore::open_in_memory(hash_embedder()).unwrap());
        let trait_store: Arc<dyn MemoryStoreTrait> = store.clone();
        let _watcher =
            MemoryWatcher::spawn(trait_store, state_dir.clone(), Duration::from_millis(100))
                .expect("spawn memory watcher");

        let spec_dir = state_dir.join("openspec/specs/power-grid");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            spec_dir.join("spec.md"),
            "# Power grid\n\n## Purpose\nKeep the lights on.\n\n## Requirements\n\n### Requirement: Grid demand is sampled\nThe system SHALL sample demand every second.\n\n#### Scenario: Sampling\n- **WHEN** the quixotic grid switch is on\n- **THEN** one demand sample is stored\n",
        )
        .unwrap();

        let id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            b"openspec:power-grid/Grid demand is sampled",
        )
        .to_string();
        // 200 × 50ms = 10s: FSEvents (macOS) delivers events after a latency
        // window, and the maintenance guard is process-global — another test
        // holding it makes this wait, not fail.
        let mut found = false;
        for _ in 0..200 {
            if store.get_memory(&id).await.unwrap().is_some() {
                found = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            found,
            "a spec written while running must be indexed live"
        );

        std::fs::remove_dir_all(&spec_dir).unwrap();
        let mut removed = false;
        for _ in 0..200 {
            if store.get_memory(&id).await.unwrap().is_none() {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            removed,
            "removing the capability folder must remove its rows live"
        );
    }

    /// The indexer's own writes (`knowledge/INDEX.md`, `memory.db-wal`) must
    /// not trigger a pass: the live reconcile writes those itself, and
    /// reacting to them would loop forever.
    ///
    /// The `plans/p1.md` probe is what makes the assertion real, and it was
    /// added after a review finding on this exact test: on an EMPTY corpus a
    /// triggered pass indexes nothing and leaves the state table empty, so the
    /// test passed even against a filter that accepted everything. Fails when
    /// [`is_memory_source_path`] is temporarily made to accept every path
    /// (verified red that way, then reverted).
    #[tokio::test]
    async fn memory_watcher_ignores_its_own_index_writes() {
        let dir = tempdir().unwrap();
        let state_dir = dir.path().join(".coding");
        std::fs::create_dir_all(state_dir.join("knowledge")).unwrap();
        std::fs::create_dir_all(state_dir.join("plans")).unwrap();
        // The probe source: written BEFORE the watcher starts, so no event
        // announces it and it is still UNINDEXED once watching begins. Any
        // pass the indexer's own writes triggered below would index it and
        // leave `plan:p1` in the state table — with an empty corpus the
        // assertion would hold even against a filter that accepts everything.
        std::fs::write(
            state_dir.join("plans/p1.md"),
            "# Plan: unindexed probe\n\n## Goal\nMust stay unindexed.\n\n- [ ] 1. step\n",
        )
        .unwrap();

        let store = Arc::new(MemoryStore::open_in_memory(hash_embedder()).unwrap());
        let trait_store: Arc<dyn MemoryStoreTrait> = store.clone();
        let _watcher =
            MemoryWatcher::spawn(trait_store, state_dir.clone(), Duration::from_millis(100))
                .expect("spawn memory watcher");

        std::fs::write(
            state_dir.join("knowledge/INDEX.md"),
            "# Knowledge index\n\n- spec: nothing yet\n",
        )
        .unwrap();
        std::fs::write(state_dir.join("memory.db-wal"), "wal").unwrap();

        // Well past the 100ms debounce — a pass would have run by then.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let keys = store.index_state_keys().await.unwrap();
        assert!(
            keys.is_empty(),
            "the indexer's own writes must not trigger a reconcile pass \
             (it indexed {keys:?} — the unindexed plans/p1.md probe proves a pass ran)"
        );
    }
}

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! OS file-watcher for CodeGraph — keeps the graph fresh on every edit.
//!
//! [`CodeGraph::index`] is incremental (content-hash gated: only changed files
//! are re-parsed) but otherwise only runs at startup and on a manual
//! `codegraph_refresh`. This module bridges that gap with an OS-level file
//! watcher ([`notify::RecommendedWatcher`]) over the project root: any create/
//! modify/remove of a source file marks the tree dirty, and a short debounce
//! after the last event of a burst triggers one incremental `index()` pass.
//!
//! Because it watches the *filesystem*, it catches edits from every source —
//! the agent's `file_edit`/`file_write`/`file_append` tools, a human editor,
//! `git checkout`/merge — without those paths needing to know the graph
//! exists. Event filtering reuses the search tool's exclusion rules
//! (`is_ignored_component` + extension gating), so build output, dependencies,
//! and VCS metadata never trigger a re-index.
//!
//! The watcher is best-effort: a watcher that fails to start, a burst that
//! races an in-progress index, or an indexing failure are all logged and
//! skipped — the graph simply stays one pass behind, never a fatal error.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc as tokio_mpsc;

use crate::codegraph::CodeGraph;

/// Whether `path` should trigger a re-index: any file that does not pass
/// through an ignored directory or `.coding/`. Mirrors `walk_searchable`'s
/// coverage (the content index holds every searchable file, not just
/// parseable source — review C1), so a docs/config edit refreshes its FTS
/// rows too.
///
/// `.coding/` is deliberately excluded from WATCH events (while still being
/// indexed by each pass): it holds the codegraph + memory DBs — which are
/// written continuously DURING an index pass, so watching them would
/// self-trigger a reindex loop — plus per-step bookkeeping (stack.json,
/// backlog.jsonl) that would churn passes all session. Those files' content
/// rows simply refresh on the next pass (startup, any source edit, or a
/// manual rebuild).
///
/// Symlink-tolerant: FSEvents (macOS) delivers event paths with symlinks
/// resolved, so a watched root under a symlinked path (every macOS
/// tempdir: /var → /private/var) is also matched in its canonical form —
/// see the fallback inside.
fn is_indexable_path(path: &Path, root: &Path) -> bool {
    fn under_base(path: &Path, base: &Path) -> bool {
        let rel = match path.strip_prefix(base) {
            Ok(r) => r,
            Err(_) => return false, // outside base — never react
        };
        for component in rel.components() {
            if let std::path::Component::Normal(name) = component {
                let name = &*name.to_string_lossy();
                if crate::tool::agent::search::is_ignored_component(name) || name == ".coding" {
                    return false;
                }
            }
        }
        true
    }
    if under_base(path, root) {
        return true;
    }
    // FSEvents (macOS) delivers event paths with every symlink resolved —
    // a watched root that itself sits under a symlink (every macOS
    // tempdir: /var → /private/var) then never prefix-matches, and the
    // watcher silently filters ALL events out (both watcher tests failed
    // this way on the macOS CI leg, run 35521221353). Retry against the
    // canonicalized root; on Windows the raw prefix already matched (or
    // the path is genuinely outside), so the fallback is inert there.
    match std::fs::canonicalize(root) {
        Ok(canonical) => canonical.as_path() != root && under_base(path, &canonical),
        Err(_) => false,
    }
}

/// Whether an event kind represents a content change worth re-indexing for
/// (create / modify / remove / rename). Metadata-only and access events are
/// ignored — they cannot change the graph.
fn is_change_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// A running CodeGraph file watcher. Holds the OS watcher alive — dropping
/// this struct stops watching. Construct with [`GraphWatcher::spawn`].
pub struct GraphWatcher {
    /// The OS watcher. Held only to keep the watch alive; events arrive on the
    /// channel consumed by the spawned debounce task.
    _watcher: RecommendedWatcher,
    /// Handle to the debounce/index task. `Some` when the caller's Tokio
    /// runtime spawned it (dropping the runtime cancels it); `None` when it
    /// runs on a dedicated thread with its own runtime. Either way the loop
    /// ends when this struct is dropped and the channel closes.
    _task: Option<tokio::task::JoinHandle<()>>,
}

impl GraphWatcher {
    /// Start watching `root` (recursively) and debounce change bursts into
    /// incremental [`CodeGraph::index`] passes.
    ///
    /// Events for indexable source files set a dirty flag and reset the
    /// debounce timer; when `debounce` elapses with no further events, one
    /// `index()` pass runs on the blocking pool. If the graph is already
    /// indexing (e.g. a manual refresh or the startup pass is still running),
    /// the loop waits for that pass to finish before starting — passes never
    /// run concurrently, and a burst is never lost (the content-hash gate
    /// makes the eventual pass cheap).
    ///
    /// Returns `Err` when the OS watcher cannot be created or the root cannot
    /// be watched (the caller logs this and continues without auto-refresh).
    pub fn spawn(
        graph: Arc<CodeGraph>,
        root: PathBuf,
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
        watcher.watch(&root, RecursiveMode::Recursive)?;

        let task_root = root.clone();
        // Run the debounce loop on a Tokio runtime. Most callers are already
        // inside one (IPC handlers, tests), but `build_brain` runs from
        // Tauri's `.setup()` hook on the main thread — there is NO reactor
        // there, and a bare `tokio::spawn` panics ("no reactor running"),
        // crashing the app on startup. Fall back to a dedicated thread with
        // its own single-threaded runtime: the loop is one task that exits
        // when the channel closes (watcher dropped), so the thread terminates
        // on its own and never needs a handle.
        let task = match tokio::runtime::Handle::try_current() {
            Ok(handle) => Some(handle.spawn(async move {
                Self::debounce_loop(graph, task_root, rx, debounce).await;
            })),
            Err(_) => {
                let thread = std::thread::Builder::new()
                    .name("codegraph-watcher".to_string())
                    .spawn(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                eprintln!("codegraph: watcher runtime failed to start: {e}");
                                return;
                            }
                        };
                        rt.block_on(Self::debounce_loop(graph, task_root, rx, debounce));
                    });
                match thread {
                    Ok(_) => None,
                    Err(e) => {
                        eprintln!("codegraph: watcher thread failed to start: {e}");
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

    /// The debounce loop: consume watcher events, coalesce a burst, then index.
    async fn debounce_loop(
        graph: Arc<CodeGraph>,
        root: PathBuf,
        mut rx: tokio_mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
        debounce: Duration,
    ) {
        loop {
            // Wait for the first event of a burst. `recv` returns None when
            // the watcher is dropped (channel closed) — exit the loop.
            let Some(first) = rx.recv().await else {
                return;
            };
            let mut dirty = Self::event_is_indexable(&first, &root);

            // Coalesce the burst: keep draining until `debounce` passes with
            // no new events. Any event resets the timer so a long edit storm
            // produces ONE index pass at the end, not N.
            loop {
                tokio::time::sleep(debounce).await;
                let mut saw_more = false;
                while let Ok(ev) = rx.try_recv() {
                    saw_more = true;
                    if Self::event_is_indexable(&ev, &root) {
                        dirty = true;
                    }
                }
                if !saw_more {
                    break; // quiet period — the burst is over
                }
            }

            if !dirty {
                continue; // only non-source/ignored paths changed
            }
            // Wait for any in-progress pass (startup or manual refresh) to
            // finish before starting ours — `index()` owns the indexing flag
            // and clears it via a drop guard on EVERY exit path (Ok, Err,
            // panic unwind), so this wait always terminates. Never run two
            // passes concurrently (review Low 1).
            while graph.is_indexing() {
                tokio::time::sleep(debounce).await;
            }
            let g = Arc::clone(&graph);
            let result = tokio::task::spawn_blocking(move || g.index(None)).await;
            match result {
                Ok(Ok(stats)) => {
                    if stats.files_reindexed > 0 || stats.files_pruned > 0 {
                        eprintln!(
                            "codegraph: auto-index — {} re-parsed, {} pruned ({} symbols, {} edges)",
                            stats.files_reindexed, stats.files_pruned, stats.symbols, stats.edges
                        );
                    }
                }
                Ok(Err(e)) => eprintln!("codegraph: auto-index failed: {e}"),
                Err(e) => eprintln!("codegraph: auto-index task panicked: {e}"),
            }
        }
    }

    /// Whether a raw watcher event carries an indexable source-file change.
    /// Error events (watch overflow / errors) are treated as indexable —
    /// better to re-scan than to miss a change.
    fn event_is_indexable(ev: &notify::Result<notify::Event>, root: &Path) -> bool {
        match ev {
            Ok(event) => {
                is_change_kind(&event.kind)
                    && event.paths.iter().any(|p| is_indexable_path(p, root))
            }
            // A watch error (e.g. queue overflow) could mean missed events —
            // conservatively re-index rather than serve a stale graph.
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn is_indexable_path_accepts_any_file_and_rejects_ignored() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        assert!(is_indexable_path(&root.join("a.rs"), root));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/b.tsx"), "export const B = 1;").unwrap();
        assert!(is_indexable_path(&root.join("src/b.tsx"), root));
        // Non-source files are watched too — the content index covers every
        // searchable file (review C1), so a docs/config edit must refresh it.
        std::fs::write(root.join("notes.md"), "# n").unwrap();
        assert!(
            is_indexable_path(&root.join("notes.md"), root),
            "non-source text files are indexable"
        );
        // Ignored dir.
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/x.rs"), "fn x() {}").unwrap();
        assert!(!is_indexable_path(&root.join("target/x.rs"), root));
        // .coding/ holds the graph/memory DBs + per-step bookkeeping —
        // watching it would self-trigger reindex loops on DB writes.
        std::fs::create_dir_all(root.join(".coding/plans")).unwrap();
        std::fs::write(root.join(".coding/plans/stack.json"), "{}").unwrap();
        assert!(!is_indexable_path(
            &root.join(".coding/plans/stack.json"),
            root
        ));
        assert!(!is_indexable_path(&root.join(".coding/codegraph.db"), root));
        // Outside root.
        let other = tempdir().unwrap();
        assert!(!is_indexable_path(&other.path().join("o.rs"), root));
    }

    /// Regression (macOS CI leg, run 35521221353): FSEvents delivers
    /// event paths with every symlink resolved, so a watched root that
    /// sits under a symlink (every macOS tempdir: /var → /private/var)
    /// never prefix-matched its own events — the watcher filtered ALL of
    /// them out and both watcher tests failed with "watcher must
    /// auto-index…". The filter must accept an event path under the
    /// canonical form of the root. Unix-only: building the alias takes a
    /// symlink; the defect is FSEvents-specific (on Windows notify
    /// delivers the watched root's own form and the raw prefix matches).
    #[cfg(unix)]
    #[test]
    fn is_indexable_path_accepts_canonical_event_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        // An alias of the root — the not-yet-canonical form the watcher
        // is handed.
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        // The event path in canonical form, as FSEvents delivers it.
        let canonical = std::fs::canonicalize(&alias).unwrap();
        let event = canonical.join("src").join("b.rs");
        assert!(
            is_indexable_path(&event, &alias),
            "a canonical event path must match its symlinked root"
        );
        // The canonical root matches directly (the fast path).
        assert!(is_indexable_path(&event, &canonical));
        // The ignored-dir filtering holds through the canonical form too.
        assert!(!is_indexable_path(&canonical.join("target/x.rs"), &alias));
    }

    #[test]
    fn change_kind_filters_metadata_and_access() {
        use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind};
        assert!(is_change_kind(&EventKind::Create(CreateKind::File)));
        assert!(is_change_kind(&EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content
        ))));
        assert!(!is_change_kind(&EventKind::Access(AccessKind::Read)));
        assert!(!is_change_kind(&EventKind::Access(AccessKind::Close(
            AccessMode::Write
        ))));
    }

    #[test]
    fn watcher_spawns_and_indexes_outside_runtime() {
        // Regression: the production call site (Tauri `.setup()` hook, main
        // thread) has NO Tokio runtime. `GraphWatcher::spawn` used to call a
        // bare `tokio::spawn` there and panic with "no reactor running",
        // crashing the app on startup. It must fall back to its own
        // thread+runtime instead. This test deliberately runs with no runtime
        // (plain #[test], not #[tokio::test]).
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("a.rs"), "pub fn alpha() {}").unwrap();
        let graph = Arc::new(CodeGraph::open_in_memory(root.clone()).unwrap());
        graph.index(None).unwrap();

        let _watcher =
            GraphWatcher::spawn(Arc::clone(&graph), root.clone(), Duration::from_millis(100))
                .expect("spawn watcher");

        std::fs::write(root.join("b.rs"), "pub fn beta() {}").unwrap();

        // Poll with std sleeps — the fallback runtime drives the loop on
        // its own thread. 200 × 50ms = 10s: FSEvents (macOS) delivers
        // events after a latency window, so the poll must outlast it.
        let mut found = false;
        for _ in 0..200 {
            let view = graph.view().unwrap();
            if !view.resolve("beta").is_empty() {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(found, "watcher must auto-index outside a Tokio runtime");
    }

    #[tokio::test]
    async fn watcher_reindexes_on_new_source_file() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Seed one file + initial index.
        std::fs::write(root.join("a.rs"), "pub fn alpha() {}").unwrap();
        let graph = Arc::new(CodeGraph::open_in_memory(root.clone()).unwrap());
        graph.index(None).unwrap();

        let _watcher =
            GraphWatcher::spawn(Arc::clone(&graph), root.clone(), Duration::from_millis(100))
                .expect("spawn watcher");

        // Create a new source file — the watcher should pick it up and index it.
        std::fs::write(root.join("b.rs"), "pub fn beta() {}").unwrap();

        // Poll for the new symbol (bounded so a broken watcher fails
        // fast). 200 × 50ms = 10s: FSEvents (macOS) delivers events after
        // a latency window, so the poll must outlast it.
        let mut found = false;
        for _ in 0..200 {
            let view = graph.view().unwrap();
            if !view.resolve("beta").is_empty() {
                found = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(found, "watcher must auto-index the new file's symbol");
    }
}

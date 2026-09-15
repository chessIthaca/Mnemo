// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! CodeGraph — a per-project code knowledge graph (GitNexus-inspired).
//!
//! On project startup the codebase is parsed with tree-sitter into symbols
//! (functions, structs, traits, classes, …) and edges between them (calls,
//! imports, containment), stored in `<root>/.coding/codegraph.db`. The graph
//! is kept incrementally up to date (only changed files are re-parsed) and is
//! exposed two ways: read-only `graph_*` agent tools (structural context in
//! one query instead of grep chains) and the right-panel Graph tab.
//!
//! The DB is a derivable cache — safe to delete, rebuilt on next startup.
//!
//! Module layout:
//! - [`walk`] — project file walker (reuses the search tool's ignore rules).
//! - [`extract`] — tree-sitter symbol/ref extraction per file + edge
//!   resolution policy.
//! - [`schema`] / [`store`] — the SQLite tables and the incremental-update
//!   rules (upsert / remove / prune / cross-file edge rebuild).
//! - [`query`] — the read-side query engine (resolve / 360° context /
//!   blast-radius impact / shortest path) over in-memory snapshots.
//!
//! [`CodeGraph`] below is the facade the app wires up: it owns the store
//! behind a `Mutex` (mirroring `MemoryStore`), exposes snapshots to the query
//! layer, and runs incremental re-indexes without ever blocking startup —
//! indexing runs on a background thread and every failure degrades to "graph
//! unavailable", never a fatal error.

pub mod extract;
pub mod query;
pub mod schema;
pub mod store;
pub mod walk;
pub mod watcher;

use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::codegraph::extract::{extract_file, extract_file_with, ParserCache, RefKind};
use crate::codegraph::query::GraphView;
use crate::codegraph::store::{
    ContentHit, FileData, FileMeta, GraphStats, Store, UpsertEntry,
};
use crate::codegraph::walk::{is_code_extension, walk_searchable, Lang};
use crate::error::Result;

/// The outcome of one indexing pass — returned to the caller and surfaced via
/// the IPC status command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IndexStats {
    /// Source files found by the walker.
    pub files_scanned: usize,
    /// Files (re-)parsed because they were new or their content changed.
    pub files_reindexed: usize,
    /// Stored files dropped because they no longer exist on disk.
    pub files_pruned: usize,
    /// Total symbols in the store after the pass.
    pub symbols: usize,
    /// Total edges in the store after the pass.
    pub edges: usize,
    /// References that could not be resolved to any symbol (dropped edges).
    pub unresolved_refs: usize,
    /// Wall-clock time for the pass, milliseconds.
    pub elapsed_ms: u64,
    /// Worker threads the extract waves ran on (0 when there was nothing
    /// to do) — the load-immune parallelism observable: a wall-clock bound
    /// cannot survive the parallel test harness (contention slows the pass
    /// ~2.5x past the quiet sequential baseline), so the regression guard
    /// asserts this instead (backlog 5a85e36c).
    pub workers: usize,
}

/// The shared graph handle. `Send + Sync`: the store lives behind a `Mutex`
/// (like `MemoryStore`) and the indexing flag is atomic so the IPC status
/// command can read it without touching the store lock.
pub struct CodeGraph {
    /// The project root the graph indexes (paths are stored relative to it).
    root: PathBuf,
    /// The SQLite store. Short critical sections only: `index` takes the
    /// lock once for the metadata snapshot and once per wave of upserts,
    /// and query snapshots load in two fast SELECTs.
    store: Mutex<Store>,
    /// True while an indexing pass is running (status surface only).
    indexing: AtomicBool,
}

/// Clears the indexing flag on drop. Held for the duration of
/// [`CodeGraph::index`], so every exit path — success, `Err`, and a panic
/// unwind — clears the flag (review M1, 2026-04-19: the flag was previously
/// set by the startup caller and never cleared, sticking at `true` for the
/// rest of the process lifetime).
struct IndexFlagGuard<'a>(&'a AtomicBool);

impl Drop for IndexFlagGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// The outcome of a [`CodeGraph::reindex_files_containing`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReindexOutcome {
    /// Source files whose on-disk bytes contain the needle.
    pub matched: usize,
    /// Code files whose on-disk bytes contain the needle that no grammar
    /// parses (a language outside [`Lang`]'s set — `.swift`, `.lua`, …).
    /// Their symbols can never resolve in the graph, so the gate treats
    /// `matched_unparsed > 0` as disk-containment evidence that the
    /// recorded test name is real and passes with a note instead of
    /// erroring on a test that exists on disk.
    pub matched_unparsed: usize,
    /// Files actually re-parsed + upserted. A store write failure drops a
    /// matched file from this count — the caller must treat
    /// `matched > 0 && reparsed == 0` as inconclusive (the refresh
    /// failed), never as "the symbol is absent" (review LOW-1,
    /// 2026-09-08).
    pub reparsed: usize,
    /// True when the re-parse budget ([`REPARSE_CAP`]) was exhausted:
    /// matches beyond the cap were counted but NOT re-parsed, so a
    /// not-found verdict after this pass is inconclusive for the
    /// un-re-parsed files (the gate error notes the partial refresh).
    pub capped: bool,
}

/// Whether `haystack` contains `needle` as a whole identifier — some
/// occurrence flanked by non-identifier bytes (or the edges). Substring
/// hits inside a longer identifier (`regressio` inside `regression`)
/// do not count: the gate's disk-containment evidence must not pass a
/// typo'd or partial recorded name (review LOW-2, 2026-09-11).
fn contains_whole_identifier(haystack: &[u8], needle: &[u8]) -> bool {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(at) = haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
    {
        let start = from + at;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_ident(haystack[start - 1]);
        let after_ok = end == haystack.len() || !is_ident(haystack[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// The re-parse budget for one [`CodeGraph::reindex_files_containing`]
/// escalation. The escalation exists to refresh a MISSING symbol, not to
/// re-parse the world: a common identifier (e.g. a widely-used helper name)
/// can match dozens of files, and 16 tree-sitter parses already bound the
/// worst case well under a second. Matches beyond the cap are still counted
/// ([`ReindexOutcome::matched`]) — the scan is cheap — only the re-parse is
/// skipped, and [`ReindexOutcome::capped`] flags the partial refresh.
const REPARSE_CAP: usize = 16;

/// Copy the graph DB at `src_db` into a fresh, consistent snapshot at
/// `dest_db` — run-all worktree seeding (2026-09-08 performance LOW-2
/// lever b): a spawned worktree agent's cold index pass otherwise re-reads,
/// re-hashes and re-parses every file the main tree's graph already
/// indexed.
///
/// `VACUUM INTO` is documented to produce "a consistent snapshot of the
/// original database" — it reads through the live WAL without blocking
/// writers. A plain file copy is NOT safe here: reading `db` + `-wal` +
/// `-shm` at different moments yields an incoherent trio, and a copy that
/// includes upsert commits but misses the trailing `rebuild_edges` commit
/// serves new symbols with stale cross-file edges — NOT self-healing,
/// because a seeded pass that re-parses nothing (`reindexed == 0`) never
/// fires its own edge rebuild, and the incremental index trusts the stored
/// content hashes. (Missed whole-file commits, by contrast, ARE
/// self-healing — a fresh parse.) Seeding is safe on the destination side
/// by construction: the worktree's index pass re-checks every file's
/// content hash against the worktree's actual bytes, so unchanged files
/// skip the parse and changed files just re-parse (never worse than cold).
///
/// Best-effort by design: any error propagates to the caller, which falls
/// back to the cold full-index pass.
pub fn snapshot_db(src_db: &Path, dest_db: &Path) -> Result<()> {
    if let Some(parent) = dest_db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open_with_flags(src_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    let dest = dest_db.to_string_lossy();
    conn.execute("VACUUM INTO ?1", [dest.as_ref()])?;
    Ok(())
}

impl CodeGraph {
    /// Open the on-disk graph for `root` (creating the DB file if needed).
    pub fn open(root: PathBuf, db_path: &Path) -> Result<Self> {
        Ok(Self {
            root,
            store: Mutex::new(Store::open(db_path)?),
            indexing: AtomicBool::new(false),
        })
    }

    /// Open a transient in-memory graph (tests).
    pub fn open_in_memory(root: PathBuf) -> Result<Self> {
        Ok(Self {
            root,
            store: Mutex::new(Store::open_in_memory()?),
            indexing: AtomicBool::new(false),
        })
    }

    /// The project root this graph indexes.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether an indexing pass is currently running.
    pub fn is_indexing(&self) -> bool {
        self.indexing.load(Ordering::Relaxed)
    }

    /// Force-set the indexing flag. [`CodeGraph::index`] owns the real
    /// lifecycle (set on entry, cleared by a drop guard on every exit path);
    /// this setter stays `pub` for direct control by the future IPC refresh
    /// / status surface.
    pub fn set_indexing(&self, value: bool) {
        self.indexing.store(value, Ordering::Relaxed);
    }

    /// Lock the store, recovering from poisoning (review L4, 2026-04-19
    /// freeze-fix review): the DB is a derived cache — if a panic poisoned
    /// the mutex mid-write, the worst case is a corrupt cache recovered by
    /// deleting the file (rebuilt on next startup), so poisoning must not
    /// cascade into every later caller panicking in *their* thread.
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Aggregate counts (files / symbols / edges / last indexed time).
    pub fn stats(&self) -> Result<GraphStats> {
        self.store().stats()
    }

    /// Snapshot the graph into an in-memory [`GraphView`] for querying. The
    /// store lock is held only for the two SELECTs; traversal runs unlocked.
    pub fn view(&self) -> Result<GraphView> {
        GraphView::load(&self.store())
    }

    /// Phrase-query the FTS content index — the `search` tool's index-backed
    /// path for literal queries. See [`Store::search_content`].
    pub fn search_content(&self, literal: &str, limit: usize) -> Result<Vec<ContentHit>> {
        self.store().search_content(literal, limit)
    }

    /// One symbol by name, loose (exact → case-insensitive equality →
    /// shortest case-insensitive substring, `LIMIT 1`) — the
    /// alternation-branch resolver for the search symbol nudge's absorption
    /// variant (C1). Best-effort like [`Self::symbol_id`]; see
    /// [`Store::symbol_id_fuzzy`].
    pub fn symbol_id_fuzzy(&self, name: &str) -> Result<Option<(String, String)>> {
        self.store().symbol_id_fuzzy(name)
    }

    /// `(indexed files, files with content rows)` — the backfill detector for
    /// DBs created before the content index existed. A gap (`with_content <
    /// files`) triggers a background reindex at graph-open time.
    pub fn content_coverage(&self) -> Result<(usize, usize)> {
        self.store().content_coverage()
    }

    /// Indexed file paths with their as-of-index mtime (unix millis) — the
    /// `search` tool's index-freshness check (F10). See
    /// [`Store::stored_mtimes`].
    pub fn stored_mtimes(&self) -> Result<HashMap<String, i64>> {
        self.store().stored_mtimes()
    }

    /// Whether any indexed symbol is named exactly `name` (case-sensitive) —
    /// the search tools' symbol nudge: when a bare-identifier pattern names
    /// an indexed symbol, `search`/`search_read` prepend a one-line note
    /// pointing at graph_search/graph_context (use the graph tools for symbol
    /// lookups; `search` is for text). Best-effort by design: `false` on
    /// any store error, a poisoned lock recovery, or an unindexed graph — a
    /// nudge must never fail or slow down a working search. One indexed
    /// SELECT. See [`Store::symbols_named`].
    pub fn symbol_exists(&self, name: &str) -> bool {
        self.store()
            .symbols_named(name)
            .is_ok_and(|symbols| !symbols.is_empty())
    }

    /// The id (`file::name::line`) of the symbol named exactly `name`
    /// (case-sensitive), or `None` when no symbol matches — the search
    /// tools' symbol nudge embeds it so the model can jump straight to
    /// `graph_context(id=...)` without a `graph_search` resolve round-trip.
    /// Best-effort by design, like [`Self::symbol_exists`]: `None` on any
    /// store error, a poisoned lock recovery, or an unindexed graph — a
    /// nudge must never fail or slow down a working search. One indexed
    /// `LIMIT 1` SELECT. See [`Store::symbol_id_named`].
    pub fn symbol_id(&self, name: &str) -> Option<String> {
        self.store().symbol_id_named(name).unwrap_or(None)
    }

    /// How many symbols the graph indexes for `path` (project-relative),
    /// or `None` when the graph is unavailable — the `read_files`
    /// whole-file nudge: reading a big indexed source file whole is
    /// exactly when targeted `graph_context(id=...)` lookups beat a
    /// second full read. Best-effort like [`Self::symbol_id`]: callers
    /// get `Some(0)` for unknown/unindexed files, never an error. One
    /// indexed COUNT SELECT; Windows-style path separators are
    /// normalized to the store's key form. See
    /// [`Store::symbols_in_file_count`].
    pub fn symbol_count_in_file(&self, path: &str) -> Option<usize> {
        let path = path.replace('\\', "/");
        self.store()
            .symbols_in_file_count(&path)
            .ok()
            .map(|c| c as usize)
    }

    /// Run one incremental indexing pass over the project: re-parse new and
    /// changed files, prune deleted ones, then re-derive cross-file edges.
    ///
    /// Owns the indexing flag: set on entry and cleared by a drop guard on
    /// EVERY exit path (Ok, `Err`, panic unwind) — callers never manage it.
    ///
    /// Change detection is content-hash based (mtime is recorded for
    /// diagnostics but a hash is authoritative — editors that touch files
    /// without changing content must not trigger re-parses). The store lock
    /// is taken once for the metadata snapshot and once per wave of
    /// upserts, so concurrent readers (agent tools, the Graph tab) stay
    /// responsive between waves during a large index.
    ///
    /// `progress`, when given, is called after each file is processed —
    /// after its extract, before the wave's store write — with
    /// `(files_processed, files_total)`.
    ///
    /// Individual file failures (unreadable, upsert error) are logged and
    /// skipped — indexing is best-effort and never fatal to the app.
    pub fn index(&self, progress: Option<&dyn Fn(usize, usize)>) -> Result<IndexStats> {
        self.index_inner(progress, true)
    }

    /// A pass over a DB seeded from ANOTHER tree (run-all worktree
    /// seeding, [`snapshot_db`]): the stored mtimes are the source tree's
    /// and say nothing about this tree's files, so the mtime fast path is
    /// bypassed and every file is content-hash-checked exactly once —
    /// deterministic, so a same-millisecond coincidence between the source
    /// tree's last write and this tree's checkout can never skip a changed
    /// file (review LOW-3, 2026-09-08).
    pub fn index_seeded(&self, progress: Option<&dyn Fn(usize, usize)>) -> Result<IndexStats> {
        self.index_inner(progress, false)
    }

    fn index_inner(
        &self,
        progress: Option<&dyn Fn(usize, usize)>,
        trust_mtime: bool,
    ) -> Result<IndexStats> {
        self.indexing.store(true, Ordering::Relaxed);
        let _indexing = IndexFlagGuard(&self.indexing);
        let started = std::time::Instant::now();
        // Walk EVERY searchable file (any extension), not just parseable
        // source: the FTS content index must cover exactly what the search
        // tool's walk engine scans, or the index path would silently miss
        // docs/config/other file types (review C1). Source files additionally
        // get symbols/edges; the rest get content-only rows.
        let files = walk_searchable(&self.root);
        let total = files.len();

        // One store round-trip for the whole tree's stored metadata
        // (backlog 5a85e36c): the mtime/hash fast paths check this map in
        // the workers instead of paying 3 SELECTs per file under the lock.
        let stored: HashMap<String, FileMeta> = {
            let store = self.store();
            store.stored_file_meta()?
        };

        // The set of files present on disk, for the prune sweep at the end.
        let mut present: HashSet<String> = HashSet::with_capacity(total);
        let mut scanned = 0usize;
        let mut reindexed = 0usize;
        // Files accounted to the progress callback — monotonic 1..=total
        // across waves, whatever order the workers finish in.
        let mut done = 0usize;
        // The worker count the waves actually ran with (0 when nothing to
        // do) — surfaced in IndexStats as the load-immune parallelism
        // observable (backlog 5a85e36c).
        let mut wave_workers = 0usize;

        // Parallel waves (backlog 5a85e36c): the per-file work — stat,
        // mtime/hash fast-path check, read, parse, extract — is
        // embarrassingly parallel, so it runs on scoped worker threads
        // (each with its own parser cache); the store writes are applied
        // in ONE transaction per wave on this thread. Waves bound the
        // memory held for extracted results and keep the store lock
        // acquisitions sparse, so concurrent readers (agent tools, the
        // Graph tab) stay responsive between waves.
        const WAVE: usize = 512;
        // WAVE is a MEMORY knob, not just a latency knob: each entry held
        // between the join and the upsert carries the file's full text, so
        // a wave of 512 × ~1 MB files (the walk's size cap) transiently
        // holds ~0.5 GB. Realistic repos are nowhere near that; if a
        // pathological tree ever needs it, pack waves by a byte budget
        // (~96 MB) instead of a file count (review LOW-2, plan 85368a1f).
        for wave in files.chunks(WAVE) {
            // Resolve rel paths on the coordinator (relpath is a pure path
            // computation); files outside root (symlink trickery — the walk
            // already guards) are counted but produce no work.
            let mut jobs: Vec<(&PathBuf, String)> = Vec::with_capacity(wave.len());
            for path in wave {
                scanned += 1;
                if let Some(rel) = relpath(&self.root, path) {
                    present.insert(rel.clone());
                    jobs.push((path, rel));
                } else if let Some(cb) = progress {
                    done += 1;
                    cb(done, total);
                }
            }
            if jobs.is_empty() {
                continue;
            }
            // Workers pull jobs off a shared index and push their results
            // into per-worker output vecs (placed by job index after the
            // join, so the application order stays the walk's sorted
            // order — deterministic regardless of interleaving).
            let next = AtomicUsize::new(0);
            let (tx, rx) = mpsc::channel::<()>();
            let workers = worker_count().min(jobs.len());
            wave_workers = wave_workers.max(workers);
            let mut outputs: Vec<Vec<(usize, UpsertEntry)>> =
                (0..workers).map(|_| Vec::new()).collect();
            std::thread::scope(|s| {
                for out in outputs.iter_mut() {
                    let tx = tx.clone();
                    let next = &next;
                    let jobs = &jobs;
                    let stored = &stored;
                    s.spawn(move || {
                        let mut parsers = ParserCache::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some((path, rel)) = jobs.get(i) else {
                                break;
                            };
                            if let Some(entry) =
                                extract_job(path, rel, trust_mtime, stored, &mut parsers)
                            {
                                out.push((i, entry));
                            }
                            let _ = tx.send(());
                        }
                    });
                }
                drop(tx);
                // The coordinator drains the completion pulses while the
                // workers run — the progress callback is not Send, so it
                // stays on this thread; the count sequence stays
                // monotonic 1..=total.
                for _ in rx {
                    done += 1;
                    if let Some(cb) = progress {
                        cb(done, total);
                    }
                }
            });

            // Apply the wave's entries in the walk's sorted order
            // (deterministic regardless of worker interleaving).
            let mut entries: Vec<(usize, UpsertEntry)> =
                outputs.into_iter().flatten().collect();
            entries.sort_unstable_by_key(|(i, _)| *i);
            let entries: Vec<UpsertEntry> = entries.into_iter().map(|(_, e)| e).collect();
            if entries.is_empty() {
                continue;
            }
            {
                let mut store = self.store();
                match store.upsert_files(&entries) {
                    Ok(()) => reindexed += entries.len(),
                    Err(e) => {
                        // A batch failure (disk full, corruption) must not
                        // lose the whole wave — fall back to per-file
                        // application, logging failures like the old
                        // sequential loop did.
                        eprintln!("codegraph: wave upsert failed ({e}); applying per file");
                        for entry in &entries {
                            match store.upsert_file(
                                &entry.path,
                                &entry.content_hash,
                                entry.mtime,
                                &entry.data,
                                &entry.content,
                            ) {
                                Ok(()) => reindexed += 1,
                                Err(e) => {
                                    eprintln!(
                                        "codegraph: failed to index {}: {e}",
                                        entry.path
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut stats = IndexStats {
            files_scanned: scanned,
            files_reindexed: reindexed,
            workers: wave_workers,
            ..IndexStats::default()
        };
        {
            let mut store = self.store();
            stats.files_pruned = store.prune_missing(&present)?;
            if reindexed > 0 || stats.files_pruned > 0 {
                stats.unresolved_refs = store.rebuild_edges()?;
            }
            let s = store.stats()?;
            stats.symbols = s.symbols;
            stats.edges = s.edges;
        }
        stats.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(stats)
    }

    /// Re-index exactly the given project-relative files (DB key form, `/`
    /// separators) — the search tools' inline refresh for a stale content
    /// index (F10): instead of only warning and falling back to the tree
    /// walk, a small staleness is repaired on the spot so the query can be
    /// re-served from the fresh index.
    ///
    /// Coordinates with the watcher through the indexing flag: the flag is
    /// claimed ATOMICALLY (`compare_exchange`) and held by the same drop
    /// guard as [`CodeGraph::index`], so the watcher's debounce loop (which
    /// waits for the flag to clear) never overlaps this pass. A claim
    /// against an already-running pass returns `Ok(0)` without touching the
    /// flag — the caller walks that once, and the running pass refreshes
    /// the index shortly.
    ///
    /// Vanished files are pruned (their lingering rows stop being served);
    /// unreadable files are skipped and logged (best-effort, like `index`).
    /// A file that became non-UTF-8 since it was indexed is pruned too —
    /// the walk engine skips binaries, so the index must not serve their
    /// old text rows. Cross-file edges are re-derived only when a source
    /// file was re-parsed or a file was pruned — a content-only refresh
    /// never touches refs. Every successfully-read path is
    /// containment-checked against the project root (canonicalized) before
    /// its content is indexed — a crafted out-of-root DB key is skipped,
    /// never indexed (defense-in-depth; a non-existent out-of-root key
    /// falls to the vanished branch, which prunes DB rows only — safe
    /// either way). Returns the number of files refreshed (upserted or
    /// pruned).
    pub fn reindex_stale_files(&self, rel_paths: &[String]) -> Result<usize> {
        if self
            .indexing
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            // A pass is already running (watcher or manual refresh) — it
            // will refresh the index; the caller walks this once.
            return Ok(0);
        }
        let _indexing = IndexFlagGuard(&self.indexing);
        // Containment baseline: canonicalize the root once — every read
        // below is checked against it (defense-in-depth, review LOW 1).
        let root_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let mut refreshed = 0usize;
        let mut touched_source = false;
        let mut pruned = false;
        for rel in rel_paths {
            let abs = self.root.join(rel);
            let Ok(bytes) = std::fs::read(&abs) else {
                // Vanished between the staleness detection and now — prune
                // its rows so the FTS index stops serving it. Safe for any
                // key: remove_file mutates DB rows only, never the
                // filesystem.
                match self.store().remove_file(rel) {
                    Ok(()) => {
                        refreshed += 1;
                        pruned = true;
                    }
                    Err(e) => eprintln!("codegraph: failed to prune {rel}: {e}"),
                }
                continue;
            };
            // Containment guard (defense-in-depth, review LOW 1): DB keys
            // are written by the indexer via `relpath` (strip_prefix —
            // guaranteed under-root), but a tampered local cache could
            // carry a `../`-prefixed key — never read content from outside
            // the project root into the index. A skipped file stays stale,
            // so the caller walks (the authoritative engine) on the retry.
            let Ok(canon) = abs.canonicalize() else {
                eprintln!("codegraph: skipping {rel}: unreadable path");
                continue;
            };
            if !canon.starts_with(&root_canon) {
                eprintln!("codegraph: skipping {rel}: outside the project root");
                continue;
            }
            let mut hasher = DefaultHasher::new();
            hasher.write(&bytes);
            let hash = format!("{:x}", hasher.finish());
            let is_source = abs
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| Lang::from_extension(&ext.to_ascii_lowercase()).is_some())
                .unwrap_or(false);
            let result = if is_source {
                touched_source = true;
                self.reindex_one(&abs, rel, &hash, &bytes)
            } else {
                // Content-only, mirroring index()'s non-source arm.
                match std::str::from_utf8(&bytes) {
                    Ok(source) => {
                        let mtime = mtime_of(&abs);
                        let data = FileData {
                            symbols: Vec::new(),
                            refs: Vec::new(),
                            contains: Vec::new(),
                        };
                        self.store().upsert_file(rel, &hash, mtime, &data, source)
                    }
                    Err(_) => {
                        // Became binary since it was indexed — drop its
                        // stale text rows (the walk engine skips binaries).
                        pruned = true;
                        self.store().remove_file(rel)
                    }
                }
            };
            match result {
                Ok(()) => refreshed += 1,
                Err(e) => eprintln!("codegraph: failed to reindex {rel}: {e}"),
            }
        }
        if touched_source || pruned {
            self.store().rebuild_edges()?;
        }
        Ok(refreshed)
    }

    /// Force a re-parse of every SOURCE file whose on-disk content contains
    /// `needle`, regardless of stored mtime/hash — the finish gate's
    /// escalation (backlog 648e0ad5). The incremental [`Self::index`] pass
    /// is mtime/hash-based: a DB whose meta rows are fresh but whose SYMBOL
    /// rows are stale or missing (a partial write, schema drift, or a prior
    /// best-effort skip) never re-parses the file, so a symbol that exists
    /// on disk keeps missing and the gate would error on it (live case
    /// 2027-01-08: 'checkFillContract'). This reads the disk truth instead:
    /// walk the searchable files, and for each CODE file (any
    /// programming-language extension — [`is_code_extension`], a superset
    /// of [`Lang`]'s parseable set) whose bytes contain the needle:
    /// parseable files re-parse + upsert unconditionally up to
    /// [`REPARSE_CAP`] — matches beyond the cap are counted but not
    /// re-parsed ([`ReindexOutcome::capped`]) (the
    /// [`Self::reindex_stale_files`] machinery: hash, containment guard,
    /// [`Self::reindex_one`]) — while non-parseable code files (a language
    /// no grammar is wired for) count as
    /// [`ReindexOutcome::matched_unparsed`]: disk-containment evidence for
    /// the gate, since their symbols can never resolve. Returns the
    /// outcome: `matched` counts parseable source files whose on-disk
    /// bytes contain the needle; `matched_unparsed` counts non-parseable
    /// code files containing it; `reparsed` counts
    /// successful re-parses — a store write failure drops a matched file
    /// from `reparsed`, so `matched > 0 && reparsed == 0` means the
    /// refresh failed (inconclusive), never that the symbol is absent;
    /// `capped` flags a partial refresh (the budget was exhausted, so a
    /// not-found verdict is inconclusive for the un-re-parsed files).
    ///
    /// Unlike `index`/`reindex_stale_files`, this does NOT take the indexing
    /// flag: it is a last-chance escalation, and a concurrent mtime/hash-based
    /// pass cannot fix the missing-symbol-rows case it exists for — bailing
    /// on contention would error the caller on a symbol that exists on
    /// disk. The upserts are idempotent and the store mutex serializes
    /// writers, so a concurrent pass is safe.
    pub fn reindex_files_containing(&self, needle: &str) -> Result<ReindexOutcome> {
        if needle.is_empty() {
            return Ok(ReindexOutcome {
                matched: 0,
                matched_unparsed: 0,
                reparsed: 0,
                capped: false,
            });
        }
        // Containment baseline: canonicalize the root once — every read
        // below is checked against it (defense-in-depth, mirroring
        // reindex_stale_files).
        let root_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let mut matched = 0usize;
        let mut matched_unparsed = 0usize;
        let mut reparsed = 0usize;
        let mut capped = false;
        let mut touched_source = false;
        for path in walk_searchable(&self.root) {
            let Some(rel) = relpath(&self.root, &path) else {
                continue;
            };
            // Only CODE files count for the containment check — every
            // programming-language extension ([`is_code_extension`]), not
            // just the parseable set: a regression test may live in a
            // language no grammar parses. Docs/config are excluded (the
            // plan file itself records the test name, so admitting them
            // would defeat the gate's hallucination guard).
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if !ext.as_deref().is_some_and(is_code_extension) {
                continue;
            }
            let parseable = ext.as_deref().and_then(Lang::from_extension).is_some();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // A cheap scan — the needle is an identifier, so containment
            // must be word-bounded: a match flanked by identifier bytes
            // (`regressio` inside `regression`) is a substring of a longer
            // name, not the recorded test (review LOW-2, 2026-09-11).
            // UTF-8 is self-synchronizing, so an ASCII needle can only
            // match as itself in valid UTF-8; the byte scan keeps the
            // semantics identical for non-UTF-8 files.
            let contains = contains_whole_identifier(&bytes, needle.as_bytes());
            if !contains {
                continue;
            }
            if !parseable {
                // A code file in a language no grammar parses: the name
                // is on disk, but there are no symbol rows to refresh —
                // count it as unparsed containment evidence for the gate.
                matched_unparsed += 1;
                continue;
            }
            // The name is on disk in a source file — count it as matched
            // BEFORE any subsequent skip (containment guard, re-parse
            // failure): a matched-but-not-reparsed file means the
            // refresh failed (inconclusive), never that the symbol is
            // absent (review LOW-1, 2026-09-08).
            matched += 1;
            // Containment guard (defense-in-depth, mirroring
            // reindex_stale_files): never read content from outside the
            // project root into the index.
            let Ok(canon) = path.canonicalize() else {
                eprintln!("codegraph: skipping {rel}: unreadable path");
                continue;
            };
            if !canon.starts_with(&root_canon) {
                eprintln!("codegraph: skipping {rel}: outside the project root");
                continue;
            }
            // Re-parse budget: a common identifier (e.g. a helper name) can
            // match dozens of files; the escalation exists to refresh a
            // MISSING symbol, not to re-parse the world. Past the cap, keep
            // counting matches (the scan is cheap) but skip the re-parse and
            // flag the outcome — the gate error notes the partial refresh.
            if reparsed >= REPARSE_CAP {
                capped = true;
                continue;
            }
            let mut hasher = DefaultHasher::new();
            hasher.write(&bytes);
            let hash = format!("{:x}", hasher.finish());
            match self.reindex_one(&path, &rel, &hash, &bytes) {
                Ok(()) => {
                    reparsed += 1;
                    touched_source = true;
                }
                Err(e) => eprintln!("codegraph: failed to reindex {rel}: {e}"),
            }
        }
        if touched_source {
            self.store().rebuild_edges()?;
        }
        Ok(ReindexOutcome {
            matched,
            matched_unparsed,
            reparsed,
            capped,
        })
    }

    /// Detect staleness among the indexed SOURCE files — the symbol index's
    /// freshness sweep, the sibling of the search tool's F10 content check
    /// (backlog 95f21af0). Every indexed file whose extension parses as a
    /// source language (any [`Lang`] extension — the only files that carry
    /// symbols) is stat'ed against its as-of-index mtime; a mismatch — or
    /// a vanished/unreadable file (mtime 0) — means the symbol rows may
    /// lag the working tree. The caller (graph_search's total-miss
    /// repair) feeds these to [`Self::reindex_stale_files`]. Stats only:
    /// no reads, no parses — a steady-state sweep costs one
    /// `stored_mtimes()` query plus one stat per indexed source file, so
    /// a miss on a genuinely absent symbol stays fast.
    pub fn stale_source_files(&self) -> Result<Vec<String>> {
        let mut stale = Vec::new();
        for (rel, stored_mtime) in self.stored_mtimes()? {
            let is_source = std::path::Path::new(&rel)
                .extension()
                .and_then(|e| e.to_str())
                .and_then(|ext| Lang::from_extension(&ext.to_ascii_lowercase()))
                .is_some();
            if !is_source {
                continue;
            }
            if mtime_of(&self.root.join(&rel)) != stored_mtime {
                stale.push(rel);
            }
        }
        Ok(stale)
    }

    /// Parse one file and replace its rows in the store. Separated from
    /// [`CodeGraph::index`] so the per-file lock scope is obvious.
    fn reindex_one(&self, path: &Path, rel: &str, hash: &str, bytes: &[u8]) -> Result<()> {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return Ok(());
        };
        let Some(lang) = Lang::from_extension(&ext.to_ascii_lowercase()) else {
            return Ok(());
        };
        // tree-sitter needs &str; lossy conversion keeps one bad byte from
        // dropping the whole file.
        let source = String::from_utf8_lossy(bytes);
        let extracted = extract_file(rel, &source, lang);
        let mtime = mtime_of(path);
        let data = FileData {
            symbols: extracted.symbols,
            refs: extracted
                .refs
                .into_iter()
                .map(|r| {
                    let kind = match r.kind {
                        RefKind::Call => "call",
                        RefKind::Import => "import",
                    };
                    (r.from, r.name, kind.to_string())
                })
                .collect(),
            contains: extracted.contains,
        };
        // The walk already applied should_search's rules (ignored dirs, 1 MB
        // cap), so `source` is exactly the content `search` would see.
        self.store().upsert_file(rel, hash, mtime, &data, &source)
    }

    /// Test-only (backlog 648e0ad5): simulate a partial write — the file's
    /// meta rows (hash + mtime + content) are written FRESH but its SYMBOL
    /// rows are empty. The incremental [`Self::index`] pass then no-ops
    /// (the hash matches), exactly the live scenario where the finish gate
    /// errored on a regression-test symbol that exists on disk.
    #[cfg(test)]
    pub(crate) fn simulate_partial_write_for_test(&self, rel: &str) -> Result<()> {
        let abs = self.root.join(rel);
        let bytes = std::fs::read(&abs)?;
        let mut hasher = DefaultHasher::new();
        hasher.write(&bytes);
        let hash = format!("{:x}", hasher.finish());
        let source = String::from_utf8_lossy(&bytes).into_owned();
        let data = FileData {
            symbols: Vec::new(),
            refs: Vec::new(),
            contains: Vec::new(),
        };
        self.store()
            .upsert_file(rel, &hash, mtime_of(&abs), &data, &source)
    }
}

/// Project-relative path with `/` separators (the DB key form), or `None`
/// when `path` is not under `root`.
fn relpath(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// A file's mtime as unix millis, or 0 when unavailable (diagnostics only —
/// the content hash is the authoritative change signal).
fn mtime_of(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        // Millisecond resolution (not seconds): two writes within the same
        // second would otherwise share an mtime, causing the M2 fast-path to
        // skip a genuinely changed file. Millis fit comfortably in i64 (safe
        // until the year 10889) and are fine-grained enough that same-tick
        // collisions are practically impossible — saves are human-paced and
        // each index pass takes >1 ms.
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The parallel index pass's worker count (backlog 5a85e36c): the
/// machine's parallelism, capped at 8 — parse work scales with cores, but
/// each worker's parser cache (one tree-sitter parser per language) makes
/// very high core counts memory-hungry for little gain.
fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
}

/// One file's parallel-extract work (backlog 5a85e36c): the mtime/hash
/// fast-path check against the batch-loaded `stored` map, the read, the
/// hash, and the parse+extract — everything that does not touch the
/// store. Returns `None` for skipped files (unchanged since last index,
/// unreadable, binary), mirroring the old sequential loop's skip rules
/// exactly.
fn extract_job(
    path: &Path,
    rel: &str,
    trust_mtime: bool,
    stored: &HashMap<String, FileMeta>,
    parsers: &mut ParserCache,
) -> Option<UpsertEntry> {
    // M2: mtime fast-path. When the on-disk mtime is unchanged since
    // the last index, the file's content cannot have changed — skip
    // the read+hash entirely (the stored hash is still valid). When
    // the mtime *did* advance, fall through to the authoritative
    // hash comparison, so editors that touch a file without changing
    // its content still don't trigger a re-parse. This turns a full
    // project re-read into a stat-only sweep on steady-state passes.
    // Seeded passes pass `trust_mtime = false`: stored mtimes from
    // another tree are meaningless for this tree's files (see
    // [`CodeGraph::index_seeded`]).
    let disk_mtime = mtime_of(path);
    let meta = stored.get(rel);
    let stored_hash = meta.and_then(|m| m.content_hash.as_deref());
    let stored_mtime = meta.and_then(|m| m.mtime);
    let has_content = meta.map(|m| m.has_content).unwrap_or(false);
    if trust_mtime && stored_mtime == Some(disk_mtime) && has_content && stored_hash.is_some() {
        // Unchanged since last index — already indexed with identical
        // content. Skip the read+hash+reparse.
        return None;
    }

    // Hash the content; unreadable files are skipped, not fatal.
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("codegraph: skipping unreadable file {}", path.display());
        return None;
    };
    let mut hasher = DefaultHasher::new();
    hasher.write(&bytes);
    let hash = format!("{:x}", hasher.finish());

    // Re-parse when the content changed OR when the file predates
    // the content index (no cg_content_meta row). The latter makes a
    // plain index() pass backfill old DBs — and every app startup
    // already runs one in the background, so existing projects gain
    // the content index with no manual step. The meta row is written
    // for every upsert (even empty files), so this fires once per
    // old file, never perpetually.
    let needs_reindex = stored_hash != Some(hash.as_str()) || !has_content;
    if !needs_reindex {
        return None;
    }
    let lang = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| Lang::from_extension(&ext.to_ascii_lowercase()));
    let (data, content) = match lang {
        Some(lang) => {
            // tree-sitter needs &str; lossy conversion keeps one bad byte
            // from dropping the whole file (mirrors reindex_one).
            let source = String::from_utf8_lossy(&bytes);
            let extracted = extract_file_with(rel, &source, lang, parsers);
            let data = FileData {
                symbols: extracted.symbols,
                refs: extracted
                    .refs
                    .into_iter()
                    .map(|r| {
                        let kind = match r.kind {
                            RefKind::Call => "call",
                            RefKind::Import => "import",
                        };
                        (r.from, r.name, kind.to_string())
                    })
                    .collect(),
                contains: extracted.contains,
            };
            (data, source.into_owned())
        }
        None => {
            // Content-only: no tree-sitter grammar, but the text is
            // searchable. Mirrors the walk engine's skip rule — a
            // non-UTF-8 file (binary) is never indexed.
            let source = std::str::from_utf8(&bytes).ok()?;
            (
                FileData {
                    symbols: Vec::new(),
                    refs: Vec::new(),
                    contains: Vec::new(),
                },
                source.to_string(),
            )
        }
    };
    let mtime = mtime_of(path);
    Some(UpsertEntry {
        path: rel.to_string(),
        content_hash: hash,
        mtime,
        data,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a small Rust + TS fixture tree.
    fn fixture(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn helper() -> u32 { 1 }\n").unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "use crate::lib::helper;\nfn main() { helper(); }\n",
        )
        .unwrap();
        std::fs::write(root.join("app.ts"), "export function boot() {}\n").unwrap();
    }

    #[test]
    fn first_index_parses_all_files() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_scanned, 3);
        assert_eq!(stats.files_reindexed, 3);
        assert_eq!(stats.files_pruned, 0);
        assert!(stats.symbols >= 6, "module symbols + definitions");
        assert!(stats.edges > 0);
        // The cross-file call main→helper resolved (same crate).
        let view = graph.view().unwrap();
        let helpers = view.resolve("helper");
        assert!(!helpers.is_empty());
        let ctx = view.context(&helpers[0].id).unwrap();
        assert!(ctx.incoming_total >= 1, "main.rs calls helper");
    }

    #[test]
    fn reindex_files_containing_reparses_despite_fresh_meta() {
        // Backlog 648e0ad5 (live case 2027-01-08: 'checkFillContract'): the
        // incremental index() pass is mtime/hash-based, so a DB whose meta
        // rows are fresh but whose SYMBOL rows are missing (a partial
        // write) never re-parses the file — the pass no-ops and a symbol
        // that exists on disk keeps missing. reindex_files_containing must
        // read the disk truth and force the re-parse.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        assert!(
            !graph.view().unwrap().resolve("boot").is_empty(),
            "sanity: the symbol resolves after a normal index"
        );
        // Corrupt: meta rows fresh (hash + mtime + content), symbol rows
        // gone — the partial-write scenario.
        graph.simulate_partial_write_for_test("app.ts").unwrap();
        assert!(
            graph.view().unwrap().resolve("boot").is_empty(),
            "sanity: the symbol rows are gone after the partial write"
        );
        // The incremental pass NO-OPS (the root cause): the hash matches,
        // so index() never re-parses the file.
        graph.index(None).unwrap();
        assert!(
            graph.view().unwrap().resolve("boot").is_empty(),
            "the incremental pass must no-op on fresh meta rows — the root cause"
        );
        // The fix: the forced re-parse finds it. Only app.ts contains
        // "boot" in the fixture, so exactly one file matches and
        // re-parses (review LOW-1, 2026-09-08: matched and reparsed are
        // tracked separately — a re-parse failure must not read as
        // "absent").
        let outcome = graph.reindex_files_containing("boot").unwrap();
        assert_eq!(outcome.matched, 1, "exactly the one file contains the name");
        assert_eq!(
            outcome.reparsed, 1,
            "exactly the one file containing the name re-parses"
        );
        assert!(
            !graph.view().unwrap().resolve("boot").is_empty(),
            "the escalation must find the symbol after the forced re-parse"
        );
    }

    #[test]
    fn reindex_files_containing_caps_its_reparse_budget() {
        // A common identifier can match dozens of files; the escalation
        // exists to refresh a MISSING symbol, not to re-parse the world.
        // Past REPARSE_CAP the pass keeps counting matches (the scan is
        // cheap) but skips the re-parse and flags the outcome, so the gate
        // error can note the partial refresh.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(REPARSE_CAP + 4) {
            std::fs::write(
                dir.path().join(format!("m{i}.rs")),
                format!("pub fn needle_holder_{i}() {{ run_all_dispatch_next(); }}\n"),
            )
            .unwrap();
        }
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        let outcome = graph
            .reindex_files_containing("run_all_dispatch_next")
            .unwrap();
        assert_eq!(
            outcome.matched,
            REPARSE_CAP + 4,
            "every match is counted, cap or not"
        );
        assert_eq!(outcome.reparsed, REPARSE_CAP, "re-parses stop at the cap");
        assert!(outcome.capped, "the cap is flagged for the gate error");
    }

    #[test]
    fn reindex_files_containing_counts_non_parseable_code_files() {
        // The gate's disk check must cover EVERY programming language, not
        // just the parseable set: a regression test written in a language
        // no grammar parses (.swift, .lua, …) exists on disk and is
        // runnable, so its name counts as unparsed containment evidence —
        // while docs (.md) never count: the plan file itself records the
        // test name, so admitting it would let ANY recorded name satisfy
        // the check and defeat the gate's hallucination guard.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tests.swift"),
            "func swift_gate_regression() {\n    assert(true)\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("notes.md"),
            "The plan records the name swift_gate_regression here.\n",
        )
        .unwrap();
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        let outcome = graph
            .reindex_files_containing("swift_gate_regression")
            .unwrap();
        assert_eq!(
            outcome.matched, 0,
            "no parseable source file contains the name"
        );
        assert_eq!(
            outcome.matched_unparsed, 1,
            "the .swift code file counts as disk-containment evidence"
        );
        assert_eq!(outcome.reparsed, 0, "nothing was re-parsed");
        assert!(!outcome.capped);
        // A name in NO code file matches nothing at all.
        let absent = graph.reindex_files_containing("no_such_test_anywhere").unwrap();
        assert_eq!(absent.matched, 0);
        assert_eq!(absent.matched_unparsed, 0);
        // Word-boundary containment (review LOW-2, 2026-09-11): a typo'd
        // partial name that is a substring of the on-disk identifier does
        // NOT count as containment evidence.
        let partial = graph.reindex_files_containing("swift_gate_regressio").unwrap();
        assert_eq!(
            partial.matched_unparsed, 0,
            "substring hits inside a longer identifier are not containment"
        );
    }

    #[test]
    fn symbol_exists_is_exact_and_best_effort() {
        // The search tools' nudge probe: EXACT (case-sensitive) symbol-name
        // matches only — a nudge must be precise, never firing on ordinary
        // words, near-miss casings, or prefixes.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        assert!(graph.symbol_exists("helper"));
        assert!(graph.symbol_exists("boot"), "TS symbols too");
        assert!(!graph.symbol_exists("Helper"), "case-sensitive");
        assert!(!graph.symbol_exists("helpe"), "prefix is not exact");
        assert!(!graph.symbol_exists("nonexistent"));

        // An unindexed graph knows no symbols — no nudge before first index.
        let dir2 = tempfile::tempdir().unwrap();
        fixture(dir2.path());
        let fresh = CodeGraph::open_in_memory(dir2.path().to_path_buf()).unwrap();
        assert!(!fresh.symbol_exists("helper"));
    }

    #[test]
    fn symbol_id_is_exact_and_best_effort() {
        // The search tools' nudge embeds `graph_context(id="...")`: an
        // exact (case-sensitive) symbol-name hit returns the id
        // (file::name::line), misses return None — mirroring
        // [`symbol_exists_is_exact_and_best_effort`].
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        assert_eq!(
            graph.symbol_id("helper").as_deref(),
            Some("src/lib.rs::helper::1")
        );
        assert_eq!(graph.symbol_id("boot").as_deref(), Some("app.ts::boot::1"));
        assert_eq!(graph.symbol_id("Helper"), None, "case-sensitive");
        assert_eq!(graph.symbol_id("helpe"), None, "prefix is not exact");
        assert_eq!(graph.symbol_id("nonexistent"), None);

        // An unindexed graph knows no symbols — no id before first index.
        let dir2 = tempfile::tempdir().unwrap();
        fixture(dir2.path());
        let fresh = CodeGraph::open_in_memory(dir2.path().to_path_buf()).unwrap();
        assert_eq!(fresh.symbol_id("helper"), None);
    }

    #[test]
    fn symbol_count_in_file_is_per_file_and_best_effort() {
        // The read_files whole-file nudge counts the symbols indexed for
        // ONE file: indexed files report their count, unknown files report
        // Some(0) (no nudge), and an unindexed graph never errors.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let lib = graph.symbol_count_in_file("src/lib.rs").unwrap();
        assert!(lib >= 2, "module + definition symbols: {lib}");
        let main = graph.symbol_count_in_file("src/main.rs").unwrap();
        assert!(main >= 2, "module + main() symbols: {main}");
        assert_eq!(graph.symbol_count_in_file("src/nope.rs"), Some(0));
        // Windows-style separators are normalized to the store's key form —
        // tool-call paths must not have to care.
        assert_eq!(
            graph.symbol_count_in_file("src\\main.rs"),
            graph.symbol_count_in_file("src/main.rs")
        );

        // An unindexed graph counts nothing — no nudge before first index.
        let dir2 = tempfile::tempdir().unwrap();
        fixture(dir2.path());
        let fresh = CodeGraph::open_in_memory(dir2.path().to_path_buf()).unwrap();
        assert_eq!(fresh.symbol_count_in_file("src/lib.rs"), Some(0));
    }

    #[test]
    fn second_index_reparses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_scanned, 3);
        assert_eq!(stats.files_reindexed, 0, "unchanged content → no re-parse");
        assert_eq!(stats.files_pruned, 0);
    }

    #[test]
    fn mtime_fast_path_skips_unchanged_files() {
        // M2: the indexer records each file's mtime and uses it as a skip
        // fast-path on steady-state passes. After the first index, the stored
        // mtime must match the on-disk mtime, and a second pass must reindex
        // nothing — proving the fast-path fired (not the hash comparison).
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();

        let lib = dir.path().join("src/lib.rs");
        let disk_mtime = mtime_of(&lib);
        // The store recorded the mtime at index time.
        assert_eq!(
            graph.store().file_mtime("src/lib.rs").unwrap(),
            Some(disk_mtime),
            "stored mtime must match disk mtime after index"
        );

        // Second pass: mtime unchanged → fast-path skips read+hash+reparse.
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_reindexed, 0, "mtime fast-path skipped all");
    }

    #[test]
    fn mtime_fast_path_still_detects_content_changes() {
        // M2: when a file's mtime advances (content edited), the fast-path
        // must NOT skip it — the authoritative hash comparison runs and
        // detects the change. Guards against the fast-path over-skipping.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();

        // Edit one file — mtime advances, content changes.
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() -> u32 { 42 }\n",
        )
        .unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(
            stats.files_reindexed, 1,
            "edited file not skipped by fast-path"
        );
    }

    #[test]
    fn editing_one_file_reindexes_exactly_one() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        // Touch content of exactly one file.
        std::fs::write(
            dir.path().join("src/main.rs"),
            "use crate::lib::helper;\nfn main() { helper(); helper(); }\n",
        )
        .unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_scanned, 3);
        assert_eq!(stats.files_reindexed, 1, "only the edited file re-parses");
    }

    #[test]
    fn reindex_stale_files_refreshes_the_given_files() {
        // The search tools' inline refresh (F10): exactly the given files
        // are re-read and re-upserted — the stored mtime catches up to the
        // disk mtime (the freshness check passes afterwards) and the FTS
        // rows serve the new content. Untouched files keep their rows.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();

        // Edit one file AFTER indexing (the watcher-lag gap).
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() -> u32 { 42 }\npub fn fresh_marker() {}\n",
        )
        .unwrap();
        let n = graph.reindex_stale_files(&["src/lib.rs".to_string()]).unwrap();
        assert_eq!(n, 1);

        // The stored mtime now matches the on-disk mtime — the F10
        // freshness check passes for this file.
        let disk = mtime_of(&dir.path().join("src/lib.rs"));
        assert_eq!(
            graph.store().file_mtime("src/lib.rs").unwrap(),
            Some(disk),
            "stored mtime caught up to the disk mtime"
        );

        // The FTS rows serve the new content.
        let hits = graph.search_content("fresh_marker", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.path == "src/lib.rs"),
            "fresh content is indexed: {hits:?}"
        );

        // The untouched file was not re-upserted — its stored mtime still
        // matches its (unchanged) on-disk mtime.
        let main_disk = mtime_of(&dir.path().join("src/main.rs"));
        assert_eq!(graph.store().file_mtime("src/main.rs").unwrap(), Some(main_disk));
    }

    #[test]
    fn reindex_stale_files_prunes_vanished_files() {
        // A file deleted after indexing (rows linger until the next full
        // pass) is pruned by the inline refresh — the FTS index stops
        // serving its content.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let (files, with_content) = graph.content_coverage().unwrap();
        assert_eq!((files, with_content), (3, 3));

        std::fs::remove_file(dir.path().join("app.ts")).unwrap();
        let n = graph.reindex_stale_files(&["app.ts".to_string()]).unwrap();
        assert_eq!(n, 1, "the vanished file counts as pruned");
        let (files, with_content) = graph.content_coverage().unwrap();
        assert_eq!((files, with_content), (2, 2), "rows gone");
        // The FTS index no longer serves its content.
        assert!(graph.search_content("boot", 10).unwrap().is_empty());
    }

    #[test]
    fn reindex_stale_files_busy_returns_zero_and_leaves_the_flag() {
        // Watcher coordination: while a pass is running (flag set), the
        // inline refresh must not run AND must not clear someone else's
        // flag — it returns Ok(0) and the caller walks that once.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() -> u32 { 42 }\npub fn busy_marker() {}\n",
        )
        .unwrap();

        graph.set_indexing(true); // a pass is "running"
        let n = graph.reindex_stale_files(&["src/lib.rs".to_string()]).unwrap();
        assert_eq!(n, 0, "busy claim → no-op");
        assert!(graph.is_indexing(), "someone else's flag is untouched");
        // And nothing was refreshed — the new content is not indexed yet.
        assert!(graph.search_content("busy_marker", 10).unwrap().is_empty());

        graph.set_indexing(false); // the pass is done
        let n = graph.reindex_stale_files(&["src/lib.rs".to_string()]).unwrap();
        assert_eq!(n, 1);
        assert!(!graph.is_indexing(), "guard cleared after the call");
        assert!(!graph.search_content("busy_marker", 10).unwrap().is_empty());
    }

    #[test]
    fn reindex_stale_files_skips_out_of_root_keys() {
        // Defense-in-depth (review LOW 1): DB keys are indexer-written, but
        // a tampered local cache could carry a `../`-prefixed key — the
        // inline refresh must never read content from outside the project
        // root into the FTS index. Fails without the containment guard
        // (the outside content would be upserted and searchable).
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();

        // A sibling directory OUTSIDE the project root, with a secret.
        let outside = dir.path().parent().unwrap().join("cg-outside-secret");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside_secret_marker\n").unwrap();

        // Plant the crafted key directly in the store (the indexer would
        // never write one — relpath/strip_prefix guarantees under-root),
        // with DECOY content distinct from the outside file's real content:
        // only an actual out-of-root READ could surface the real content.
        let data = FileData {
            symbols: Vec::new(),
            refs: Vec::new(),
            contains: Vec::new(),
        };
        graph
            .store()
            .upsert_file(
                "../cg-outside-secret/secret.txt",
                "h",
                1,
                &data,
                "planted_decoy\n",
            )
            .unwrap();

        let n = graph
            .reindex_stale_files(&["../cg-outside-secret/secret.txt".to_string()])
            .unwrap();
        assert_eq!(n, 0, "the out-of-root key is skipped, not refreshed");
        assert!(
            graph
                .search_content("outside_secret_marker", 10)
                .unwrap()
                .is_empty(),
            "the outside file was never read into the index"
        );
        assert!(
            !graph.search_content("planted_decoy", 10).unwrap().is_empty(),
            "the planted row is untouched (skipped, not pruned)"
        );

        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn deleting_a_file_prunes_it() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        std::fs::remove_file(dir.path().join("app.ts")).unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.files_pruned, 1);
        assert!(graph.view().unwrap().resolve("boot").is_empty());
    }

    #[test]
    fn indexing_flag_flips_around_pass() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        assert!(!graph.is_indexing());
        graph.set_indexing(true);
        assert!(graph.is_indexing());
        graph.set_indexing(false);
        assert!(!graph.is_indexing());
        // index() owns the flag lifecycle (review M1): a successful pass
        // leaves it clear.
        graph.index(None).unwrap();
        assert!(!graph.is_indexing(), "success path clears the flag");
    }

    /// Review M1 regression: the indexing flag is set during a pass and
    /// cleared on EVERY exit path — including a panic unwind (the old code
    /// set the flag in the startup caller and never cleared it, so one pass
    /// left the status surface showing "indexing" forever).
    #[test]
    fn indexing_flag_clears_on_panic_unwind() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();

        // Observe the flag mid-pass via the progress callback, then panic at
        // the last file to force an unwind out of index().
        let seen_during_pass = std::sync::Arc::new(AtomicBool::new(false));
        let seen = seen_during_pass.clone();
        let graph_ref = &graph;
        let progress = move |done: usize, total: usize| {
            if graph_ref.is_indexing() {
                seen.store(true, Ordering::Relaxed);
            }
            if done == total {
                panic!("forced mid-index panic");
            }
        };

        // Silence the panic hook so the expected panic doesn't spam output.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph.index(Some(&progress))
        }));
        std::panic::set_hook(prev_hook);

        assert!(result.is_err(), "the progress callback panicked");
        assert!(
            seen_during_pass.load(Ordering::Relaxed),
            "flag was set during the pass"
        );
        assert!(
            !graph.is_indexing(),
            "panic unwind cleared the flag (the drop guard ran)"
        );
    }

    #[test]
    fn index_populates_content_index() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        // Literal content query hits the FTS rows populated by the index pass.
        let hits = graph.search_content("helper", 10).unwrap();
        assert!(!hits.is_empty(), "content index populated by index()");
        assert!(
            hits.iter().any(|h| h.path == "src/lib.rs" && h.line == 1),
            "path + 1-based line recorded: {hits:?}"
        );
        assert_eq!(graph.content_coverage().unwrap(), (3, 3));
        // Edits update content rows through the same incremental path the
        // watcher drives.
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn renamed_helper() -> u32 { 1 }\n",
        )
        .unwrap();
        graph.index(None).unwrap();
        let hits = graph.search_content("renamed_helper", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "pub fn renamed_helper() -> u32 { 1 }");
        assert_eq!(graph.content_coverage().unwrap(), (3, 3));
    }

    #[test]
    fn index_backfills_content_for_pre_content_index_dbs() {
        // Simulate a DB created before the content index existed: symbol
        // rows present, cg_content/cg_content_meta empty. A plain index()
        // pass — the one every app startup already runs in the background —
        // must detect the gap per file and re-parse once to populate the
        // FTS rows, so existing projects gain the index with no manual step.
        // The DB sits at its production path (.coding/codegraph.db): that
        // path is excluded from indexing (should_search), which this test
        // also pins — the DB must never index itself (it changes during
        // every pass).
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        let db = dir.path().join(".coding/codegraph.db");
        {
            let graph = CodeGraph::open(dir.path().to_path_buf(), &db).unwrap();
            graph.index(None).unwrap();
            assert_eq!(graph.content_coverage().unwrap(), (3, 3));
        }
        // Rewind to the pre-content-index state (symbol rows only).
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("DELETE FROM cg_content", []).unwrap();
            conn.execute("DELETE FROM cg_content_meta", []).unwrap();
        }
        let graph = CodeGraph::open(dir.path().to_path_buf(), &db).unwrap();
        assert_eq!(
            graph.content_coverage().unwrap(),
            (3, 0),
            "gap: symbols indexed, content missing"
        );
        let stats = graph.index(None).unwrap();
        assert_eq!(
            stats.files_reindexed, 3,
            "every file re-parsed once to backfill content"
        );
        assert_eq!(graph.content_coverage().unwrap(), (3, 3));
        assert!(!graph.search_content("helper", 10).unwrap().is_empty());
        // Steady state: the next pass re-parses nothing — the meta row is
        // written for every upsert, so the backfill never fires perpetually.
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_reindexed, 0);
    }

    /// Run-all worktree seeding (2026-09-08 performance LOW-2 lever b): a
    /// [`snapshot_db`] copy of one tree's DB, opened over a second tree
    /// where exactly ONE file changed, must re-parse exactly that file —
    /// the seeded pass bypasses the mtime fast path (stored mtimes are the
    /// source tree's, meaningless here), so every file is content-hash
    /// checked and the seeded rows for unchanged files answer queries
    /// identically to a cold build. Tree A's graph stays open across the
    /// snapshot so its committed rows live in the un-checkpointed WAL —
    /// pinning the live-WAL consistency property that motivated VACUUM
    /// INTO over a plain file copy (review LOW-4, 2026-09-08).
    #[test]
    fn snapshot_db_seeds_and_reindexes_only_changed_files() {
        // Tree A ("the main tree"): two source files, cold-indexed on disk.
        let dir_a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir_a.path().join(".coding")).unwrap();
        std::fs::write(dir_a.path().join("a.rs"), "pub fn alpha() -> u32 { 1 }\n")
            .unwrap();
        std::fs::write(dir_a.path().join("b.rs"), "pub fn beta() -> u32 { 2 }\n")
            .unwrap();
        let db_a = dir_a.path().join(".coding/codegraph.db");
        // Held open for the whole test: with the connection alive the
        // committed rows sit in the un-checkpointed WAL (a DB this small
        // never hits the 1000-page auto-checkpoint), so the snapshot below
        // MUST read through the WAL — a naive db-only fs::copy would yield
        // an empty DB and the `files_reindexed == 1` assertion would fail.
        let graph_a = CodeGraph::open(dir_a.path().to_path_buf(), &db_a).unwrap();
        let stats_a = graph_a.index(None).unwrap();
        assert_eq!(stats_a.files_reindexed, 2, "cold pass parses both files");
        assert!(graph_a.symbol_exists("alpha"));
        assert!(graph_a.symbol_exists("beta"));

        // Tree B ("the worktree"): a.rs IDENTICAL, b.rs changed. Seed B's
        // DB from A's snapshot, then run the seeded index pass — it
        // bypasses the mtime fast path by design, so every file is
        // read+hashed and only the hash-mismatched b.rs re-parses.
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_b.path().join("a.rs"), "pub fn alpha() -> u32 { 1 }\n")
            .unwrap();
        std::fs::write(dir_b.path().join("b.rs"), "pub fn beta2() -> u32 { 3 }\n")
            .unwrap();
        let db_b = dir_b.path().join(".coding/codegraph.db");
        snapshot_db(&db_a, &db_b).unwrap();
        let graph = CodeGraph::open(dir_b.path().to_path_buf(), &db_b).unwrap();
        let stats = graph.index_seeded(None).unwrap();
        assert_eq!(stats.files_scanned, 2, "the sweep reads+hashes every file");
        assert_eq!(
            stats.files_reindexed, 1,
            "exactly the changed file re-parses"
        );
        // The seeded rows answer for the unchanged file; the changed
        // file's new symbol is found and its old one is gone — identical
        // to a cold build over tree B.
        assert!(
            graph.symbol_exists("alpha"),
            "unchanged file answered from the seeded rows"
        );
        assert!(graph.symbol_exists("beta2"));
        assert!(!graph.symbol_exists("beta"));
    }

    /// Backlog 5a85e36c (plan 85368a1f): the initial index pass must stay
    /// parallel — a ~500-file mixed-language fixture must index with the
    /// extract waves actually running on worker threads
    /// (`IndexStats::workers`). The guard is deliberately load-immune: a
    /// wall-clock bound cannot survive the parallel test harness (under
    /// `cargo test --workspace`, contention slowed this very fixture to
    /// 2_918 ms — past the quiet sequential baseline of 2_547–2_802 ms —
    /// so no fixed ms bound separates "parallel under load" from
    /// "sequential quiet"). The wall-clock evidence lives in the plan
    /// (sequential 2_547–2_802 ms → parallel 1_174–1_234 ms quiet; repo
    /// cold index 89_235 ms → ~67_500 ms) and in the #[ignore]d
    /// repo_index_timing measurement.
    #[test]
    fn large_fixture_indexes_on_worker_threads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // ~500 files across the four parser families (Rust, TS, Python,
        // HTML-with-script-blocks), a few KB each — enough parse + store
        // work per file that the sequential loop's round-trips dominate.
        for i in 0..200 {
            let body: String = (0..40)
                .map(|j| format!("pub fn r{i}_{j}(a: u32) -> u32 {{ a + {j} }}\n"))
                .collect();
            std::fs::write(root.join(format!("mod{i}.rs")), body).unwrap();
        }
        for i in 0..150 {
            let body: String = (0..40)
                .map(|j| format!("export function t{i}_{j}(a: number) {{ return a + {j}; }}\n"))
                .collect();
            std::fs::write(root.join(format!("app{i}.ts")), body).unwrap();
        }
        for i in 0..100 {
            let body: String = (0..40)
                .map(|j| format!("def p{i}_{j}(a):\n    return a + {j}\n"))
                .collect();
            std::fs::write(root.join(format!("script{i}.py")), body).unwrap();
        }
        for i in 0..50 {
            let js: String = (0..20)
                .map(|j| format!("function h{i}_{j}() {{ return {j}; }}\n"))
                .collect();
            let body = format!("<html><body><script>\n{js}</script></body></html>\n");
            std::fs::write(root.join(format!("page{i}.html")), body).unwrap();
        }
        // The DB lives OUTSIDE the fixture root — walk_searchable counts
        // every file under the root, and the .db/-wal/-shm sidecars would
        // inflate files_scanned by 3.
        let db_dir = tempfile::tempdir().unwrap();
        let db = db_dir.path().join("codegraph.db");
        let graph = CodeGraph::open(root.to_path_buf(), &db).unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_scanned, 500, "the whole fixture is searchable");
        assert_eq!(stats.files_reindexed, 500, "a cold index parses every file");
        println!(
            "large fixture index: {} ms on {} workers",
            stats.elapsed_ms, stats.workers
        );
        // The load-immune guard: the extract waves must actually run on
        // worker threads. A wall-clock bound cannot survive the parallel
        // test harness — under `cargo test --workspace` contention slowed
        // this very fixture to 2_918 ms, PAST the quiet sequential baseline
        // (2_547–2_802 ms), so no fixed ms bound separates "parallel under
        // load" from "sequential quiet". The wall-clock evidence lives in
        // the plan and in the #[ignore]d repo_index_timing measurement
        // (backlog 5a85e36c).
        if worker_count() > 1 {
            assert!(
                stats.workers > 1,
                "the index pass must run its extract waves on worker threads \
                 ({} workers used; see plan 85368a1f)",
                stats.workers
            );
        }
    }

    /// Backlog 5a85e36c (plan 85368a1f): a manual before/after measurement
    /// of the initial index over THIS repository — run with
    /// `cargo test repo_index_timing -- --ignored --nocapture` and record
    /// both numbers in the plan/summary. Ignored in normal runs (it
    /// indexes the real repo tree — seconds — into a throwaway DB in a
    /// tempdir, never the developer's own .coding/codegraph.db).
    #[test]
    #[ignore]
    fn repo_index_timing() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("codegraph.db");
        let graph = CodeGraph::open(root, &db).unwrap();
        let stats = graph.index(None).unwrap();
        println!(
            "repo index: {} files scanned, {} reindexed, {} symbols, {} edges, {} unresolved in {} ms",
            stats.files_scanned,
            stats.files_reindexed,
            stats.symbols,
            stats.edges,
            stats.unresolved_refs,
            stats.elapsed_ms
        );
    }
}

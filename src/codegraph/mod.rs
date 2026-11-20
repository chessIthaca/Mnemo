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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::codegraph::extract::{extract_file, extract_file_with, ParserCache, RefKind, Symbol, SymbolKind};
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
    /// The SQLite store behind an `Arc`, so the background symbol-embed
    /// pass (plan d3336a8a step 9) shares the SAME connection and lock
    /// discipline. Short critical sections only: `index` takes the lock
    /// once for the metadata snapshot and once per wave of upserts, query
    /// snapshots load in two fast SELECTs, and the embed pass holds it
    /// only around one batch read or one vector write — never across an
    /// embed call.
    store: Arc<Mutex<Store>>,
    /// True while an indexing pass is running (status surface only).
    indexing: AtomicBool,
    /// State for the background symbol-embed pass (plan d3336a8a step 9).
    symbol_embed: SymbolEmbedState,
    /// The semantic code search snapshot (plan d3336a8a step 11): the
    /// generation counter the vector/symbol writers bump, plus the loaded
    /// in-memory index [`Self::symbol_search`] ranks against.
    symbol_search: SymbolSearchState,
}

/// The embedder source a pass builds from (plan d3336a8a step 9).
#[cfg(feature = "embeddings")]
#[derive(Clone)]
enum ArmedEmbedder {
    /// The real path:
    /// [`crate::provider::client_factory::build_code_embedder`] on the
    /// pass thread, once per process (the process-global slot), with the
    /// configured memory model widening the shared host's serve list.
    Real {
        /// The model cache dir (`<config dir>/models`).
        cache_dir: PathBuf,
        /// The configured bundled memory model, when there is one.
        memory_model: Option<String>,
    },
    /// The test seam: a stub embedder runs the pass directly — tests
    /// never download a model or touch any host. Test builds only: the
    /// release pass always builds the real embedder.
    #[cfg(test)]
    Stub(Arc<dyn crate::memory::embedder::Embedder>),
}

/// Per-graph state for the background symbol-embed pass (plan d3336a8a
/// step 9). The stop flag exists in every build so [`CodeGraph`]'s drop
/// stays feature-independent; the armed/running state is
/// `embeddings`-only.
pub(crate) struct SymbolEmbedState {
    /// Set by [`CodeGraph`]'s drop: a running pass polls it between
    /// batches and exits, so a dropped graph (engine shutdown, a test
    /// graph going out of scope) never leaves a pass writing to its DB.
    stop: Arc<AtomicBool>,
    /// The armed embedder source — set once by
    /// [`CodeGraph::arm_symbol_embeddings`] BEFORE the graph is shared
    /// (`&mut self`); seed graphs (worktree seeding) are never armed, so
    /// their passes never exist.
    #[cfg(feature = "embeddings")]
    armed: Option<ArmedEmbedder>,
    /// Single-flight: at most one pass per graph — a trigger while one
    /// runs is a no-op (the running pass re-reads the pending queue
    /// every batch, so rows the new trigger produced are picked up).
    #[cfg(feature = "embeddings")]
    running: Arc<AtomicBool>,
}

impl Default for SymbolEmbedState {
    #[cfg(feature = "embeddings")]
    fn default() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            armed: None,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Light build (no `embeddings` feature): the pass does not exist —
    /// the state is only the stop flag.
    #[cfg(not(feature = "embeddings"))]
    fn default() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for CodeGraph {
    /// Signal a running symbol-embed pass to stop at its next batch
    /// boundary (plan d3336a8a step 9): a dropped graph must not leave a
    /// detached pass writing to a DB nobody reads. The pass holds the
    /// store `Arc` only, so without this signal it would run its whole
    /// queue against a database whose engine is gone.
    fn drop(&mut self) {
        self.symbol_embed.stop.store(true, Ordering::Release);
    }
}

// ====== Semantic code search snapshot (plan d3336a8a step 11) ======
// `find_code` ranks symbols by meaning blended with text and name matches.
// The vectors live in `cg_symbol_vec`; re-reading ~16k × 768 f32 rows per
// query would cost more than the ranking itself, so one snapshot per
// generation is held in memory and reloaded only when a writer says it moved.

/// The semantic-search snapshot state (plan d3336a8a step 11): the
/// generation counter the writers bump plus the loaded snapshot.
pub(crate) struct SymbolSearchState {
    /// Bumped whenever stored vectors or symbols change — the symbol-embed
    /// pass storing a batch, an index pass re-parsing or pruning files. The
    /// snapshot records the generation it was loaded at; a query that finds
    /// a newer generation reloads it.
    generation: Arc<AtomicU64>,
    /// The loaded snapshot with the generation and model it was built for.
    cached: Mutex<Option<(u64, String, Arc<SymbolSearchIndex>)>>,
}

impl Default for SymbolSearchState {
    fn default() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            cached: Mutex::new(None),
        }
    }
}

/// The in-memory snapshot `find_code` ranks against (plan d3336a8a step
/// 11): every stored vector for one embedding model plus the symbol
/// metadata and text rows the ranking and the output need. Loaded once per
/// generation by [`CodeGraph::symbol_search`], on the blocking pool.
pub(crate) struct SymbolSearchIndex {
    /// Unit-normalised vectors keyed by symbol id, so cosine is a plain dot
    /// product. Zero vectors are dropped — they carry no direction.
    vectors: HashMap<String, Vec<f32>>,
    /// Every stored symbol, in stable (id) order.
    symbols: Vec<Symbol>,
    /// Symbol id → index into `symbols`.
    by_id: HashMap<String, usize>,
    /// File → symbol indices ordered by `(start_line asc, end_line desc)`,
    /// so an FTS hit's enclosing-symbol lookup is a short scan.
    by_file: HashMap<String, Vec<usize>>,
    /// Symbol id → `(signature, doc block)` from the stored text rows.
    text: HashMap<String, (String, String)>,
    /// Stored vector rows for the model (the progress note's N).
    vector_rows: usize,
    /// Stored symbol text rows (the progress note's M).
    text_rows: usize,
}

impl SymbolSearchIndex {
    /// Load the snapshot for `model`: one read per table, never on the
    /// request path ([`CodeGraph::symbol_search`] runs it on the blocking
    /// pool).
    fn load(store: &Store, model: &str) -> Result<Self> {
        let symbols = store.all_symbols()?;
        let mut by_id = HashMap::with_capacity(symbols.len());
        let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, sym) in symbols.iter().enumerate() {
            by_id.insert(sym.id.clone(), i);
            by_file.entry(sym.file.clone()).or_default().push(i);
        }
        for list in by_file.values_mut() {
            list.sort_by(|a, b| {
                let (x, y) = (&symbols[*a], &symbols[*b]);
                x.start_line
                    .cmp(&y.start_line)
                    .then(y.end_line.cmp(&x.end_line))
            });
        }
        let text: HashMap<String, (String, String)> = store
            .all_symbol_texts()?
            .into_iter()
            .map(|t| (t.symbol_id, (t.signature, t.doc)))
            .collect();
        let text_rows = store.symbol_text_count()?;
        let rows = store.all_symbol_vecs(model)?;
        let vector_rows = rows.len();
        let mut vectors = HashMap::with_capacity(rows.len());
        for row in rows {
            // A vector whose symbol vanished awaits a prune — it can never
            // be rendered, so it never ranks.
            if !by_id.contains_key(&row.symbol_id) {
                continue;
            }
            let norm = row.vec.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm <= 1e-6 {
                // A zero vector carries no direction — it cannot rank.
                continue;
            }
            vectors.insert(
                row.symbol_id,
                row.vec.iter().map(|v| v / norm).collect::<Vec<f32>>(),
            );
        }
        Ok(Self {
            vectors,
            symbols,
            by_id,
            by_file,
            text,
            vector_rows,
            text_rows,
        })
    }

    /// Stored vector rows for the model — the progress note's N.
    pub(crate) fn vector_count(&self) -> usize {
        self.vector_rows
    }

    /// Stored symbol text rows — the progress note's M.
    pub(crate) fn symbol_count(&self) -> usize {
        self.text_rows
    }

    /// Whether any vector can carry the cosine term.
    pub(crate) fn has_vectors(&self) -> bool {
        !self.vectors.is_empty()
    }

    /// The cached vectors as `(symbol id, unit-norm vector)` pairs.
    pub(crate) fn vectors(&self) -> impl Iterator<Item = (&str, &[f32])> {
        self.vectors
            .iter()
            .map(|(id, vector)| (id.as_str(), vector.as_slice()))
    }

    /// One symbol by id, when it still exists.
    pub(crate) fn symbol(&self, id: &str) -> Option<&Symbol> {
        self.by_id.get(id).map(|i| &self.symbols[*i])
    }

    /// Every stored symbol, in stable (id) order — the name-match scan.
    pub(crate) fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// The `(signature, doc block)` stored for a symbol, when it has a text
    /// row.
    pub(crate) fn text(&self, id: &str) -> Option<&(String, String)> {
        self.text.get(id)
    }

    /// The innermost REAL symbol whose line range contains `line` in `file`
    /// — how a content-index hit becomes a symbol (plan d3336a8a step 11).
    /// Synthetic module symbols are skipped: a hit outside any real
    /// definition has no useful symbol to name.
    pub(crate) fn enclosing(&self, file: &str, line: usize) -> Option<&Symbol> {
        let indices = self.by_file.get(file)?;
        let span = |s: &Symbol| s.end_line.saturating_sub(s.start_line);
        let mut best: Option<&Symbol> = None;
        for &i in indices {
            let sym = &self.symbols[i];
            if sym.start_line > line || sym.end_line < line {
                continue;
            }
            if matches!(sym.kind, SymbolKind::Module) {
                continue;
            }
            best = match best {
                Some(current)
                    if span(sym) > span(current)
                        || (span(sym) == span(current)
                            && sym.start_line <= current.start_line) =>
                {
                    Some(current)
                }
                _ => Some(sym),
            };
        }
        best
    }
}

/// Why the code-embedder slot's retry gate was parked (plan d3336a8a step
/// 17, finding parkedNoteEdges): the fallback note must blame the right
/// thing — a serveable host that is not answering, or a model that could
/// not be loaded offline.
#[cfg(feature = "embeddings")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ParkReason {
    /// A host is serveable (a binary resolves or a live descriptor is
    /// published) but did not answer — the note says the shared model
    /// host is not answering.
    HostNotAnswering,
    /// No host can serve at all and the in-process load failed — the
    /// note says the model could not be loaded.
    NotLoaded,
}

/// The process-global code-embedder slot's availability, read by
/// find_code's text fallback (plan d3336a8a step 17, finding
/// notReadyNoteMisleads): a parked retry gate must not be described as
/// "still being built" — nothing is building, and the note says the
/// model is unavailable and when the next round retries.
#[cfg(feature = "embeddings")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CodeEmbedderState {
    /// The slot holds a good build (or a test stub answers queries).
    Ready,
    /// A build is running on another thread.
    Building,
    /// A failed build parked the gate; `retry_in` is how long until the
    /// next round may retry, and `reason` is why it parked.
    Parked {
        retry_in: std::time::Duration,
        reason: ParkReason,
    },
    /// No build has run in this process yet (the graph was never armed,
    /// or the first pass has not reached the embedder).
    Unarmed,
}

impl CodeGraph {
    /// Invalidate the semantic-search snapshot (plan d3336a8a step 11):
    /// called wherever stored vectors or symbols change. The next query
    /// reloads; the stale snapshot still serves until then.
    fn bump_symbol_search(&self) {
        self.symbol_search.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// The semantic-search snapshot for `model` (plan d3336a8a step 11):
    /// the cached one while its generation and model still match, else a
    /// fresh load. Blocking (a full read of the vector and symbol tables) —
    /// call it from the blocking pool, never a tokio worker.
    pub(crate) fn symbol_search(&self, model: &str) -> Result<Arc<SymbolSearchIndex>> {
        let generation = self.symbol_search.generation.load(Ordering::Acquire);
        let mut cached = self
            .symbol_search
            .cached
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some((loaded_at, loaded_model, index)) = cached.as_ref() {
            if *loaded_at == generation && loaded_model == model {
                return Ok(Arc::clone(index));
            }
        }
        let index = Arc::new(SymbolSearchIndex::load(&self.store(), model)?);
        *cached = Some((generation, model.to_string(), Arc::clone(&index)));
        Ok(index)
    }

    /// The stored vector and text-row counts without loading the snapshot —
    /// the `find_code` fallback note and the eval's progress probe.
    pub(crate) fn symbol_search_counts(&self) -> Result<(usize, usize)> {
        let store = self.store();
        Ok((store.symbol_vec_count()?, store.symbol_text_count()?))
    }

    /// How many symbols still await a vector for `model` (plan d3336a8a
    /// step 11) — the eval waits for the pass to drain on this.
    #[cfg(all(test, feature = "embeddings"))]
    pub(crate) fn symbol_embed_pending(&self, model: &str) -> Result<usize> {
        Ok(self.store().pending_symbols(model, 1)?.len())
    }

    /// Content-index hits for a token set, best bm25 first (plan d3336a8a
    /// step 11): the semantic code search's lexical term. One FTS query.
    pub(crate) fn content_hits_any(
        &self,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<ContentHit>> {
        self.store().search_content_any(tokens, limit)
    }

    /// Test seam (plan d3336a8a step 11): store one symbol's vector row, so
    /// a ranking test needs no embed pass.
    #[cfg(all(test, feature = "embeddings"))]
    pub(crate) fn store_symbol_vector_for_test(
        &self,
        model: &str,
        symbol_id: &str,
        vec: &[f32],
    ) -> Result<()> {
        let mut store = self.store();
        let batch: Vec<_> = store
            .pending_symbols(model, 4096)?
            .into_iter()
            .filter(|p| p.symbol_id == symbol_id)
            .collect();
        store.store_symbol_vecs(model, vec.len(), &batch, &[vec.to_vec()])?;
        Ok(())
    }
}

impl CodeGraph {
    /// The code embedder a `find_code` query may use (plan d3336a8a step
    /// 11): the process-global slot's cached good build — never a build,
    /// never a wait. A miss starts the shared build in the background and
    /// answers `None`, so the query falls back to text search instead of
    /// blocking. `None` also when the graph was never armed (no code
    /// embedding for this project).
    #[cfg(feature = "embeddings")]
    pub(crate) fn query_embedder(&self) -> Option<Arc<dyn crate::memory::embedder::Embedder>> {
        match &self.symbol_embed.armed {
            Some(ArmedEmbedder::Real {
                cache_dir,
                memory_model,
            }) => code_embedder_slot::cached().or_else(|| {
                code_embedder_slot::ensure_build(cache_dir, memory_model.as_deref());
                None
            }),
            #[cfg(test)]
            Some(ArmedEmbedder::Stub(embedder)) => Some(Arc::clone(embedder)),
            None => None,
        }
    }

    /// The code-embedder slot's availability for find_code's text
    /// fallback (plan d3336a8a step 17, finding notReadyNoteMisleads): a
    /// parked retry gate must read as "not available right now" — nothing
    /// is being built. Never waits on a build. A stub-armed test graph is
    /// `Ready` (the stub answers queries); an un-armed graph is `Unarmed`.
    #[cfg(feature = "embeddings")]
    pub(crate) fn code_embedder_state(&self) -> CodeEmbedderState {
        match &self.symbol_embed.armed {
            Some(ArmedEmbedder::Real { .. }) => code_embedder_slot::state(),
            #[cfg(test)]
            Some(ArmedEmbedder::Stub(_)) => CodeEmbedderState::Ready,
            None => CodeEmbedderState::Unarmed,
        }
    }



    /// Light build (no `embeddings` feature): no code embedder exists, so
    /// every `find_code` query answers from the text fallback.
    #[cfg(not(feature = "embeddings"))]
    pub(crate) fn query_embedder(&self) -> Option<Arc<dyn crate::memory::embedder::Embedder>> {
        None
    }
}

/// Lock a graph store, recovering from poisoning (review L4, 2026-04-19
/// freeze-fix review): the DB is a derived cache — if a panic poisoned
/// the mutex mid-write, the worst case is a corrupt cache recovered by
/// deleting the file (rebuilt on next startup), so poisoning must not
/// cascade into every later caller panicking in *their* thread. Shared
/// by the graph's methods and the background symbol-embed pass (plan
/// d3336a8a step 9), which locks the same mutex from its own thread.
fn lock_store(mutex: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
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

/// The exact test-title hit behind the finish gate's string-titled tier
/// (backlog 5f3f5123, live case: plan 3d40e529): a vitest/Jest test is
/// string-titled — `it("title", () => …)` — and its anonymous callback
/// body produces no symbol rows, so the graph can never resolve the
/// title even though the test exists on disk and is runnable. When the
/// recorded regression name equals the ENTIRE quoted literal argument of
/// a test-call in a test file, this records where.
#[derive(Debug, Clone)]
pub struct TestTitleHit {
    /// The file (relative to the graph root) whose source carries the
    /// call.
    pub path: std::path::PathBuf,
    /// The test call whose first (title) argument is the literal.
    pub call: &'static str,
}

/// Test-call names whose first (title) argument is a string literal —
/// the accepted set of [`CodeGraph::find_exact_test_title`]. Chained
/// variants (`it.only(`/`test.skip(`) are deliberately outside it: the
/// gate's error names the accepted forms so the agent self-corrects.
const TEST_TITLE_CALLS: [&str; 3] = ["it", "test", "describe"];

/// Whether `rel` (a path relative to the graph root, backslashes
/// normalized) is a test file by the finish gate's filter: the FILE name
/// carries one of the test suffixes (`*.test.*` / `*.spec.*` /
/// `*_test.*` / `*_tests.*`, case-insensitive) or a path component is a
/// `tests` directory.
fn is_test_file(rel: &str) -> bool {
    let lower = rel.replace('\\', "/").to_ascii_lowercase();
    if lower.split('/').any(|c| c == "tests") {
        return true;
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    [".test.", ".spec.", "_test.", "_tests."]
        .iter()
        .any(|suffix| name.contains(suffix))
}

/// The byte-level scan behind [`CodeGraph::find_exact_test_title`]:
/// report the first test call in `bytes` whose ENTIRE quoted literal
/// argument equals `title` (exact byte-for-byte equality between the
/// opening and closing quote — a word-bounded substring, a
/// suffix-composed variant, or a comment mention does not satisfy it).
/// The call must be written in its `call(` form: the call name sits at a
/// word start (the byte before is not an identifier byte —
/// `submit(`/`knit(`/`testify(` never read as `it(`/`test(`) and the
/// byte right after the call name must not be an identifier byte (so
/// `testify(` cannot split into `test` + `ify(`), followed after
/// optional whitespace by the open paren, then a quote.
/// Quotes: `"`, `'`, `` ` `` — the closing quote must match the opening
/// one, and the argument terminator (`,`/`)`) must follow it: an
/// argument built by string concatenation (`it("a" + "b", …)`) is not
/// a single quoted literal, so its runtime title is not `a`.
fn scan_exact_test_title(bytes: &[u8], title: &str) -> Option<&'static str> {
    if title.is_empty() {
        return None;
    }
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    for call in TEST_TITLE_CALLS {
        let opener = call.as_bytes();
        let mut from = 0usize;
        while from + opener.len() <= bytes.len() {
            let Some(at) = bytes[from..]
                .windows(opener.len())
                .position(|w| w == opener)
                .map(|i| i + from)
            else {
                break;
            };
            from = at + 1;
            let before_ok = at == 0 || !is_ident(bytes[at - 1]);
            let after = at + opener.len();
            if !before_ok || after >= bytes.len() || is_ident(bytes[after]) {
                continue;
            }
            // `it(`-form: optional whitespace, the open paren, optional
            // whitespace (including newlines), then the title quote.
            let mut cursor = after;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'(' {
                continue;
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                continue;
            }
            let quote = bytes[cursor];
            if quote != b'"' && quote != b'\'' && quote != b'`' {
                continue;
            }
            cursor += 1;
            let needle = title.as_bytes();
            if bytes.len() < cursor + needle.len() + 1 {
                continue;
            }
            if &bytes[cursor..cursor + needle.len()] != needle {
                continue;
            }
            if bytes[cursor + needle.len()] != quote {
                continue;
            }
            // The argument terminator (`,` or `)`) must follow: an argument
            // built by string concatenation (`it("a" + "b", …)`) is not a
            // single quoted literal, so its runtime title is not `a` — the
            // recorded name must match the whole literal.
            let mut term = cursor + needle.len() + 1;
            while term < bytes.len() && bytes[term].is_ascii_whitespace() {
                term += 1;
            }
            if term < bytes.len() && (bytes[term] == b',' || bytes[term] == b')') {
                return Some(call);
            }
        }
    }
    None
}

/// The latency budget for an inline stale-reindex pass
/// ([`CodeGraph::reindex_stale_files`]) — shared by the content-index
/// (`search`/`search_read`) and symbol-index (`graph_search`) repair paths.
///
/// The pass holds the indexing-flag claim for its whole duration and stops
/// between files once the budget is spent, so a checkout-scale staleness can
/// never parse unbounded inside a tool call: the caller re-queries once and
/// walks (the authoritative engine) if anything remains stale.
pub const STALE_REINDEX_BUDGET: Duration = Duration::from_millis(500);

impl CodeGraph {
    /// Open the on-disk graph for `root` (creating the DB file if needed).
    pub fn open(root: PathBuf, db_path: &Path) -> Result<Self> {
        Ok(Self {
            root,
            store: Arc::new(Mutex::new(Store::open(db_path)?)),
            indexing: AtomicBool::new(false),
            symbol_embed: SymbolEmbedState::default(),
            symbol_search: SymbolSearchState::default(),
        })
    }

    /// Open a transient in-memory graph (tests).
    pub fn open_in_memory(root: PathBuf) -> Result<Self> {
        Ok(Self {
            root,
            store: Arc::new(Mutex::new(Store::open_in_memory()?)),
            indexing: AtomicBool::new(false),
            symbol_embed: SymbolEmbedState::default(),
            symbol_search: SymbolSearchState::default(),
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

    /// Lock the store — [`lock_store`] owns the poisoning-recovery
    /// behaviour (shared with the background symbol-embed pass thread).
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        lock_store(&self.store)
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

    /// Indexed file paths with their as-of-index mtime AND content hash — the
    /// staleness log's classification evidence (backlog fc1d57fe). One SELECT
    /// for the whole tree, like [`Store::stored_mtimes`].
    pub fn stored_file_meta(&self) -> Result<HashMap<String, FileMeta>> {
        self.store().stored_file_meta()
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
        self.index_inner(progress)
    }

    fn index_inner(&self, progress: Option<&dyn Fn(usize, usize)>) -> Result<IndexStats> {
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
                                extract_job(path, rel, stored, &mut parsers)
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
        if reindexed > 0 || stats.files_pruned > 0 {
            // Semantic code search (step 11): the symbol and text rows
            // moved — the in-memory query snapshot is stale.
            self.bump_symbol_search();
        }
        // The background symbol-embed pass (plan d3336a8a step 9): armed
        // graphs schedule it after every index pass — it embeds (or
        // lazily re-embeds) whatever is pending and exits when the queue
        // drains. No-op on unarmed graphs (worktree seeds) and in light
        // builds.
        self.schedule_symbol_embed_pass();
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
    ///
    /// The pass is BUDGET-BOUNDED: `budget` caps how long the caller is
    /// willing to block, checked between files — once it is spent the
    /// remaining paths are simply left stale and the returned count reflects
    /// only the files actually refreshed. The caller re-queries once and
    /// walks if anything remains stale, so an overrun degrades to the
    /// authoritative walk instead of a slow tool call. A zero budget
    /// therefore refreshes nothing (the unit test pins that).
    pub fn reindex_stale_files(&self, rel_paths: &[String], budget: Duration) -> Result<usize> {
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
        // The budget clock. Started after the flag claim, so the claim
        // itself (which can only fail instantly) is never charged to the
        // budget.
        let start = Instant::now();
        let mut refreshed = 0usize;
        let mut touched_source = false;
        let mut pruned = false;
        for rel in rel_paths {
            // Budget gate — checked BEFORE each file, so a spent budget
            // stops the pass without starting another file's parse. What is
            // left stays stale: the caller's re-query sees it and walks
            // (the authoritative engine), and the watcher's next pass
            // refreshes the index shortly.
            if start.elapsed() >= budget {
                break;
            }
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
            let hash = content_hash(&bytes);
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
        if touched_source || pruned {
            // Semantic code search (step 11): re-parsed or pruned rows make
            // the in-memory query snapshot stale.
            self.bump_symbol_search();
        }
        // Same step-9 scheduling as the index pass above: the inline
        // refresh changed text rows, so pending symbols are embedded.
        self.schedule_symbol_embed_pass();
        Ok(refreshed)
    }

    /// Arm the background symbol-embed pass (plan d3336a8a step 9):
    /// after this, every [`Self::index`] / [`Self::reindex_stale_files`]
    /// schedules a pass over symbols whose text hash has no vector for
    /// the current model — batches of [`SYMBOL_EMBED_BATCH`] through
    /// [`Embedder::embed_batch`](crate::memory::embedder::Embedder::embed_batch).
    /// Called once by the brain right after open, BEFORE the graph is
    /// shared (`&mut self`); seed graphs are never armed, so worktree
    /// seeding never spawns passes.
    ///
    /// `cache_dir` is the model cache dir (the same `<config>/models`
    /// the memory embedder uses — the shared host serves both models).
    /// `memory_model` is the configured bundled memory model
    /// (`general.bundled_embedding_model`): when a pass SPAWNS the
    /// shared host, the host's serve list includes it too, so the
    /// memory side later attaches to the same host instead of spawning a
    /// second one. The list only matters on the spawn leg — a host
    /// already serving the code model is attached by the exact-set rule
    /// regardless.
    #[cfg(feature = "embeddings")]
    pub fn arm_symbol_embeddings(&mut self, cache_dir: &Path, memory_model: Option<&str>) {
        self.symbol_embed.armed = Some(ArmedEmbedder::Real {
            cache_dir: cache_dir.to_path_buf(),
            memory_model: memory_model.map(str::to_string),
        });
    }

    /// The test seam (plan d3336a8a step 9): arm with a stub embedder —
    /// fixed dim, deterministic vectors — so pass tests never download a
    /// model or touch any host (the no-download rule). Generic over the
    /// embedder so call sites pass their concrete stub Arcs. `pub(crate)`
    /// since step 11: the `find_code` ranking tests arm it too.
    #[cfg(all(test, feature = "embeddings"))]
    pub(crate) fn arm_symbol_embedder_stub<E>(&mut self, embedder: Arc<E>)
    where
        E: crate::memory::embedder::Embedder + 'static,
    {
        self.symbol_embed.armed = Some(ArmedEmbedder::Stub(embedder));
    }

    /// The test seam for an ALREADY-BUILT embedder (plan d3336a8a step 11):
    /// the eval arms the real code model through it (the generic stub arm
    /// needs a concrete type, the factory answers a trait object).
    #[cfg(all(test, feature = "embeddings"))]
    pub(crate) fn arm_symbol_embedder_dyn(
        &mut self,
        embedder: Arc<dyn crate::memory::embedder::Embedder>,
    ) {
        self.symbol_embed.armed = Some(ArmedEmbedder::Stub(embedder));
    }

    /// Schedule the background symbol-embed pass when the graph is armed.
    /// Light build (no `embeddings` feature): no pass exists, so this is
    /// a no-op and the call sites stay cfg-free.
    #[cfg(not(feature = "embeddings"))]
    fn schedule_symbol_embed_pass(&self) {}

    /// Schedule the background symbol-embed pass when the graph is armed
    /// (plan d3336a8a step 9) — single-flight: a pass already running
    /// keeps running (it re-reads the pending queue every batch), a
    /// trigger for an unarmed graph is a no-op, and a failure to even
    /// create the thread releases the flag so a later index retries.
    #[cfg(feature = "embeddings")]
    fn schedule_symbol_embed_pass(&self) {
        let Some(armed) = self.symbol_embed.armed.clone() else {
            return;
        };
        // The swap claims the single-flight flag; an already-running pass
        // swallows this trigger.
        if self.symbol_embed.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let stop = Arc::clone(&self.symbol_embed.stop);
        let running = Arc::clone(&self.symbol_embed.running);
        let store = Arc::clone(&self.store);
        // Semantic code search (step 11): the pass announces stored vectors
        // so an in-memory query snapshot reloads instead of ranking stale.
        let generation = Arc::clone(&self.symbol_search.generation);
        let spawned = std::thread::Builder::new()
            .name("codegraph-symbol-embed".to_string())
            .spawn(move || {
                // Review finding panicStuckRunning: a panic inside the
                // pass (a misbehaving embedder) must not leave the
                // single-flight flag stuck for the rest of the process —
                // the guard clears it on scope exit, unwind or return.
                let _running = RunningFlagGuard(Arc::clone(&running));
                run_symbol_embed_pass(armed, store, Arc::clone(&stop), generation);
            });
        if spawned.is_err() {
            // Resource exhaustion — best-effort pass, never fatal. Free
            // the single-flight flag so the next index can try again.
            eprintln!("codegraph: could not start the symbol-embed pass");
            self.symbol_embed.running.store(false, Ordering::Release);
        }
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
            let hash = content_hash(&bytes);
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
        if touched_source {
            // Semantic code search (step 11): re-parsed rows make the
            // in-memory query snapshot stale — it would serve vanished ids
            // and old line ranges, and the new text rows need vectors. Same
            // two calls as `index` / `reindex_stale_files`, made after the
            // store lock is released.
            self.bump_symbol_search();
        }
        // Same step-9 scheduling as the index pass: the refresh changed text
        // rows, so pending symbols are embedded.
        self.schedule_symbol_embed_pass();
        Ok(ReindexOutcome {
            matched,
            matched_unparsed,
            reparsed,
            capped,
        })
    }

    /// The exact test-title check behind the finish gate's string-titled
    /// tier (backlog 5f3f5123, live case: plan 3d40e529). Walks the
    /// searchable disk (the same pattern as
    /// [`Self::reindex_files_containing`]) restricted to TEST files
    /// ([`is_test_file`]) and returns the first file whose source carries
    /// an `it()`/`test()`/`describe()` call whose ENTIRE quoted literal
    /// argument equals `title` ([`scan_exact_test_title`]). A fresh disk
    /// walk is byte-identical to the store's fresh upsert rows: the finish
    /// gate runs this AFTER its forced re-index escalation (backlog
    /// 648e0ad5), so disk truth and store truth agree — no store-schema
    /// dependency is added. Exact equality ONLY: a suffix paraphrase, a
    /// partial title, or a comment mention never satisfies the check (the
    /// anti-paraphrase contract of the symbol-name containment grounds
    /// this fn).
    ///
    /// The scan is byte-exact with NO escape unescaping: the recorded
    /// name must be the title literal AS WRITTEN in source — an escaped
    /// inner quote (`it("says \"hi\"")`) or a template-literal `${…}`
    /// interpolation appears in the source with its escape bytes, so
    /// recording the runtime title (`says "hi"`) rejects. The
    /// conservative contract lets the agent self-correct in one shot
    /// with the gate's both-forms hint (LOW-2, review round 1).
    pub fn find_exact_test_title(&self, title: &str) -> Result<Option<TestTitleHit>> {
        if title.is_empty() {
            return Ok(None);
        }
        let root_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        for path in walk_searchable(&self.root) {
            let Some(rel) = relpath(&self.root, &path) else {
                continue;
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if !ext.as_deref().is_some_and(is_code_extension) {
                continue;
            }
            if !is_test_file(&rel) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Some(call) = scan_exact_test_title(&bytes, title) {
                // Defense-in-depth (mirroring reindex_files_containing):
                // never read content from outside the project root —
                // canonicalize failure skips the file exactly like the
                // precedent, no lenient fall-through (LOW-1, review round 1).
                let Ok(canon) = path.canonicalize() else {
                    eprintln!("codegraph: skipping {rel}: unreadable path");
                    continue;
                };
                if !canon.starts_with(&root_canon) {
                    eprintln!("codegraph: skipping {rel}: outside the project root");
                    continue;
                }
                return Ok(Some(TestTitleHit { path, call }));
            }
        }
        Ok(None)
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
                        RefKind::MethodCall => "method_call",
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
        let hash = content_hash(&bytes);
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
    let disk_mtime = mtime_of(path);
    let meta = stored.get(rel);
    let stored_hash = meta.and_then(|m| m.content_hash.as_deref());
    let stored_mtime = meta.and_then(|m| m.mtime);
    let has_content = meta.map(|m| m.has_content).unwrap_or(false);
    let indexed_version = meta.and_then(|m| m.extract_version);
    // The extract-version clause keeps the fast path honest for the
    // version backfill (plan 0d53d659): an unchanged mtime alone must not
    // skip a file whose rows predate the current extractor.
    if stored_mtime == Some(disk_mtime)
        && has_content
        && stored_hash.is_some()
        && indexed_version == Some(schema::EXTRACTOR_VERSION)
    {
        // Unchanged since last index — already indexed with identical
        // content. Skip the read+hash+reparse.
        return None;
    }

    // Hash the content; unreadable files are skipped, not fatal.
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("codegraph: skipping unreadable file {}", path.display());
        return None;
    };
    let hash = content_hash(&bytes);

    // Re-parse when the content changed OR when the file predates
    // the content index (no cg_content_meta row) OR when its rows were
    // written by an older extractor (no/mismatched `cg_extract_meta`
    // version). The latter two make a plain index() pass backfill old
    // DBs — and every app startup already runs one in the background,
    // so existing projects gain the content index and the ref-semantics
    // change with no manual step. The meta rows are written for every
    // upsert (even empty files), so this fires once per old file,
    // never perpetually.
    let needs_reindex = stored_hash != Some(hash.as_str())
        || !has_content
        || indexed_version != Some(schema::EXTRACTOR_VERSION);
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
                            RefKind::MethodCall => "method_call",
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

/// The content hash the index stores for a file: std `DefaultHasher` over the
/// raw bytes, hex-formatted — the same hasher the indexer has always used,
/// and what [`Store::stored_file_meta`]'s `content_hash` carries. Kept as ONE
/// function so the indexer and the staleness log's disk probe can never drift
/// apart (backlog fc1d57fe): the probe's classification compares like for
/// like.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("{:x}", hasher.finish())
}

// ============ Background symbol-embed pass (plan d3336a8a step 9) ============
// `SymbolEmbedState`/`ArmedEmbedder` live at the struct above; the
// scheduling methods inside `impl CodeGraph` sit right after
// `reindex_stale_files`. This section holds the pass body and the
// process-global embedder slot.

/// How many symbols one embed batch carries (the step-9 rule: 64 at a
/// time, yielding between batches).
#[cfg(feature = "embeddings")]
const SYMBOL_EMBED_BATCH: usize = 64;

/// Clears the pass's single-flight flag when the pass thread ends —
/// normal return or panic (review finding panicStuckRunning: Drop runs
/// on unwind too, so a panicking embedder must not disable the graph's
/// pass for the rest of the process).
#[cfg(feature = "embeddings")]
struct RunningFlagGuard(Arc<AtomicBool>);

#[cfg(feature = "embeddings")]
impl Drop for RunningFlagGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The background pass body (plan d3336a8a step 9): builds the embedder
/// per the armed source — the real path builds it ONCE per process ON
/// THIS THREAD (attach-or-spawn of the shared host, or an in-process
/// ONNX load, possibly a first ~600 MB download: never on a tokio
/// worker, never under the store lock, never on the index path) — then
/// loops batches of [`SYMBOL_EMBED_BATCH`]: read the pending batch in
/// one short lock, RELEASE the store, embed, write the vectors in one
/// short transaction, yield between batches. The stop flag (set by
/// [`CodeGraph`]'s drop) is polled between batches, so a dropped graph
/// ends the pass promptly.
///
/// Never stores junk: only the real code model with a non-`Failed`
/// status runs, and an all-zero batch (the degraded-embedder contract)
/// is dropped, not persisted.
#[cfg(feature = "embeddings")]
fn run_symbol_embed_pass(
    armed: ArmedEmbedder,
    store: Arc<Mutex<Store>>,
    stop: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
) {
    let embedder = match &armed {
        #[cfg(test)]
        ArmedEmbedder::Stub(embedder) => Arc::clone(embedder),
        ArmedEmbedder::Real {
            cache_dir,
            memory_model,
        } => match code_embedder_slot::acquire(cache_dir, memory_model.as_deref()) {
            Some(embedder) => embedder,
            None => {
                // Not the code model (a fallback embedder), a Failed
                // build, or the retry window — skip silently, one line
                // per process.
                code_embedder_slot::log_skip_once(
                    "code embedder unavailable (fallback, failed load, or retry window)",
                );
                return;
            }
        },
    };
    // Belt and braces: the stub seam may carry any model id, so the
    // model/status check lives on the pass path too, not only in the
    // slot. The vec table's model column must always name the real
    // model (the step-9 rule).
    if embedder.model_id() != crate::memory::embedder::CODE_EMBEDDING_MODEL
        || matches!(
            embedder.status(),
            Some(crate::memory::embedder::EmbedderStatus::Failed)
        )
    {
        code_embedder_slot::log_skip_once("embedder is not the code model or reports Failed");
        return;
    }
    let model = embedder.model_id().to_string();
    let dim = embedder.dim();
    // embed_batch is async (HTTP for the shared host; the in-process
    // fallback internally hops to the blocking pool) — the pass gets
    // its own minimal runtime on this dedicated thread.
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        eprintln!("codegraph: symbol-embed pass could not build its runtime");
        return;
    };
    runtime.block_on(async move {
        let mut stored = 0usize;
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            // Read the batch under one short lock, RELEASE before the
            // embed call — the write connection is never held across an
            // embed.
            let batch = match lock_store(&store).pending_symbols(&model, SYMBOL_EMBED_BATCH) {
                Ok(batch) => batch,
                Err(e) => {
                    eprintln!("codegraph: symbol-embed batch read failed: {e}");
                    break;
                }
            };
            if batch.is_empty() {
                break;
            }
            let texts: Vec<String> = batch.iter().map(|s| s.text.clone()).collect();
            let vectors = embedder.embed_batch(&texts).await;
            // Review finding midPassHashJunk: re-check the status EVERY
            // batch, after the embed call. A mid-pass degradation — the
            // shared host died and the in-process load failed — keeps
            // ANSWERING, as the hash fallback's deterministic, NON-ZERO,
            // right-dim vectors under the code model's id, so neither
            // the model check, the dim guard nor the zero guard catches
            // it; only the status flip to Failed does. Drop the batch,
            // stop the pass — the next pass retries after the host or
            // model recovers.
            if matches!(
                embedder.status(),
                Some(crate::memory::embedder::EmbedderStatus::Failed)
            ) {
                code_embedder_slot::log_skip_once("embedder degraded mid-pass (status Failed)");
                break;
            }
            if vectors.iter().all(|v| v.iter().all(|f| *f == 0.0)) {
                // The bundled embedder's own failure contract answers
                // zeros: persisting them would be junk, and a zero
                // vector is indistinguishable from no signal in cosine.
                // Stop — the next pass retries after recovery.
                code_embedder_slot::log_skip_once("embedder answered zero vectors");
                break;
            }
            match lock_store(&store).store_symbol_vecs(&model, dim, &batch, &vectors) {
                Ok(0) => {
                    // Review finding zeroProgressLoop: nothing stored for
                    // a NON-empty batch (wrong-length vectors, or every
                    // symbol vanished mid-batch) — re-reading the queue
                    // would return the very same batch in a hot loop.
                    // Stop; the next pass retries from a fresh queue.
                    code_embedder_slot::log_skip_once("a non-empty batch stored nothing");
                    break;
                }
                Ok(n) => {
                    stored += n;
                    // Semantic code search (step 11): the new vectors are
                    // rankable now — the query snapshot reloads.
                    generation.fetch_add(1, Ordering::AcqRel);
                }
                Err(e) => {
                    eprintln!("codegraph: symbol-embed store failed: {e}");
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
        if stored > 0 {
            eprintln!("codegraph: embedded {stored} symbols for semantic code search");
        }
    });
}

/// The process-global code-embedder slot (plan d3336a8a step 9): built
/// lazily ONCE per process, on the pass thread, kept for the process
/// lifetime. A FAILED build is never cached — only its retry gate is:
/// the next pass may rebuild no sooner than [`retry_window`] later (a
/// host may come up, a download may finish; the 10-minute rule).
#[cfg(feature = "embeddings")]
mod code_embedder_slot {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::memory::embedder::{Embedder, EmbedderStatus};

    use super::CodeEmbedderState;

    /// How a fresh embedder is built. The default wraps
    /// [`crate::provider::client_factory::build_code_embedder`]; tests
    /// install their own (the no-download seam).
    pub(super) type Build = Box<dyn Fn(&Path, Option<&str>) -> Arc<dyn Embedder> + Send + Sync>;

    /// The parked slot: a good embedder, or the retry gate a failed
    /// build left behind (with why it parked — finding
    /// parkedNoteEdges).
    struct Slot {
        embedder: Option<Arc<dyn Embedder>>,
        retry_at: Option<Instant>,
        park_reason: Option<super::ParkReason>,
    }

    static SLOT: Mutex<Slot> = Mutex::new(Slot {
        embedder: None,
        retry_at: None,
        park_reason: None,
    });
    static BUILDER: Mutex<Option<Build>> = Mutex::new(None);
    static SKIP_LOGGED: AtomicBool = AtomicBool::new(false);
    static RETRY_WINDOW_SECS: AtomicU64 = AtomicU64::new(DEFAULT_RETRY_WINDOW_SECS);
    /// True while a query started the shared build (plan d3336a8a step 11)
    /// — dedupes those background builds, which the pass's own acquire
    /// would otherwise run again.
    static BUILDING: AtomicBool = AtomicBool::new(false);

    /// The retry window (10 minutes); tests shrink it via the injected
    /// interval.
    pub(super) const DEFAULT_RETRY_WINDOW_SECS: u64 = 600;

    /// The retry window a failed build parks behind — the injected
    /// interval (the lead's "inject the clock or the interval").
    pub(super) fn retry_window() -> Duration {
        Duration::from_secs(RETRY_WINDOW_SECS.load(Ordering::Relaxed))
    }

    /// Acquire the process-global code embedder: a cached good one
    /// clones out instantly; a fresh build runs on THIS thread when no
    /// live retry gate parks it and none is already running. The build
    /// runs OUTSIDE the slot lock (finding slotLockAcrossRetry, plan
    /// d3336a8a step 17): with the acquire retries it can take up to
    /// about a minute, and `cached`/`state` readers must never wait
    /// behind it — they see `BUILDING` and answer at once. A build
    /// already in flight (the query-started shared build, or another
    /// round) is WAITED FOR, bounded by [`PASS_BUILD_WAIT`] (finding
    /// skippedPassNotRescheduled): on a quiet project nothing else
    /// would re-run the pass when that build lands, so its result must
    /// reach this round. `None` (a live gate parked, or the wait
    /// expired) when the build is not the code model or reports
    /// `Failed` — junk vectors are never stored, and a failed build is
    /// never cached.
    pub(super) fn acquire(
        cache_dir: &Path,
        memory_model: Option<&str>,
    ) -> Option<Arc<dyn Embedder>> {
        let deadline = Instant::now() + PASS_BUILD_WAIT;
        loop {
            let claimed = {
                let slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(embedder) = &slot.embedder {
                    return Some(Arc::clone(embedder));
                }
                if slot.retry_at.is_some_and(|at| Instant::now() < at) {
                    return None;
                }
                // Claim the build under the same lock hold that checked the
                // cache and gate; the early returns above are fine — only
                // build_and_store re-takes the (non-reentrant) lock, so it
                // must run OUTSIDE this block (finding slotLockAcrossRetry).
                !BUILDING.swap(true, Ordering::AcqRel)
            };
            if claimed {
                return build_and_store(cache_dir, memory_model);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(BUILD_POLL);
        }
    }

    /// How long a pass round waits for an in-flight query-started build
    /// before skipping the round (finding skippedPassNotRescheduled):
    /// about the build's own worst-case budget, so the result reaches
    /// this round instead of a someday index event.
    const PASS_BUILD_WAIT: Duration = Duration::from_secs(120);

    /// The wait loop's poll cadence.
    const BUILD_POLL: Duration = Duration::from_millis(100);

    /// The build + store half of [`acquire`], run OUTSIDE the slot lock.
    /// The caller has claimed `BUILDING`; the [`BuildingGuard`] clears
    /// it on ANY exit — return or panic unwind (finding
    /// buildingFlagPanicWedge, the same discipline as the pass's
    /// `RunningFlagGuard`): a panic inside the build must not wedge the
    /// slot into "always building" — every later acquire would answer
    /// `None` and every note would say "still being built" for the rest
    /// of the process.
    fn build_and_store(cache_dir: &Path, memory_model: Option<&str>) -> Option<Arc<dyn Embedder>> {
        let _building = BuildingGuard;
        let embedder = build(cache_dir, memory_model);
        let good = embedder.model_id() == crate::memory::embedder::CODE_EMBEDDING_MODEL
            && !matches!(embedder.status(), Some(EmbedderStatus::Failed));
        let mut slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
        if good {
            slot.embedder = Some(Arc::clone(&embedder));
            slot.retry_at = None;
            slot.park_reason = None;
        } else {
            slot.retry_at = Some(Instant::now() + retry_window());
            // Why the gate parked (finding parkedNoteEdges (a)): a
            // serveable host that answered nothing, or an offline load
            // that failed — the fallback note blames the right thing.
            slot.park_reason = Some(
                if crate::provider::client_factory::host_is_serveable(cache_dir) {
                    super::ParkReason::HostNotAnswering
                } else {
                    super::ParkReason::NotLoaded
                },
            );
        }
        good.then_some(embedder)
    }

    /// Clears the shared `BUILDING` flag on scope exit — unwind or return
    /// (finding buildingFlagPanicWedge).
    struct BuildingGuard;

    impl Drop for BuildingGuard {
        fn drop(&mut self) {
            BUILDING.store(false, Ordering::Release);
        }
    }

    /// The cached good build, WITHOUT building one (plan d3336a8a step 11)
    /// — the instant path a `find_code` query uses. `None` when the slot is
    /// empty or parked behind a retry gate. NEVER waits on a build: builds
    /// run outside the slot lock (finding slotLockAcrossRetry), so this
    /// only ever holds the lock long enough to read one field.
    pub(super) fn cached() -> Option<Arc<dyn Embedder>> {
        let slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
        slot.embedder.as_ref().map(Arc::clone)
    }

    /// The availability snapshot for find_code's fallback note (plan
    /// d3336a8a step 17, finding notReadyNoteMisleads): a parked retry
    /// gate must not read as "still being built" — nothing is building,
    /// and the note says the model is unavailable and when the next
    /// round retries. An EXPIRED gate is not `Parked` either (finding
    /// parkedNoteEdges (c)): the window has lapsed, a query's
    /// `ensure_build` starts the retry, and the note must not promise a
    /// retry that already happened. Like [`cached`], never waits on a
    /// build.
    pub(super) fn state() -> CodeEmbedderState {
        let slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
        if slot.embedder.is_some() {
            return CodeEmbedderState::Ready;
        }
        if BUILDING.load(Ordering::Acquire) {
            return CodeEmbedderState::Building;
        }
        match slot.retry_at {
            Some(at) if Instant::now() < at => CodeEmbedderState::Parked {
                retry_in: at.saturating_duration_since(Instant::now()),
                reason: slot
                    .park_reason
                    .unwrap_or(super::ParkReason::HostNotAnswering),
            },
            _ => CodeEmbedderState::Unarmed,
        }
    }

    /// Start the shared build on its own thread and return at once (plan
    /// d3336a8a step 11): a query that finds no cached embedder must never
    /// wait for the tens-of-seconds build. One build at a time; a LIVE
    /// retry gate is left alone (the pass retries it once the window
    /// expires, a query does not), but an EXPIRED gate is retried NOW
    /// (finding parkedNoteEdges (c)): the fallback note promises "it
    /// retries in about N minutes", so a query after the window must
    /// really start the retry. The `BUILDING` claim happens inside the
    /// SAME lock hold as the cache/gate check (finding
    /// ensureBuildSwapRace): a build that lands between a separate check
    /// and a separate swap would otherwise be rebuilt redundantly. The
    /// thread builds outside the slot lock like [`acquire`] does (finding
    /// slotLockAcrossRetry).
    pub(super) fn ensure_build(cache_dir: &Path, memory_model: Option<&str>) {
        {
            let slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
            if slot.embedder.is_some() {
                return;
            }
            if slot.retry_at.is_some_and(|at| Instant::now() < at) {
                return;
            }
            if BUILDING.swap(true, Ordering::AcqRel) {
                return;
            }
        }
        let cache_dir = cache_dir.to_path_buf();
        let memory_model = memory_model.map(str::to_string);
        let spawned = std::thread::Builder::new()
            .name("codegraph-code-embedder".to_string())
            .spawn(move || {
                // BUILDING is already ours (the swap above) — the build
                // runs outside the slot lock, and build_and_store's guard
                // clears the flag when the result lands, panic or return.
                let _ = build_and_store(&cache_dir, memory_model.as_deref());
            });
        if spawned.is_err() {
            // Resource exhaustion — best-effort, never fatal; the pass
            // builds on its own thread at the next index.
            BUILDING.store(false, Ordering::Release);
        }
    }

    /// Log one skip reason per process (the step's "skip silently, one
    /// info line per process").
    pub(super) fn log_skip_once(reason: &str) {
        if !SKIP_LOGGED.swap(true, Ordering::Relaxed) {
            eprintln!("codegraph: symbol embedding skipped — {reason}");
        }
    }

    fn build(cache_dir: &Path, memory_model: Option<&str>) -> Arc<dyn Embedder> {
        let guard = BUILDER.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(build) => build(cache_dir, memory_model),
            None => crate::provider::client_factory::build_code_embedder(cache_dir, memory_model),
        }
    }

    /// Test-only control over the process-global slot: the slot is
    /// shared by every test in the binary, so tests touching it
    /// serialize through `SLOT_TEST_GUARD` (see mod tests) and reset on
    /// both ends.
    #[cfg(test)]
    pub(crate) mod test {
        use super::*;

        pub(crate) fn reset() {
            *SLOT.lock().unwrap_or_else(|p| p.into_inner()) = Slot {
                embedder: None,
                retry_at: None,
                park_reason: None,
            };
            *BUILDER.lock().unwrap_or_else(|p| p.into_inner()) = None;
            SKIP_LOGGED.store(false, Ordering::Relaxed);
            BUILDING.store(false, Ordering::Relaxed);
            RETRY_WINDOW_SECS.store(DEFAULT_RETRY_WINDOW_SECS, Ordering::Relaxed);
        }

        pub(crate) fn install_builder(build: Build) {
            *BUILDER.lock().unwrap_or_else(|p| p.into_inner()) = Some(build);
        }

        pub(crate) fn set_retry_window(secs: u64) {
            RETRY_WINDOW_SECS.store(secs, Ordering::Relaxed);
        }

        /// Park the retry gate NOW (the note tests): the state a failed
        /// build leaves behind, without running a build. `window` is how
        /// long the gate stays live — `Duration::ZERO` is an instantly
        /// expired gate (finding parkedNoteEdges (c)); `reason` is what
        /// the note blames (finding parkedNoteEdges (a)).
        pub(crate) fn park_gate_for_test(
            reason: crate::codegraph::ParkReason,
            window: std::time::Duration,
        ) {
            let mut slot = SLOT.lock().unwrap_or_else(|p| p.into_inner());
            slot.retry_at = Some(Instant::now() + window);
            slot.park_reason = Some(reason);
        }

        /// Fake a build in flight (the slotLockAcrossRetry and note tests).
        pub(crate) fn set_building_for_test(building: bool) {
            BUILDING.store(building, Ordering::Release);
        }

        /// Whether a build is in flight (the promptness test polls this).
        pub(crate) fn building() -> bool {
            BUILDING.load(Ordering::Acquire)
        }
    }
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
    fn find_exact_test_title_resolves_string_titled_calls() {
        // Backlog 5f3f5123 (live case: plan 3d40e529): a string-titled
        // vitest/Jest test (`it("title", () => …)`) produces no symbol rows —
        // the anonymous callback body is not indexed — so the graph can
        // never resolve the title, yet the test exists and is runnable.
        // find_exact_test_title resolves the EXACT full title literal of an
        // it()/test()/describe() call, restricted to test files
        // (*.test.* / *.spec.* / *_test.* / *_tests.* names or a tests/
        // directory).
        let title = "regression (backlog 076f601e): a late text_delta after \
                     finished settles into the transcript — never resurrects \
                     the plain streaming block";
        let dir = tempfile::tempdir().unwrap();
        // The live-case title, as an it() argument in a .test.ts fixture.
        std::fs::write(
            dir.path().join("use_agent_store.test.ts"),
            format!("it({title:?}, () => {{\n  // assertions\n}});\n"),
        )
        .unwrap();
        // A tests/ directory counts as a test file too.
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("tests").join("late_delta.ts"),
            format!("describe({title:?}, () => {{\n  it(\"inner\", () => {{}});\n}});\n"),
        )
        .unwrap();
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        let hit = graph.find_exact_test_title(title).unwrap().unwrap();
        assert!(
            hit.call == "it" || hit.call == "describe",
            "the hit names the call carrying the title: {:?}",
            hit
        );
        let hit_text = hit.path.to_string_lossy().replace('\\', "/");
        assert!(
            hit_text.ends_with("use_agent_store.test.ts")
                || hit_text.ends_with("tests/late_delta.ts"),
            "the hit lands in one of the two test fixtures: {:?}",
            hit.path
        );
    }

    #[test]
    fn find_exact_test_title_rejects_paraphrase_partial_comment_non_test() {
        // The anti-paraphrase contract (the gate's precision is the point):
        // a suffix paraphrase, a partial title, and a comment-only mention
        // must ALL reject — only the entire quoted literal argument of an
        // it()/test()/describe() call in a TEST file satisfies the check.
        let title = "regression (backlog 076f601e): a late text_delta after \
                     finished settles into the transcript — never resurrects \
                     the plain streaming block";
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        // Suffix paraphrase: the on-disk title TOOK extra trailing words.
        std::fs::write(
            dir.path().join("a.test.ts"),
            format!(
                "it({title:?} + \" — with extra trailing words\", () => {{}});\n"
            ),
        )
        .unwrap();
        // Comment-only mention in an otherwise call-free test file.
        std::fs::write(
            dir.path().join("b.test.ts"),
            format!("// {title}\n// mentioned only in a comment\n"),
        )
        .unwrap();
        // The EXACT it() title in a NON-test file (src/, not a tests/
        // component, name without .test./.spec./_test./_tests.).
        std::fs::write(
            dir.path().join("src").join("main.ts"),
            format!("it({title:?}, () => {{}});\n"),
        )
        .unwrap();
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        assert!(
            graph.find_exact_test_title(title).unwrap().is_none(),
            "a comment-only mention must reject"
        );
        assert!(
            graph
                .find_exact_test_title(&format!("{title} — with extra trailing words"))
                .unwrap()
                .is_none(),
            "a suffix-composed title is not an on-disk exact literal"
        );
        // Word-boundary discipline (the contains_whole_identifier
        // precedent): `submit(`/`testify(` must not read as `test(`
        // with a title argument.
        std::fs::write(
            dir.path().join("c.test.ts"),
            format!("submit({title:?});\ntestify({title:?});\n"),
        )
        .unwrap();
        assert!(
            graph.find_exact_test_title(title).unwrap().is_none(),
            "non-call word shapes must reject"
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
        let n = graph.reindex_stale_files(&["src/lib.rs".to_string()], Duration::from_secs(60)).unwrap();
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
        let n = graph.reindex_stale_files(&["app.ts".to_string()], Duration::from_secs(60)).unwrap();
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
        let n = graph.reindex_stale_files(&["src/lib.rs".to_string()], Duration::from_secs(60)).unwrap();
        assert_eq!(n, 0, "busy claim → no-op");
        assert!(graph.is_indexing(), "someone else's flag is untouched");
        // And nothing was refreshed — the new content is not indexed yet.
        assert!(graph.search_content("busy_marker", 10).unwrap().is_empty());

        graph.set_indexing(false); // the pass is done
        let n = graph.reindex_stale_files(&["src/lib.rs".to_string()], Duration::from_secs(60)).unwrap();
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
            .reindex_stale_files(
                &["../cg-outside-secret/secret.txt".to_string()],
                Duration::from_secs(60),
            )
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
    fn reindex_stale_files_respects_the_budget() {
        // The adaptive bound (backlog 9201704f): the pass is budget-bounded,
        // not count-bounded — a spent budget stops it between files and the
        // remainder stays stale for the caller's re-query (and its walk).
        // Duration::ZERO is the deterministic stand-in for "no time left":
        // it is a no-op, which makes the tool-level "budget spent → walk"
        // contract testable without wall-clock timing.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();

        // Two files edited AFTER indexing (the watcher-lag gap).
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() -> u32 { 42 }\npub fn budget_marker_one() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "export function boot() {}\nexport function budget_marker_two() {}\n",
        )
        .unwrap();
        let paths = vec!["src/lib.rs".to_string(), "app.ts".to_string()];

        // No time left — nothing refreshed, neither edit is indexed yet.
        let n = graph.reindex_stale_files(&paths, Duration::ZERO).unwrap();
        assert_eq!(n, 0, "a spent budget refreshes nothing");
        assert!(
            graph.search_content("budget_marker_one", 10).unwrap().is_empty(),
            "the first file is left stale"
        );
        assert!(
            graph.search_content("budget_marker_two", 10).unwrap().is_empty(),
            "the remainder of the set is left stale too"
        );

        // A generous budget processes the whole set.
        let n = graph
            .reindex_stale_files(&paths, Duration::from_secs(60))
            .unwrap();
        assert_eq!(n, 2, "both stale files refreshed");
        assert!(!graph.search_content("budget_marker_one", 10).unwrap().is_empty());
        assert!(!graph.search_content("budget_marker_two", 10).unwrap().is_empty());
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

    #[test]
    fn index_backfills_refs_when_the_extractor_version_moves() {
        // The extractor-semantics backfill (plan 0d53d659), the same shape
        // as the content backfill above: rows written by an older
        // extractor (refs without the method-call shape) cannot be
        // re-derived in place, so a plain index() pass must re-parse every
        // file whose `cg_extract_meta` stamp is missing or old, exactly
        // once, and be quiet afterwards — never perpetually.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        let db = dir.path().join(".coding/codegraph.db");
        {
            let graph = CodeGraph::open(dir.path().to_path_buf(), &db).unwrap();
            graph.index(None).unwrap();
        }
        // Rewind to the pre-version-marker state (an older extractor
        // wrote no cg_extract_meta rows).
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("DELETE FROM cg_extract_meta", []).unwrap();
        }
        let graph = CodeGraph::open(dir.path().to_path_buf(), &db).unwrap();
        let stats = graph.index(None).unwrap();
        assert_eq!(
            stats.files_reindexed, 3,
            "every file re-parsed once to stamp the extractor version"
        );
        // Steady state: the version row is written for every upsert, so
        // the backfill never fires perpetually.
        let stats = graph.index(None).unwrap();
        assert_eq!(stats.files_reindexed, 0);
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

    // ===== Background symbol-embed pass (plan d3336a8a step 9) =====

    /// Serialize tests that mutate the process-global code-embedder slot
    /// (builder, retry window, skip-log gate): the slot is shared by the
    /// whole test binary.
    #[cfg(feature = "embeddings")]
    static SLOT_TEST_GUARD: Mutex<()> = Mutex::new(());

    /// The deterministic stub embedder (the lead's no-download seam):
    /// fixed dim, vectors derived from the text's bytes, a configurable
    /// model id and status, an embed-call counter — and a degrade knob:
    /// after `fail_after_batches` served batches, `status()` reports
    /// Failed (the mid-pass fallback shape, review finding
    /// midPassHashJunk: the hash fallback keeps answering, non-zero and
    /// right-dim, under the code model's id).
    #[cfg(feature = "embeddings")]
    struct StubEmbedder {
        model: String,
        dim: usize,
        status: Option<crate::memory::embedder::EmbedderStatus>,
        calls: AtomicUsize,
        batches: AtomicUsize,
        fail_after_batches: usize,
    }

    #[cfg(feature = "embeddings")]
    impl StubEmbedder {
        fn new(
            model: &str,
            dim: usize,
            status: Option<crate::memory::embedder::EmbedderStatus>,
        ) -> Arc<Self> {
            Arc::new(Self {
                model: model.to_string(),
                dim,
                status,
                calls: AtomicUsize::new(0),
                batches: AtomicUsize::new(0),
                fail_after_batches: 0,
            })
        }

        /// Degrade after `after` served batches: `status()` flips to
        /// Failed from the batch after them on (zero = never degrade).
        fn new_degrading(model: &str, dim: usize, after: usize) -> Arc<Self> {
            Arc::new(Self {
                model: model.to_string(),
                dim,
                status: None,
                calls: AtomicUsize::new(0),
                batches: AtomicUsize::new(0),
                fail_after_batches: after,
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        /// Deterministic pseudo-embedding: the vector depends only on
        /// the text, so re-embed behaviour is observable. The first
        /// element is always non-zero (this stub must never trip the
        /// pass's all-zero guard).
        fn embed_one(&self, text: &str) -> Vec<f32> {
            let mut v = vec![0.0; self.dim];
            v[0] = 1.0;
            for (i, b) in text.bytes().enumerate() {
                v[(i + 1) % self.dim] += (b as f32) / 255.0 - 0.5;
            }
            v
        }
    }

    #[cfg(feature = "embeddings")]
    #[async_trait::async_trait]
    impl crate::memory::embedder::Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> Vec<f32> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.embed_one(text)
        }

        async fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
            self.calls.fetch_add(texts.len(), Ordering::Relaxed);
            self.batches.fetch_add(1, Ordering::Relaxed);
            texts.iter().map(|t| self.embed_one(t)).collect()
        }

        fn dim(&self) -> usize {
            self.dim
        }

        fn model_id(&self) -> &str {
            &self.model
        }

        fn status(&self) -> Option<crate::memory::embedder::EmbedderStatus> {
            if self.fail_after_batches > 0
                && self.batches.load(Ordering::Relaxed) > self.fail_after_batches
            {
                return Some(crate::memory::embedder::EmbedderStatus::Failed);
            }
            self.status.clone()
        }
    }

    /// Run one pass synchronously against an armed graph — the
    /// direct-call seam for deterministic pass tests (the schedule
    /// path's own thread is exercised separately).
    #[cfg(feature = "embeddings")]
    fn run_armed_pass(graph: &CodeGraph) {
        let armed = graph
            .symbol_embed
            .armed
            .clone()
            .expect("the graph must be armed first");
        run_symbol_embed_pass(
            armed,
            Arc::clone(&graph.store),
            Arc::clone(&graph.symbol_embed.stop),
            Arc::clone(&graph.symbol_search.generation),
        );
    }

    /// Step 9: an unchanged symbol is never re-embedded — the second
    /// pass over an identical index embeds nothing (text-hash-keyed
    /// incremental embedding, end to end through index() and the pass).
    #[cfg(feature = "embeddings")]
    #[test]
    fn unchanged_symbol_not_reembedded() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let embedder = StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&embedder));
        run_armed_pass(&graph);
        let first = embedder.calls();
        assert!(first > 0, "the first pass embeds the fixture's symbols");
        let stored = graph.store().symbol_vec_count().unwrap();
        assert_eq!(stored, first, "every embedded symbol stored its vector");
        // The second pass over the unchanged index embeds nothing.
        run_armed_pass(&graph);
        assert_eq!(embedder.calls(), first, "no symbol is re-embedded");
        assert_eq!(graph.store().symbol_vec_count().unwrap(), stored);
    }

    /// Step 9: a model change re-embeds everything lazily — rows
    /// stamped with another model turn pending and are re-embedded by the
    /// next pass (the vec table's model column always names the real
    /// model).
    #[cfg(feature = "embeddings")]
    #[test]
    fn model_change_reembeds() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let model = crate::memory::embedder::CODE_EMBEDDING_MODEL;
        let embedder = StubEmbedder::new(model, 8, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&embedder));
        run_armed_pass(&graph);
        let first = embedder.calls();
        assert!(first > 0);
        // What a future CODE_EMBEDDING_MODEL change leaves behind: the
        // stored rows name the old model.
        graph
            .store()
            .overwrite_symbol_vec_model_for_test("old-code-model")
            .unwrap();
        assert_eq!(
            graph.store().pending_symbols(model, 100).unwrap().len(),
            first,
            "every row of the old model is pending again"
        );
        run_armed_pass(&graph);
        assert_eq!(embedder.calls(), first * 2, "everything is re-embedded");
        assert_eq!(
            graph.store().symbol_vec_count().unwrap(),
            first,
            "INSERT OR REPLACE keeps one row per symbol"
        );
        assert_eq!(
            graph
                .store()
                .symbol_vec("src/lib.rs::helper::1")
                .unwrap()
                .unwrap()
                .model,
            model
        );
    }

    /// Step 9's never-store-junk rule: a pass with an embedder that is
    /// not the code model (the hash fallback's id) or reports Failed
    /// embeds nothing and stores nothing.
    #[cfg(feature = "embeddings")]
    #[test]
    fn pass_skips_embedders_that_are_not_the_code_model() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();

        // The hash fallback's model id — the shape the real path answers
        // with when neither the host nor the in-process model is
        // available.
        let hash = StubEmbedder::new("hash", 8, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&hash));
        run_armed_pass(&graph);
        assert_eq!(hash.calls(), 0);
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);

        // The right model id but a Failed status (a degraded remote
        // whose fallback would answer zeros): still skipped.
        let failed = StubEmbedder::new(
            crate::memory::embedder::CODE_EMBEDDING_MODEL,
            8,
            Some(crate::memory::embedder::EmbedderStatus::Failed),
        );
        graph.arm_symbol_embedder_stub(Arc::clone(&failed));
        run_armed_pass(&graph);
        assert_eq!(failed.calls(), 0);
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);
    }

    /// Step 9: a failed embedder build stores nothing and is never
    /// cached — the retry window parks the rebuild (10 minutes by
    /// default), and the injected zero interval lets the next pass try
    /// again (the lead's clock/interval injection).
    #[cfg(feature = "embeddings")]
    #[test]
    fn failed_build_is_not_cached_and_retries_after_the_window() {
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let builds = Arc::new(AtomicUsize::new(0));
        // A builder that returns a FALLBACK embedder (not the code
        // model, so the build fails the slot's validation) and counts
        // its calls.
        fn count_builder(builds: &Arc<AtomicUsize>) -> code_embedder_slot::Build {
            let counted = Arc::clone(builds);
            Box::new(move |_cache_dir, _memory_model| {
                counted.fetch_add(1, Ordering::Relaxed);
                StubEmbedder::new("hash", 8, None)
            })
        }
        code_embedder_slot::test::install_builder(count_builder(&builds));
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embeddings(Path::new("does-not-matter"), None);
        run_armed_pass(&graph);
        // The failed build stored nothing...
        assert_eq!(builds.load(Ordering::Relaxed), 1, "one build attempt");
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);
        // ...and within the retry window there is no second attempt.
        run_armed_pass(&graph);
        assert_eq!(
            builds.load(Ordering::Relaxed),
            1,
            "the retry window parks the rebuild"
        );
        // A zero window (the injected interval) unparks the slot: the
        // next pass builds again — and the build still fails, still
        // storing nothing, and a follow-up pass may build again too
        // (the gate never re-arms when the window is zero).
        code_embedder_slot::test::reset();
        code_embedder_slot::test::set_retry_window(0);
        code_embedder_slot::test::install_builder(count_builder(&builds));
        run_armed_pass(&graph);
        assert_eq!(
            builds.load(Ordering::Relaxed),
            2,
            "the window's expiry allows a rebuild"
        );
        run_armed_pass(&graph);
        assert_eq!(
            builds.load(Ordering::Relaxed),
            3,
            "a zero window never parks the slot"
        );
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);
        code_embedder_slot::test::reset();
    }

    /// Finding slotLockAcrossRetry (plan d3336a8a step 17): the slot lock
    /// is never held across a build. A deliberately slow build (3 s — far
    /// longer than the old lock-hold) is in progress, and `cached()`
    /// still answers at once with `None` instead of stalling a tokio
    /// worker behind the build; the running build stores its result for
    /// the next round.
    #[cfg(feature = "embeddings")]
    #[test]
    fn cached_answers_promptly_while_a_build_is_in_progress() {
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        code_embedder_slot::test::install_builder(Box::new(move |_cache_dir, _memory_model| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None)
        }));
        let builder = std::thread::spawn(|| {
            code_embedder_slot::acquire(Path::new("does-not-matter"), None)
        });
        // Wait until the slow build is actually in flight.
        let start = std::time::Instant::now();
        while !code_embedder_slot::test::building() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the slow build never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let t0 = std::time::Instant::now();
        let cached = code_embedder_slot::cached();
        let elapsed = t0.elapsed();
        assert!(
            cached.is_none(),
            "no cached embedder exists while the build runs"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "cached() waited {elapsed:?} behind a build — the slot lock must never be held across a build"
        );
        // The running build stores its result for the next round.
        let built = builder.join().unwrap();
        assert!(built.is_some(), "the slow build completed");
        assert!(
            code_embedder_slot::cached().is_some(),
            "the finished build is cached for the next round"
        );
        code_embedder_slot::test::reset();
    }

    /// Finding notReadyNoteMisleads (plan d3336a8a step 17): a parked
    /// retry gate says the model is NOT AVAILABLE and when it retries —
    /// nothing is being built, so the old "still being built (0 of M)"
    /// line would mislead. The gate is parked directly (the test control)
    /// so no build ever runs.
    #[cfg(feature = "embeddings")]
    #[test]
    fn find_code_note_says_unavailable_while_the_gate_is_parked() {
        use crate::tool::Tool;
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embeddings(Path::new("does-not-matter"), None);
        code_embedder_slot::test::park_gate_for_test(
            crate::codegraph::ParkReason::HostNotAnswering,
            std::time::Duration::from_secs(600),
        );
        let tool = crate::tool::agent::find_code::FindCodeTool::new(Some(Arc::new(graph)));
        // Sync test + block_on: the slot guard is a std Mutex and must not
        // be held across an await.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(tool.execute(serde_json::json!({
            "query": "parse the fixture files",
            "limit": 4,
        })));
        assert!(result.success, "the text fallback answers: {result:?}");
        assert!(
            result.output.starts_with(
                "the code search model is not available right now (the shared \
                 model host is not answering); showing text matches; it retries \
                 in about 10 minutes"
            ),
            "the parked note reads: {result:?}"
        );
        code_embedder_slot::test::reset();
    }

    /// The other wording (finding notReadyNoteMisleads): while a build is
    /// actually in progress the note keeps the honest "still being built"
    /// progress line. The building flag is set directly so no build ever
    /// runs (and the query must not wait for one).
    #[cfg(feature = "embeddings")]
    #[test]
    fn find_code_note_keeps_being_built_while_a_build_runs() {
        use crate::tool::Tool;
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embeddings(Path::new("does-not-matter"), None);
        code_embedder_slot::test::set_building_for_test(true);
        let tool = crate::tool::agent::find_code::FindCodeTool::new(Some(Arc::new(graph)));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(tool.execute(serde_json::json!({
            "query": "parse the fixture files",
            "limit": 4,
        })));
        assert!(result.success, "the text fallback answers: {result:?}");
        assert!(
            result.output.starts_with("code embeddings are still being built (0 of"),
            "the building note reads: {result:?}"
        );
        code_embedder_slot::test::reset();
    }

    /// Finding buildingFlagPanicWedge (plan d3336a8a step 17): a panic
    /// inside the build must not wedge `BUILDING` — the Drop guard
    /// clears it on unwind, and a later acquire builds again.
    #[cfg(feature = "embeddings")]
    #[test]
    fn a_panicking_build_leaves_the_slot_buildable() {
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        code_embedder_slot::test::install_builder(Box::new(|_cache_dir, _memory_model| {
            panic!("the injected build panics");
        }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            code_embedder_slot::acquire(Path::new("does-not-matter"), None)
        }));
        assert!(result.is_err(), "the panicking build panics");
        assert!(
            !code_embedder_slot::test::building(),
            "the panic unwound with BUILDING cleared — the slot is not wedged"
        );
        // A later acquire builds again and stores the good result.
        code_embedder_slot::test::install_builder(Box::new(|_cache_dir, _memory_model| {
            StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None)
        }));
        let built = code_embedder_slot::acquire(Path::new("does-not-matter"), None);
        assert!(built.is_some(), "the slot builds again after the panic");
        assert!(code_embedder_slot::cached().is_some());
        code_embedder_slot::test::reset();
    }

    /// Finding skippedPassNotRescheduled (plan d3336a8a step 17): a pass
    /// round that races a query-started build WAITS for it (bounded)
    /// instead of skipping — the build's result reaches THIS round, so
    /// pending symbols are embedded without any new index event, and no
    /// redundant second build runs.
    #[cfg(feature = "embeddings")]
    #[test]
    fn a_pass_round_waits_for_the_query_started_build() {
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let builds = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&builds);
        code_embedder_slot::test::install_builder(Box::new(move |_cache_dir, _memory_model| {
            counted.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(600));
            StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None)
        }));
        // The query-started shared build: owns BUILDING at once, builds
        // in the background.
        code_embedder_slot::ensure_build(Path::new("does-not-matter"), None);
        assert!(
            code_embedder_slot::test::building(),
            "ensure_build claimed the build at once"
        );
        // The pass round, racing the in-flight build: it waits for the
        // good result instead of skipping — and does NOT build again.
        let got = code_embedder_slot::acquire(Path::new("does-not-matter"), None);
        assert!(got.is_some(), "the round waited for the build to land");
        assert_eq!(
            builds.load(Ordering::Relaxed),
            1,
            "no second build — the round reused the query-started one"
        );
        assert!(code_embedder_slot::cached().is_some());
        code_embedder_slot::test::reset();
    }

    /// Finding parkedNoteEdges (a): an OFFLINE park (no host could
    /// serve, the in-process load failed) must not blame the shared
    /// model host — the note says the model could not be loaded.
    #[cfg(feature = "embeddings")]
    #[test]
    fn find_code_note_blames_the_load_when_no_host_could_serve() {
        use crate::tool::Tool;
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embeddings(Path::new("does-not-matter"), None);
        code_embedder_slot::test::park_gate_for_test(
            crate::codegraph::ParkReason::NotLoaded,
            std::time::Duration::from_secs(600),
        );
        let tool = crate::tool::agent::find_code::FindCodeTool::new(Some(Arc::new(graph)));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(tool.execute(serde_json::json!({
            "query": "parse the fixture files",
            "limit": 4,
        })));
        assert!(result.success, "the text fallback answers: {result:?}");
        assert!(
            result.output.starts_with(
                "the code search model could not be loaded; showing text \
                 matches; it retries in about 10 minutes"
            ),
            "the offline note reads: {result:?}"
        );
        code_embedder_slot::test::reset();
    }

    /// Finding parkedNoteEdges (b): the retry tail is grammatical — one
    /// minute singular, and "shortly" for a gate that has all but
    /// expired.
    #[cfg(feature = "embeddings")]
    #[test]
    fn find_code_note_retry_tail_is_worded_correctly() {
        use crate::tool::Tool;
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embeddings(Path::new("does-not-matter"), None);
        let tool = crate::tool::agent::find_code::FindCodeTool::new(Some(Arc::new(graph)));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let note = |window: std::time::Duration| -> String {
            code_embedder_slot::test::park_gate_for_test(
                crate::codegraph::ParkReason::HostNotAnswering,
                window,
            );
            let result = rt.block_on(tool.execute(serde_json::json!({
                "query": "parse the fixture files",
                "limit": 4,
            })));
            result.output.lines().next().unwrap().to_string()
        };
        let minute = note(std::time::Duration::from_secs(60));
        assert!(
            minute.ends_with("it retries in about 1 minute"),
            "the singular tail reads: {minute}"
        );
        let shortly = note(std::time::Duration::from_millis(900));
        assert!(
            shortly.ends_with("it retries shortly"),
            "the expiring tail reads: {shortly}"
        );
        let minutes = note(std::time::Duration::from_secs(600));
        assert!(
            minutes.ends_with("it retries in about 10 minutes"),
            "the plural tail reads: {minutes}"
        );
        code_embedder_slot::test::reset();
    }

    /// Finding parkedNoteEdges (c): an EXPIRED gate is not reported
    /// Parked, and a query really retries it — ensure_build starts the
    /// retry the note promised.
    #[cfg(feature = "embeddings")]
    #[test]
    fn an_expired_gate_is_retried_by_the_next_query() {
        let _guard = SLOT_TEST_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        code_embedder_slot::test::reset();
        let builds = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&builds);
        code_embedder_slot::test::install_builder(Box::new(move |_cache_dir, _memory_model| {
            counted.fetch_add(1, Ordering::Relaxed);
            StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None)
        }));
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        graph.arm_symbol_embeddings(Path::new("does-not-matter"), None);
        // An instantly-expired gate: the state must not report Parked.
        code_embedder_slot::test::park_gate_for_test(
            crate::codegraph::ParkReason::HostNotAnswering,
            std::time::Duration::ZERO,
        );
        assert_eq!(
            graph.code_embedder_state(),
            CodeEmbedderState::Unarmed,
            "an expired gate is not Parked"
        );
        // The next query really retries: ensure_build claims the
        // expired gate and builds.
        code_embedder_slot::ensure_build(Path::new("does-not-matter"), None);
        let start = std::time::Instant::now();
        while builds.load(Ordering::Relaxed) == 0 {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the query-started retry never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(code_embedder_slot::cached().is_some());
        code_embedder_slot::test::reset();
    }

    /// Step 9: the stop flag (set by the graph's Drop) ends a pass at
    /// the next batch boundary — a pre-stopped graph embeds nothing
    /// despite pending symbols, and dropping the graph really sets the
    /// flag.
    #[cfg(feature = "embeddings")]
    #[test]
    fn pass_stops_when_the_graph_drops() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let embedder = StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&embedder));
        // The signal Drop sends, simulated first (the deterministic
        // half): a stopped graph embeds nothing.
        graph.symbol_embed.stop.store(true, Ordering::Release);
        run_armed_pass(&graph);
        assert_eq!(embedder.calls(), 0);
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);
        // The real signal: dropping the graph sets the flag.
        let stop = Arc::clone(&graph.symbol_embed.stop);
        drop(graph);
        assert!(
            stop.load(Ordering::Acquire),
            "CodeGraph's Drop sets the stop flag"
        );
    }

    /// Step 9's scheduling end to end: `schedule_symbol_embed_pass` runs
    /// the pass on its own thread (single-flight), the vectors land, and
    /// the running flag clears when the queue drains.
    #[cfg(feature = "embeddings")]
    #[test]
    fn scheduled_pass_runs_in_background_and_drains() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let embedder = StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 8, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&embedder));
        graph.schedule_symbol_embed_pass();
        // Poll the single-flight flag until the pass finishes (a
        // generous deadline; the fixture is tiny).
        let deadline = Instant::now() + Duration::from_secs(30);
        while graph.symbol_embed.running.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !graph.symbol_embed.running.load(Ordering::Acquire),
            "the scheduled pass finished"
        );
        assert!(embedder.calls() > 0);
        assert_eq!(
            graph.store().symbol_vec_count().unwrap(),
            embedder.calls(),
            "the background pass embedded every pending symbol"
        );
    }

    /// A stub that panics inside its embed calls — a misbehaving embedder
    /// (review finding panicStuckRunning).
    #[cfg(feature = "embeddings")]
    struct PanickingStubEmbedder {
        entries: Arc<AtomicUsize>,
    }

    #[cfg(feature = "embeddings")]
    #[async_trait::async_trait]
    impl crate::memory::embedder::Embedder for PanickingStubEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            self.entries.fetch_add(1, Ordering::Relaxed);
            panic!("a misbehaving embedder");
        }

        async fn embed_batch(&self, _texts: &[String]) -> Vec<Vec<f32>> {
            self.entries.fetch_add(1, Ordering::Relaxed);
            panic!("a misbehaving embedder");
        }

        fn dim(&self) -> usize {
            8
        }

        fn model_id(&self) -> &str {
            crate::memory::embedder::CODE_EMBEDDING_MODEL
        }
    }

    /// Review finding panicStuckRunning: the pass thread's Drop guard
    /// clears the single-flight flag even when the pass panics, so a
    /// later trigger schedules the pass again — a panicking embedder
    /// cannot disable the graph's pass for the rest of the process.
    #[cfg(feature = "embeddings")]
    #[test]
    fn panicking_embedder_does_not_stick_the_single_flight_flag() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let entries = Arc::new(AtomicUsize::new(0));
        graph.arm_symbol_embedder_stub(Arc::new(PanickingStubEmbedder {
            entries: Arc::clone(&entries),
        }));
        graph.schedule_symbol_embed_pass();
        // Wait out the panicking pass: the guard clears the flag on
        // unwind.
        let deadline = Instant::now() + Duration::from_secs(30);
        while graph.symbol_embed.running.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !graph.symbol_embed.running.load(Ordering::Acquire),
            "the guard cleared the flag despite the panic"
        );
        assert_eq!(entries.load(Ordering::Relaxed), 1);
        // The second schedule really runs — the flag was not stuck.
        graph.schedule_symbol_embed_pass();
        let deadline = Instant::now() + Duration::from_secs(30);
        while graph.symbol_embed.running.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            entries.load(Ordering::Relaxed),
            2,
            "the pass runs again after the panicking embedder"
        );
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);
    }

    /// A stub answering non-zero vectors of the WRONG length — the
    /// store's dim-mismatch guard skips every row, so a non-empty batch
    /// stores nothing (review finding zeroProgressLoop).
    #[cfg(feature = "embeddings")]
    struct WrongLengthStubEmbedder {
        batches: AtomicUsize,
    }

    #[cfg(feature = "embeddings")]
    #[async_trait::async_trait]
    impl crate::memory::embedder::Embedder for WrongLengthStubEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            vec![1.0, 2.0]
        }

        async fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
            self.batches.fetch_add(1, Ordering::Relaxed);
            texts.iter().map(|_| vec![1.0, 2.0]).collect()
        }

        fn dim(&self) -> usize {
            // The vectors above are 2-long: the mismatch trips the store
            // guard, and the batch stores nothing.
            8
        }

        fn model_id(&self) -> &str {
            crate::memory::embedder::CODE_EMBEDDING_MODEL
        }
    }

    /// Review finding zeroProgressLoop: a non-empty batch that stores
    /// nothing stops the pass after ONE embed — re-reading the queue
    /// would return the very same batch forever.
    #[cfg(feature = "embeddings")]
    #[test]
    fn pass_stops_when_a_non_empty_batch_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let stub = Arc::new(WrongLengthStubEmbedder {
            batches: AtomicUsize::new(0),
        });
        graph.arm_symbol_embedder_stub(Arc::clone(&stub));
        run_armed_pass(&graph);
        assert_eq!(
            stub.batches.load(Ordering::Relaxed),
            1,
            "exactly one batch embed — no hot loop"
        );
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 0);
        assert_eq!(
            graph
                .store()
                .pending_symbols(crate::memory::embedder::CODE_EMBEDDING_MODEL, 100)
                .unwrap()
                .len(),
            3,
            "the symbols stay pending for the next pass"
        );
    }

    /// Review finding midPassHashJunk: a mid-pass degradation keeps
    /// answering — the hash fallback's deterministic, non-zero,
    /// right-dim vectors under the code model's id — so only the
    /// per-batch status re-check catches it. The batch embedded before
    /// the flip is stored; the batch embedded after it is dropped, and
    /// the pass stops.
    #[cfg(feature = "embeddings")]
    #[test]
    fn pass_stops_and_drops_the_batch_when_the_embedder_degrades_mid_pass() {
        let dir = tempfile::tempdir().unwrap();
        // 100 embeddable fns: two batches at SYMBOL_EMBED_BATCH = 64.
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let mut src = String::new();
        for i in 0..100 {
            src.push_str(&format!("pub fn f{i}() -> u32 {{ {i} }}\n"));
        }
        std::fs::write(src_dir.join("big.rs"), src).unwrap();
        let mut graph = CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        let model = crate::memory::embedder::CODE_EMBEDDING_MODEL;
        assert_eq!(
            graph.store().pending_symbols(model, 1000).unwrap().len(),
            100
        );
        let stub = StubEmbedder::new_degrading(model, 8, 1);
        graph.arm_symbol_embedder_stub(Arc::clone(&stub));
        run_armed_pass(&graph);
        // Batch 1 (64 symbols) stored; batch 2 embedded, then dropped on
        // its status check; nothing after.
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 64);
        assert_eq!(stub.calls(), 100);
        // The pass stopped: the degraded batch's symbols stay pending
        // for the next pass.
        assert_eq!(
            graph.store().pending_symbols(model, 1000).unwrap().len(),
            36
        );
    }

    /// Step 9's migration path end to end: a pre-step-9 DB (its tables
    /// absent, files stamped with the older extractor version) opens,
    /// gains the tables, and the version backfill re-parses the files so
    /// the text rows appear — then the pass embeds them, and a later
    /// index never re-parses them again (never perpetual).
    #[cfg(feature = "embeddings")]
    #[test]
    fn old_db_migrates_and_backfills_text_rows() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let db = dir.path().join("graph.db");
        {
            let graph = CodeGraph::open(dir.path().to_path_buf(), &db).unwrap();
            graph.index(None).unwrap();
        }
        {
            // The pre-step-9 database: no step-9 tables, v1 stamps (a
            // separate connection — the graph is closed).
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "DROP TABLE cg_symbol_text;
                 DROP TABLE cg_symbol_vec;
                 UPDATE cg_extract_meta SET version = 1;",
            )
            .unwrap();
        }
        let mut graph = CodeGraph::open(dir.path().to_path_buf(), &db).unwrap();
        // The version bump forces one re-parse of every v1-stamped file
        // (mtime and hash unchanged), backfilling the text rows.
        let stats = graph.index(None).unwrap();
        assert_eq!(
            stats.files_reindexed, 3,
            "every v1-stamped file is re-parsed once"
        );
        let model = crate::memory::embedder::CODE_EMBEDDING_MODEL;
        let pending = graph.store().pending_symbols(model, 100).unwrap();
        assert_eq!(pending.len(), 3, "the re-parse backfilled the text rows");
        let embedder = StubEmbedder::new(model, 8, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&embedder));
        run_armed_pass(&graph);
        assert_eq!(embedder.calls(), 3);
        assert_eq!(graph.store().symbol_vec_count().unwrap(), 3);
        // And a second index pass does NOT re-parse them again (the
        // re-parse stamped v2 — the backfill fires exactly once).
        let stats2 = graph.index(None).unwrap();
        assert_eq!(stats2.files_reindexed, 0, "the backfill is never perpetual");
    }

    /// Step 9's report fixture: indexes THIS repository and runs one
    /// full embed pass with the deterministic stub embedder — reporting
    /// symbol counts, the embedded count, DB growth, and pass duration.
    /// Run with `cargo test symbol_embed_measurement -- --ignored
    /// --nocapture` (ignored in normal runs: it walks the whole repo
    /// tree, like `repo_index_timing` above). The stub's vectors carry
    /// the real model's 768 dims, so the DB-growth number is the real
    /// one; only the inference time is stubbed.
    #[cfg(feature = "embeddings")]
    #[test]
    #[ignore]
    fn symbol_embed_measurement_on_this_repo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("codegraph.db");
        let mut graph = CodeGraph::open(root, &db).unwrap();
        let stats = graph.index(None).unwrap();
        let db_bytes_before = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        let embedder = StubEmbedder::new(crate::memory::embedder::CODE_EMBEDDING_MODEL, 768, None);
        graph.arm_symbol_embedder_stub(Arc::clone(&embedder));
        let started = Instant::now();
        run_armed_pass(&graph);
        let pass_elapsed = started.elapsed();
        let db_bytes_after = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        let embedded = graph.store().symbol_vec_count().unwrap();
        let still_pending = graph
            .store()
            .pending_symbols(crate::memory::embedder::CODE_EMBEDDING_MODEL, 10_000_000)
            .unwrap()
            .len();
        println!(
            "step-9 measurement: files scanned {}, reindexed {}, symbols {} (incl. modules/impls), embedded {}, pending after pass {}, db {} -> {} bytes (+{}), full pass {:?} (stub inference)",
            stats.files_scanned,
            stats.files_reindexed,
            stats.symbols,
            embedded,
            still_pending,
            db_bytes_before,
            db_bytes_after,
            db_bytes_after.saturating_sub(db_bytes_before),
            pass_elapsed
        );
        assert!(embedded > 0);
        assert_eq!(still_pending, 0, "the pass drained the queue");
    }
}

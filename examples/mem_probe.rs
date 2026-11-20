// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `mem_probe` — THROWAWAY phase-1 memory measurement probe (plan d3336a8a
//! step 17). Never ship: this file exists only to attribute a Mnemo2
//! window's resident memory by phase (open → index → embed pass →
//! find_code → memory recall) and is deleted once the numbers are in.
//!
//! Two instruments:
//! - a counting [`std::alloc::GlobalAlloc`] — Rust-side live/peak heap bytes
//!   at each checkpoint. Native allocations (ONNX Runtime, SQLite,
//!   tree-sitter) are invisible to it and appear as working-set-minus-live
//!   in the external sampler.
//! - `PHASE <name> ms=<unix_ms> …` markers on stdout, correlated with the
//!   external per-process working-set sampler.
//!
//! Usage (from the repo root, against a FRESH temp config dir — never the
//! real `~/.mnemo`):
//!
//! ```text
//! cargo run --features embeddings --example mem_probe -- \
//!     graph <mode> <project_root> <work_dir> <models_cache_dir> [memory.db]
//! ```
//!
//! `<mode>`:
//! - `noarm` — open + index only: the full re-parse pass, no embed pass.
//! - `inproc` — arm + index so the embed pass runs with the shared host
//!   UNREACHABLE (this exe is run from a scratch copy with no sibling
//!   `mnemo-models.exe`, and `MNEMO_MODELS_EXE` is unset): the pass loads
//!   the in-process ONNX fallback — the suspected window state.
//! - `remote` — arm + index with the shared models host spawned/attached
//!   from the temp config dir (`MNEMO_MODELS_EXE` points at the built
//!   binary): the intended steady state.
//!
//! `[memory.db]` — an offline copy of a project memory store to open and
//! recall against, measuring the fully-semantic cosine-pool build.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Bytes handed out by the counting allocator since process start.
static ALLOCATED: AtomicU64 = AtomicU64::new(0);
/// Bytes returned since process start.
static FREED: AtomicU64 = AtomicU64::new(0);
/// Currently live Rust-side heap bytes.
static LIVE: AtomicU64 = AtomicU64::new(0);
/// The peak of [`LIVE`] (high-water mark of the Rust-side heap).
static PEAK: AtomicU64 = AtomicU64::new(0);
/// The number of `alloc` calls since start.
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// The counting wrapper around the system allocator.
struct Counting;

impl Counting {
    fn bump_in(bytes: u64) {
        ALLOCATED.fetch_add(bytes, Ordering::Relaxed);
        let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(live, Ordering::Relaxed);
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    fn bump_out(bytes: u64) {
        FREED.fetch_add(bytes, Ordering::Relaxed);
        LIVE.fetch_sub(bytes, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            Self::bump_in(layout.size() as u64);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        Self::bump_out(layout.size() as u64);
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            Self::bump_in(layout.size() as u64);
        }
        ptr
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let ptr = System.realloc(ptr, layout, new_size);
        if !ptr.is_null() {
            Self::bump_out(layout.size() as u64);
            Self::bump_in(new_size as u64);
        }
        ptr
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// One measurement checkpoint: `PHASE` line for the external sampler to
/// correlate against, with the allocator counters.
fn checkpoint(label: &str) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let live = LIVE.load(Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);
    let allocated = ALLOCATED.load(Ordering::Relaxed);
    let count = ALLOC_COUNT.load(Ordering::Relaxed);
    println!(
        "PHASE {label} ms={ms} live_mb={:.1} peak_mb={:.1} total_alloc_mb={:.1} allocs={count}",
        live as f64 / 1e6,
        peak as f64 / 1e6,
        allocated as f64 / 1e6,
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// The probe needs the `embeddings` feature (the embedder machinery it
/// measures lives there). Without it, refuse rather than measure nothing.
#[cfg(not(feature = "embeddings"))]
fn main() {
    eprintln!("mem_probe: build with --features embeddings");
}

#[cfg(feature = "embeddings")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 || args[0] != "graph" {
        eprintln!(
            "usage: mem_probe graph <noarm|inproc|remote> <project_root> <work_dir> \
             <models_cache_dir> [memory.db]"
        );
        std::process::exit(2);
    }
    let mode = args[1].clone();
    let root = PathBuf::from(&args[2]);
    let work = PathBuf::from(&args[3]);
    let cache_dir = PathBuf::from(&args[4]);
    let memory_db = args.get(5).map(PathBuf::from);
    println!(
        "PROBE pid={} mode={} root={}",
        std::process::id(),
        mode,
        root.display()
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run(mode, root, work, cache_dir, memory_db));
    checkpoint("end");
}

/// The measured engine flow. Mirrors what the app does on open: index
/// (fresh DB → full re-parse), the background symbol-embed pass, one
/// `find_code` query, then a memory-store open + recall.
#[cfg(feature = "embeddings")]
async fn run(
    mode: String,
    root: PathBuf,
    work: PathBuf,
    cache_dir: PathBuf,
    memory_db: Option<PathBuf>,
) {
    use mnemo::codegraph::CodeGraph;
    use mnemo::memory::embedder::{BundledEmbedder, Embedder, EmbedderStatus, HashEmbedder};
    use mnemo::memory::{MemoryFilter, MemoryStore, MemoryStoreTrait};
    use mnemo::provider::client_factory::acquire_remote_embedder;
    use mnemo::tool::Tool;
    use std::io::Write;
    use std::sync::{Arc, RwLock};

    std::fs::create_dir_all(&work).expect("work dir");
    checkpoint("start");

    let db = work.join("codegraph.db");
    let mut graph = CodeGraph::open(root, &db).expect("open code graph");
    checkpoint("open");

    let armed = mode != "noarm";
    if armed {
        graph.arm_symbol_embeddings(&cache_dir, Some("bge-base-en-v1.5"));
    }
    checkpoint("armed");
    let graph = Arc::new(graph);

    let stats = graph.index(None).expect("index pass");
    println!(
        "INDEX files_scanned={} files_reindexed={} files_pruned={} symbols={}",
        stats.files_scanned, stats.files_reindexed, stats.files_pruned, stats.symbols
    );
    checkpoint("index");

    if armed {
        // The pass runs on its own background thread; drain it so the
        // pass-done checkpoint sees the post-pass steady state. The drain
        // probe reads the store's pending count directly (the store's own
        // accessor is `pub(crate)`, invisible from an example): rows whose
        // text hash has no vector from the current model — the pass's own
        // pending semantics.
        let started = Instant::now();
        let mut last_report = Instant::now() - Duration::from_secs(5);
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let pending = symbol_vec_pending(&db).expect("pending probe");
            if pending == 0 {
                break;
            }
            if started.elapsed() > Duration::from_secs(25 * 60) {
                eprintln!("pass did not drain within 25 min, continuing");
                break;
            }
            if last_report.elapsed() >= Duration::from_secs(10) {
                println!("PASS pending={pending}");
                last_report = Instant::now();
                checkpoint("pass-progress");
            }
        }
        let (vectors, texts) = symbol_vec_counts(&db).expect("symbol vec counts");
        println!("PASS-DONE vectors={vectors} texts={texts}");
    }
    checkpoint("pass-done");

    // One find_code query through the real tool (query embedder + the
    // SymbolSearchIndex snapshot + the blended ranking).
    let tool = mnemo::tool::agent::find_code::FindCodeTool::new(Some(Arc::clone(&graph)));
    let result = tool
        .execute(serde_json::json!({
            "query": "parse source files and build the symbol index",
            "limit": 8,
        }))
        .await;
    let summary: String = result.output.chars().take(200).collect();
    println!("FIND_CODE success={} {summary}", result.success);
    checkpoint("find-code");

    // The memory store: open a (copy of a) real store and run one recall,
    // building the fully-semantic cosine pool over all live rows.
    if let Some(mem_db) = memory_db {
        let status = Arc::new(RwLock::new(EmbedderStatus::Ready));
        let embedder: Arc<dyn Embedder> = match mode.as_str() {
            "remote" => acquire_remote_embedder("bge-base-en-v1.5", &cache_dir, status)
                .unwrap_or_else(|| {
                    println!("NOTE memory side: no host, falling back in-process");
                    Arc::new(
                        BundledEmbedder::new("bge-base-en-v1.5", &cache_dir, Arc::new(RwLock::new(EmbedderStatus::Ready)))
                            .expect("in-process bge"),
                    )
                }),
            "inproc" => Arc::new(
                BundledEmbedder::new("bge-base-en-v1.5", &cache_dir, status)
                    .expect("in-process bge"),
            ),
            _ => Arc::new(HashEmbedder::new()),
        };
        let store = MemoryStore::open(&mem_db, embedder).expect("open memory store");
        checkpoint("mem-open");
        let hits = store
            .recall(
                "how the shell tool runs a command",
                &MemoryFilter::new().limit(10),
            )
            .await
            .expect("recall");
        println!("RECALL hits={}", hits.len());
        checkpoint("mem-recall");
    }

    // Idle steady state: three seconds with everything still alive (the
    // process-global code-embedder slot keeps the pass's embedder).
    std::thread::sleep(Duration::from_secs(3));
    checkpoint("steady");
    let _ = std::io::stdout().flush();
}
/// Open the code graph DB read-only (WAL: concurrent readers are safe).
#[cfg(feature = "embeddings")]
fn graph_db_readonly(db: &Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}
/// How many symbol-text rows still need a vector for the current code
/// model — the pass's own pending semantics (no vector for the model, or
/// the text hash drifted since the vector was stored).
#[cfg(feature = "embeddings")]
fn symbol_vec_pending(db: &Path) -> rusqlite::Result<usize> {
    let conn = graph_db_readonly(db)?;
    let sql = format!(
        "SELECT COUNT(*) FROM cg_symbol_text t
         LEFT JOIN cg_symbol_vec v
           ON v.symbol_id = t.symbol_id
          AND v.model = '{}'
         WHERE v.symbol_id IS NULL OR v.text_hash != t.text_hash",
        mnemo::memory::embedder::CODE_EMBEDDING_MODEL
    );
    conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map(|count| count as usize)
}

/// The stored vector and text-row counts (what the find_code snapshot
/// would load).
#[cfg(feature = "embeddings")]
fn symbol_vec_counts(db: &Path) -> rusqlite::Result<(usize, usize)> {
    let conn = graph_db_readonly(db)?;
    let vectors =
        conn.query_row("SELECT COUNT(*) FROM cg_symbol_vec", [], |row| {
            row.get::<_, i64>(0)
        })? as usize;
    let texts = conn.query_row("SELECT COUNT(*) FROM cg_symbol_text", [], |row| {
        row.get::<_, i64>(0)
    })? as usize;
    Ok((vectors, texts))
}
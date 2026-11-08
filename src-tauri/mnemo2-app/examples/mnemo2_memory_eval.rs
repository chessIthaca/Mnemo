// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `mnemo2_memory_eval` — the memory recall eval's CLI (plan 373dec00, step 1).
//!
//! Copies a project's live memory store (`<coding>/memory.db` plus its `-wal` /
//! `-shm` sidecars) into a temp directory, opens the COPY with the app's
//! configured embedder, and asks every question of the shipped hand set
//! (`mnemo_app::mnemo2::memory_eval::EVAL_CASES_JSONL`) through the app's own
//! non-bumping recall path (`recall_peek`, default filter, limit 12). It prints
//! the score and writes `<coding>/analysis/memory_eval.json`.
//!
//! The live store is never opened for writing — it is only read for the copy.
//! This is deliberately NOT part of CI: it needs the local store.
//!
//! Usage (from `src-tauri/mnemo2-app`):
//!
//! ```text
//! cargo run --example mnemo2_memory_eval -- --coding C:\Mnemo\.coding
//! ```
//!
//! `--coding` defaults to the repository root's `.coding` side-car; `--out`
//! defaults to `<coding>/analysis/memory_eval.json`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use mnemo::config::{self, Config};
use mnemo::memory::{EmbedderStatus, MemoryFilter, MemoryStore, MemoryStoreTrait};
use mnemo::provider::client_factory::build_embedder;
use mnemo_app::mnemo2::memory_eval::{
    hit_from_memory, knowledge_stem, load_fixture, matching_rank, save_record, score_run,
    EvalCase, EvalHit, EvalRecord, HIT_LIMIT,
};
use mnemo_app::mnemo2::route_eval::today_utc;

const USAGE: &str = "\
mnemo2_memory_eval — the memory recall eval (plan 373dec00, step 1)

Copies a project's memory store into a temp dir, opens the copy with the app's
configured embedder, and scores the shipped question set through recall_peek.

Usage:
  cargo run --example mnemo2_memory_eval -- [--coding <dir>] [--out <file>]

  --coding <dir>   the project's .coding side-car (default: <repo>/.coding)
  --out <file>     where to write the eval record
                   (default: <coding>/analysis/memory_eval.json)
  -h, --help       show this text";

#[tokio::main]
async fn main() -> Result<(), String> {
    let Some(options) = parse_args(std::env::args().skip(1).collect())? else {
        println!("{USAGE}");
        return Ok(());
    };

    let cases = load_fixture()?;
    let config_dir = config::resolve_config_dir(None);
    let config = Config::load(&config_dir).map_err(|error| error.to_string())?;
    let status = Arc::new(RwLock::new(EmbedderStatus::default()));
    let embedder = build_embedder(
        &config,
        &config::embedder_cache_dir(&config_dir),
        Arc::clone(&status),
    );
    let configured_model = config.general.general.bundled_embedding_model.clone();
    if let Some(model) = &configured_model {
        if embedder.model_id() != model {
            return Err(format!(
                "the configured embedder '{model}' did not load (active: '{}'); \
                 the score would be meaningless — fix the model install first",
                embedder.model_id()
            ));
        }
    }
    println!(
        "embedder: {} (dim {}, config {})",
        embedder.model_id(),
        embedder.dim(),
        config_dir.display()
    );

    // The live store is read-only for us: copy it (with its WAL) and open the
    // copy. Opening the copy applies the schema and may checkpoint — on the
    // copy only.
    let temp = tempfile::tempdir().map_err(|error| format!("cannot create a temp dir: {error}"))?;
    copy_store(&options.coding, temp.path())?;
    let store = MemoryStore::open(&temp.path().join("memory.db"), embedder)
        .map_err(|error| error.to_string())?;
    // Read the store the way the app does: the live `[memory]` knobs (decay,
    // per-query caps, derived backstop) are part of the retrieval result.
    store.set_memory_search_config(config.general.memory.clone());

    let mut results: Vec<Vec<EvalHit>> = Vec::with_capacity(cases.len());
    for case in &cases {
        let hits = store
            .recall_peek(&case.query, &MemoryFilter::new().limit(HIT_LIMIT))
            .await
            .map_err(|error| format!("recall failed for {:?}: {error}", case.query))?;
        results.push(hits.iter().map(|scored| hit_from_memory(&scored.memory)).collect());
    }

    let score = score_run(&cases, &results);
    println!(
        "\n{} questions — hit@1 {:.3}   hit@5 {:.3}   MRR {:.3}   noise@5 {:.3}",
        cases.len(),
        score.hit_at_1,
        score.hit_at_5,
        score.mrr,
        score.noise_at_5
    );
    for (kind, (hit_at_5, mrr)) in &score.per_kind {
        println!("  {kind:<9} hit@5 {hit_at_5:.3}   MRR {mrr:.3}");
    }
    print_misses(&cases, &results);

    let project_root = options
        .coding
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| options.coding.clone());
    let record = EvalRecord {
        date: today_utc(),
        commit: git_head(&project_root),
        score,
    };
    save_record(&options.out, &record)?;
    println!("\nrecord: {}", options.out.display());
    Ok(())
}

/// Where to read the store from and where to write the record.
struct Options {
    coding: PathBuf,
    out: PathBuf,
}

/// Parse the CLI arguments. `Ok(None)` means help was requested.
fn parse_args(args: Vec<String>) -> Result<Option<Options>, String> {
    let mut coding: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut iter = args.into_iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--coding" => {
                let value = iter.next().ok_or("--coding needs a directory")?;
                coding = Some(PathBuf::from(value));
            }
            "--out" => {
                let value = iter.next().ok_or("--out needs a file path")?;
                out = Some(PathBuf::from(value));
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }
    let coding = coding.unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(".coding")
    });
    let out = out.unwrap_or_else(|| coding.join("analysis").join("memory_eval.json"));
    Ok(Some(Options { coding, out }))
}

/// Copy `memory.db` and its WAL sidecars from the project's `.coding` into
/// `destination`. Only the copy is ever opened.
fn copy_store(coding: &Path, destination: &Path) -> Result<(), String> {
    let source = coding.join("memory.db");
    if !source.is_file() {
        return Err(format!(
            "no memory store at {} (pass --coding <project>/.coding)",
            source.display()
        ));
    }
    for suffix in ["", "-wal", "-shm"] {
        let from = coding.join(format!("memory.db{suffix}"));
        if !from.is_file() {
            continue;
        }
        let to = destination.join(format!("memory.db{suffix}"));
        std::fs::copy(&from, &to)
            .map_err(|error| format!("cannot copy {}: {error}", from.display()))?;
    }
    Ok(())
}

/// Print every question whose expected record missed the top five, with the
/// hit that did come first — the review aid for the next memory step.
fn print_misses(cases: &[EvalCase], results: &[Vec<EvalHit>]) {
    let mut misses = 0usize;
    for (index, case) in cases.iter().enumerate() {
        let hits = results.get(index).map(Vec::as_slice).unwrap_or(&[]);
        if matching_rank(case, hits).is_some_and(|rank| rank <= 5) {
            continue;
        }
        misses += 1;
        let top = hits
            .first()
            .map(|hit| {
                let stem = hit.path.as_deref().and_then(knowledge_stem).unwrap_or("<no file>");
                format!("{stem} — {}", hit.title)
            })
            .unwrap_or_else(|| "<no hits>".to_string());
        println!("  miss [{}] {} -> {}", case.kind, case.query, top);
    }
    if misses == 0 {
        println!("  no misses in the top five");
    }
}

/// The measured project's HEAD, so the record names the tree it describes.
fn git_head(project_root: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|head| !head.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

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
//! non-bumping recall path (`recall_peek`, default filter, limit 12). Two
//! passes run: the paraphrased questions, and a title control that asks each
//! one as the record's own title. It prints both scores and the miss list, and
//! writes `<coding>/tmp/memory_eval.json`.
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
//! defaults to `<coding>/tmp/memory_eval.json` (under the gitignored tmp dir).

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use mnemo::config::{self, Config};
use mnemo::memory::{EmbedderStatus, MemoryFilter, MemoryStore, MemoryStoreTrait};
use mnemo::provider::client_factory::build_embedder;
use mnemo_app::mnemo2::memory_eval::{
    control_cases, hit_from_memory, load_fixture, misses, save_record, score_run,
    summarize_misses, EvalCase, EvalHit, EvalRecord, EvalScore, Miss, MissSummary, HIT_LIMIT,
};
use mnemo_app::mnemo2::route_eval::today_utc;

const USAGE: &str = "\
mnemo2_memory_eval — the memory recall eval (plan 373dec00, step 1)

Copies a project's memory store into a temp dir, opens the copy with the app's
configured embedder, and scores the shipped question set through recall_peek —
once as written (paraphrased) and once with each record's own title (control).

Usage:
  cargo run --example mnemo2_memory_eval -- [--coding <dir>] [--out <file>]

  --coding <dir>   the project's .coding side-car (default: <repo>/.coding)
  --out <file>     where to write the eval record
                   (default: <coding>/tmp/memory_eval.json)
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

    // Pass 1 — the paraphrased questions, as a user would ask them.
    let results = run_pass(&store, &cases).await?;
    let score = score_run(&cases, &results);

    // Pass 2 — the title control: the same expectations asked with each
    // record's own title. A working harness and store score this near 1.0;
    // only then does a low paraphrased score mean weak recall.
    let control = control_cases(&cases, &options.coding.join("knowledge"))?;
    let control_results = run_pass(&store, &control).await?;
    let control_score = score_run(&control, &control_results);

    println!("\n{} questions", cases.len());
    print_score("paraphrased", &score);
    print_score("title_control", &control_score);

    let miss_list = misses(&cases, &results);
    print_misses(&miss_list, &summarize_misses(&miss_list));

    let project_root = options
        .coding
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| options.coding.clone());
    let record = EvalRecord {
        date: today_utc(),
        commit: git_head(&project_root),
        score,
        control: control_score,
        misses: miss_list,
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
    let out = out.unwrap_or_else(|| coding.join("tmp").join("memory_eval.json"));
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

/// Run one pass (a case set) through the app's non-bumping recall path.
async fn run_pass(store: &MemoryStore, cases: &[EvalCase]) -> Result<Vec<Vec<EvalHit>>, String> {
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let hits = store
            .recall_peek(&case.query, &MemoryFilter::new().limit(HIT_LIMIT))
            .await
            .map_err(|error| format!("recall failed for {:?}: {error}", case.query))?;
        results.push(hits.iter().map(|scored| hit_from_memory(&scored.memory)).collect());
    }
    Ok(results)
}

/// Print one pass's overall score and its per-kind `(hit@5, MRR)` rows.
fn print_score(label: &str, score: &EvalScore) {
    println!(
        "{label:<13} hit@1 {:.3}   hit@5 {:.3}   MRR {:.3}   noise@5 {:.3}",
        score.hit_at_1, score.hit_at_5, score.mrr, score.noise_at_5
    );
    for (kind, (hit_at_5, mrr)) in &score.per_kind {
        println!("  {kind:<9} hit@5 {hit_at_5:.3}   MRR {mrr:.3}");
    }
}

/// Print the miss list and its one-line summary — the review aid for the next
/// memory step.
fn print_misses(misses: &[Miss], summary: &MissSummary) {
    for miss in misses {
        let top = miss
            .top
            .first()
            .map(|hit| {
                format!(
                    "{} [{}, {}{}]",
                    hit.title,
                    hit.tier,
                    hit.class,
                    if hit.noise { ", noise" } else { "" }
                )
            })
            .unwrap_or_else(|| "<no hits>".to_string());
        let rank = match miss.rank {
            Some(rank) => format!("expected at rank {rank}"),
            None => "expected absent from all 12".to_string(),
        };
        println!("  miss {} -> {top} ({rank})", miss.query);
    }
    if misses.is_empty() {
        println!("  no misses in the top five");
    }
    println!(
        "  misses: {} — absent from all 12: {} — noise in their top-5 slots: {}/{}",
        summary.misses, summary.absent_from_all, summary.noise_slots, summary.top_slots
    );
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

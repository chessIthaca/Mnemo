// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `mnemo2_route_eval` — the routing eval's CLI (plan 2ba2168a, design §4.8).
//!
//! Asks a RUNNING Laya about the hand set — and, with `--history` / `--mnemo1`,
//! the mined corpora — one prompt at a time, under four labels and again under
//! three and two, then prints the threshold sweep, the misses and the
//! suggested gates.
//! The last run's suggestions land in `<training_dir>/route_eval.json`, which
//! the Laya page shows beside its gates.
//!
//! `--laya-url` is required: the managed sidecar binds a dynamically chosen
//! loopback port and persists nothing, so a separate process cannot discover
//! it.
//!
//! Run with `cargo run -p mnemo-app --features mnemo2 --bin mnemo2_route_eval
//! -- --laya-url http://127.0.0.1:PORT`.

use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use mnemo::OfferedLabels;
use mnemo::memory::classifier::{ClassifierStatus, LayaClassifier};

use mnemo_app::mnemo2::dispatch_routing::{self, DispatchRoutingRow};
use mnemo_app::mnemo2::route_eval::{self, EvalCase};

/// The tool's usage line and the flag explanations.
const USAGE: &str = "usage: mnemo2_route_eval --laya-url URL [--history] [--mnemo1]\n\
\n\
  --laya-url URL  the running Laya's base URL (required: the managed sidecar\n\
                  binds a dynamic loopback port and persists nothing)\n\
  --history       also classify step texts mined from .coding/plans/*.md and\n\
                  .coding/reviews/team-*.md plus the dispatch log's labelled\n\
                  outcomes\n\
  --mnemo1        classify plain-language first-prompt twins of the hand set";

/// The parsed command line.
struct Flags {
    /// The running Laya's base URL.
    laya_url: Option<String>,
    /// Mine the history corpus and the dispatch log's outcome cases.
    history: bool,
    /// Twin the hand set into first-prompt form.
    mnemo1: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mnemo2_route_eval: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Parse the flags; an unknown flag or a missing value is an error carrying
/// the usage text.
fn parse_flags(args: impl Iterator<Item = String>) -> Result<Flags, String> {
    let mut flags = Flags { laya_url: None, history: false, mnemo1: false };
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--laya-url" => {
                let url = args.next().ok_or_else(|| format!("--laya-url needs a URL\n{USAGE}"))?;
                flags.laya_url = Some(url);
            }
            "--history" => flags.history = true,
            "--mnemo1" => flags.mnemo1 = true,
            other => return Err(format!("unknown flag '{other}'\n{USAGE}")),
        }
    }
    Ok(flags)
}

/// Assemble the corpus the flags ask for: the hand set, optionally twinned and
/// optionally extended with the mined history and the log's labelled
/// outcomes.
fn corpus(flags: &Flags) -> Result<Vec<EvalCase>, String> {
    let hand_set = route_eval::parse_hand_set(route_eval::HAND_SET_JSONL)?;
    let hand_set = if flags.mnemo1 {
        route_eval::mnemo1_twins(&hand_set)
    } else {
        hand_set
    };
    let mut cases = hand_set;
    if flags.history {
        let plans = read_markdown_dir(Path::new(".coding/plans"), false)?;
        let reviews = read_markdown_dir(Path::new(".coding/reviews"), true)?;
        cases.extend(route_eval::history_corpus(&plans, &reviews));
        cases.extend(route_eval::outcome_cases(&read_log_rows()));
    }
    Ok(cases)
}

/// Every `.md` document in `dir` (only `team-*.md` when `team_only`), in
/// filesystem order.
fn read_markdown_dir(dir: &Path, team_only: bool) -> Result<Vec<String>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut docs = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || (team_only && !name.starts_with("team-")) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            docs.push(text);
        }
    }
    Ok(docs)
}

/// The dispatch log's rows; a missing log is no rows and a torn line is
/// skipped (the log is append-only).
fn read_log_rows() -> Vec<DispatchRoutingRow> {
    let Ok(text) = std::fs::read_to_string(dispatch_routing::dispatch_routing_log_path()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The first line of a case's text, capped so the miss list stays readable.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 100 {
        return line.to_string();
    }
    let mut capped: String = line.chars().take(97).collect();
    capped.push_str("...");
    capped
}

/// Print one offered-label count's sweep, its suggestion and its misses.
fn print_report(report: &route_eval::EvalReport, cases: usize) {
    println!(
        "\n{} labels offered — {} cases, default gate {:.3}",
        report.labels, cases, report.default_gate
    );
    println!("{:>6}  {:>7}  {:>8}", "gate", "routed", "accuracy");
    for row in &report.table {
        println!(
            "{:>6.3}  {:>6.1}%  {:>7.1}%",
            row.threshold,
            row.routed_share * 100.0,
            row.accuracy * 100.0
        );
    }
    match report.suggested {
        Some(gate) => println!("suggested min_confidence_{} = {gate:.3}", report.labels),
        None => println!(
            "no gate keeps {:.0}% accuracy on the routed cases — the provisional default stands",
            90.0
        ),
    }
    println!("misses at the default gate: {}", report.misses.len());
    for miss in &report.misses {
        let expected = miss.expected.as_deref().unwrap_or("no route");
        let got = match (&miss.got, miss.confidence) {
            (Some(label), Some(confidence)) => format!("{label} ({confidence:.2})"),
            (Some(label), None) => label.clone(),
            (None, _) => "no answer".to_string(),
        };
        println!("  want {expected:<8} got {got:<16} {}", first_line(&miss.text));
    }
}

/// Build the classifier for `url`, run the three label-count passes (four,
/// three and two offered) and write the record.
async fn run() -> Result<(), String> {
    let flags = parse_flags(std::env::args().skip(1))?;
    let url = flags
        .laya_url
        .clone()
        .ok_or_else(|| format!("--laya-url is required\n{USAGE}"))?;
    let cases = corpus(&flags)?;
    if cases.is_empty() {
        return Err("the corpus is empty".to_string());
    }
    println!("classifying {} prompts, one at a time, against {url}", cases.len());

    let status = Arc::new(RwLock::new(ClassifierStatus::default()));
    let classifier = LayaClassifier::with_timeout(&url, status, Duration::from_secs(30))
        .map_err(|error| format!("cannot build the Laya client for {url}: {error}"))?;

    let four_offered = OfferedLabels {
        trivial: true,
        routine: true,
        involved: true,
        hard: true,
    };
    let three_offered = OfferedLabels {
        trivial: true,
        routine: true,
        involved: false,
        hard: true,
    };
    let two_offered = OfferedLabels { trivial: true, routine: false, involved: false, hard: true };
    let mut reports = Vec::new();
    for offered in [four_offered, three_offered, two_offered] {
        let scores = route_eval::classify_corpus(&classifier, offered, &cases).await;
        reports.push(route_eval::report(&scores, offered));
        print_report(reports.last().expect("just pushed"), cases.len());
    }

    let record = route_eval::eval_record(&reports[0], &reports[1], &reports[2]);
    route_eval::save_eval_record(&record)?;
    println!("\nwrote {}", route_eval::route_eval_path().display());
    Ok(())
}

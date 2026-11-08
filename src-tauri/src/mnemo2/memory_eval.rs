// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The memory recall eval's core (plan 373dec00, step 1) — the hand-written
//! question set, the pure scoring function, and the record the CLI writes.
//!
//! The eval asks every question through the app's own non-bumping recall path
//! (`MemoryStore::recall_peek`, default filter, limit [12](HIT_LIMIT)) against
//! a COPY of a project's memory store, and scores the ranked hits against the
//! knowledge record each question was written from. It is deliberately NOT
//! part of CI: it needs the local store (`.coding/memory.db`, gitignored).
//! The CLI lives in `src-tauri/mnemo2-app/examples/mnemo2_memory_eval.rs`.
//!
//! Two passes run in one process: the **paraphrased** set, and a **title
//! control** that asks each question as the record's own title
//! ([`control_cases`]). A working harness and store should score the control
//! near 1.0; only then does a low paraphrased score mean weak recall instead
//! of a broken harness. The record keeps both ([`EvalRecord::score`] /
//! [`EvalRecord::control`]) and lists what recall returned on every
//! paraphrased miss ([`EvalRecord::misses`]).
//!
//! Metric definitions (all ratios over the question set):
//! - `hit_at_1` — the first hit is one of the question's expected records.
//! - `hit_at_5` — one of the first five hits is. This is the metric later
//!   memory steps are judged by: it must never end below the baseline.
//! - `mrr` — the mean of `1 / rank` of the first matching hit (0 when none
//!   matched), so ranking quality inside the returned list is visible.
//! - `noise_at_5` — pooled share of the top-five hits that are noise: a row
//!   whose title carries no typed prefix (SPEC/DECISION/BUG/PLAN/HOW/REVIEW),
//!   or a synthetic `Session involved N tool calls` episodic summary. Noise is
//!   the thing later steps are supposed to stop adding.
//! - `per_kind` — `(hit_at_5, mrr)` for each fixture kind (bug, decision, how,
//!   spec), so a regression in one knowledge kind cannot hide inside the
//!   overall average.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use mnemo::memory::knowledge::parse_file;
use mnemo::memory::{Memory, MemoryRecordType};

/// The shipped question set. The fixture format (one JSON object per line,
/// `#` comments allowed) is documented at the top of the file itself.
pub const EVAL_CASES_JSONL: &str = include_str!("../../tests/fixtures/memory_eval.jsonl");

/// How many hits the eval requests per question — the app's own recall default
/// (`MemoryFilter::new().limit(12)` in `memory_search`).
pub const HIT_LIMIT: usize = 12;

/// How many characters of a hit's body the eval keeps as `content_head` —
/// enough to recognize the synthetic session line, no more.
const CONTENT_HEAD_CHARS: usize = 160;

/// The fixture kinds the hand set covers, ten questions each.
pub const KINDS: [&str; 4] = ["bug", "decision", "how", "spec"];

/// One question of the eval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCase {
    /// The question, written the way a user would ask it without having seen
    /// the source record.
    #[serde(rename = "q")]
    pub query: String,
    /// Knowledge file stems (the file name without `.md`) that answer the
    /// question. Several stems when sibling records answer it equally.
    pub expect: Vec<String>,
    /// Fixture kind: one of [`KINDS`].
    pub kind: String,
}

/// One recall hit, reduced to what scoring needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalHit {
    /// The knowledge file the hit was derived from (`<kind>/<slug>.md`,
    /// relative to `.coding/knowledge/`), when the hit is a knowledge-row
    /// digest.
    pub path: Option<String>,
    /// The memory tier (`semantic`, `episodic`, `working`, …).
    pub tier: String,
    /// The memory title (carries the typed-record prefix on typed rows).
    pub title: String,
    /// The head of the memory body — enough to spot a synthetic session row.
    pub content_head: String,
    /// The memory's provenance class (`authored` / `derived`).
    pub class: String,
}

/// The eval's score. `f64::NAN` never appears: every ratio is 0.0 when its
/// denominator is empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalScore {
    /// Share of questions whose first hit is an expected record.
    pub hit_at_1: f64,
    /// Share of questions with an expected record in the top five.
    pub hit_at_5: f64,
    /// Mean reciprocal rank of the first matching hit.
    pub mrr: f64,
    /// Pooled share of top-five hits that are noise.
    pub noise_at_5: f64,
    /// `(hit_at_5, mrr)` per fixture kind.
    pub per_kind: BTreeMap<String, (f64, f64)>,
}

/// The record the CLI writes (by default to `<coding>/tmp/memory_eval.json`,
/// gitignored): which tree was measured, when, the paraphrased score, the
/// title-control score, and what recall returned on every paraphrased miss.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalRecord {
    /// The run's UTC date (`YYYY-MM-DD`).
    pub date: String,
    /// The git commit checked out in the measured project (HEAD at run time).
    pub commit: String,
    /// The paraphrased question set's score — unchanged shape.
    pub score: EvalScore,
    /// The title-control pass's score: each question asked as the record's own
    /// title. A working harness and store should score near 1.0.
    pub control: EvalScore,
    /// Every paraphrased question without an expected record in its top five,
    /// with what recall did return.
    pub misses: Vec<Miss>,
}

/// One hit of a miss report, reduced for the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissHit {
    /// The memory title.
    pub title: String,
    /// The memory tier.
    pub tier: String,
    /// The memory's provenance class (`authored` / `derived`).
    pub class: String,
    /// Whether the hit counts as noise (see [`is_noise`]).
    pub noise: bool,
}

/// Why a paraphrased question missed: the first hits recall returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Miss {
    /// The paraphrased question.
    #[serde(rename = "q")]
    pub query: String,
    /// The knowledge stems the question was written from.
    pub expect: Vec<String>,
    /// Up to five hits that came first.
    pub top: Vec<MissHit>,
    /// 1-based rank of the first expected record anywhere in the returned
    /// list, when it was returned at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
}

/// The miss list's one-line summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissSummary {
    /// How many questions missed the top five.
    pub misses: usize,
    /// How many of those never returned their expected record at all.
    pub absent_from_all: usize,
    /// How many of the misses' top-five slots were noise.
    pub noise_slots: usize,
    /// How many top-five slots the misses produced in total.
    pub top_slots: usize,
}

/// Score one eval run. `results[i]` is the ranked hit list for `cases[i]`;
/// a missing entry scores as "no hits". Pure — no store, no I/O.
pub fn score_run(cases: &[EvalCase], results: &[Vec<EvalHit>]) -> EvalScore {
    #[derive(Default)]
    struct KindTally {
        cases: usize,
        hits_at_5: usize,
        reciprocal_rank_sum: f64,
    }

    let mut hit_at_1 = 0usize;
    let mut hit_at_5 = 0usize;
    let mut reciprocal_rank_sum = 0.0f64;
    let mut noise_hits = 0usize;
    let mut top_hits = 0usize;
    let mut per_kind: BTreeMap<String, KindTally> = BTreeMap::new();

    for (index, case) in cases.iter().enumerate() {
        let hits = results.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let rank = matching_rank(case, hits);
        let tally = per_kind.entry(case.kind.clone()).or_default();
        tally.cases += 1;
        if rank == Some(1) {
            hit_at_1 += 1;
        }
        if let Some(rank) = rank {
            if rank <= 5 {
                hit_at_5 += 1;
                tally.hits_at_5 += 1;
            }
            let reciprocal = 1.0 / rank as f64;
            reciprocal_rank_sum += reciprocal;
            tally.reciprocal_rank_sum += reciprocal;
        }
        for hit in hits.iter().take(5) {
            top_hits += 1;
            if is_noise(hit) {
                noise_hits += 1;
            }
        }
    }

    let per_kind = per_kind
        .into_iter()
        .map(|(kind, tally)| {
            let cases = tally.cases.max(1) as f64;
            (
                kind,
                (tally.hits_at_5 as f64 / cases, tally.reciprocal_rank_sum / cases),
            )
        })
        .collect();

    EvalScore {
        hit_at_1: ratio(hit_at_1, cases.len()),
        hit_at_5: ratio(hit_at_5, cases.len()),
        mrr: if cases.is_empty() { 0.0 } else { reciprocal_rank_sum / cases.len() as f64 },
        noise_at_5: ratio(noise_hits, top_hits),
        per_kind,
    }
}

/// `part / whole`, or 0.0 when `whole` is empty (no hits at all).
fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// 1-based rank of the first hit whose knowledge file stem is expected.
/// `None` when no hit matches — the CLI prints these questions as misses.
pub fn matching_rank(case: &EvalCase, hits: &[EvalHit]) -> Option<usize> {
    hits.iter()
        .position(|hit| hit_matches(case, hit))
        .map(|index| index + 1)
}

/// Whether a hit is one of the records the question was written from.
fn hit_matches(case: &EvalCase, hit: &EvalHit) -> bool {
    hit.path
        .as_deref()
        .and_then(knowledge_stem)
        .is_some_and(|stem| case.expect.iter().any(|expected| expected == stem))
}

/// The knowledge file stem of a `<kind>/<slug>.md` path — the last path
/// segment without its `.md` suffix.
pub fn knowledge_stem(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next()?;
    name.strip_suffix(".md").filter(|stem| !stem.is_empty())
}

/// Whether a hit counts as noise: an untyped title, or a synthetic
/// `Session involved N tool calls` episodic summary.
pub fn is_noise(hit: &EvalHit) -> bool {
    let title = hit.title.trim_start();
    MemoryRecordType::from_title(title) == MemoryRecordType::None
        || title.starts_with("Session involved ")
        || hit.content_head.trim_start().starts_with("Session involved ")
}

/// Reduce a store row to the eval's hit shape. `path` is the knowledge file a
/// derived knowledge row points at; rows without one never match a question.
pub fn hit_from_memory(memory: &Memory) -> EvalHit {
    EvalHit {
        path: knowledge_rel(memory),
        tier: memory.tier.as_str().to_string(),
        title: memory.title.clone(),
        content_head: content_head(&memory.content),
        class: memory.record_class.as_str().to_string(),
    }
}

/// The knowledge file a derived row was built from (`<type>/<slug>.md`),
/// when the row carries one — the same `data` shape `memory_view` reads.
fn knowledge_rel(memory: &Memory) -> Option<String> {
    let data = memory.data.as_object()?;
    if data.get("kind").and_then(|kind| kind.as_str()) != Some("knowledge") {
        return None;
    }
    data.get("rel_path").and_then(|rel| rel.as_str()).map(str::to_string)
}

/// The first [`CONTENT_HEAD_CHARS`] characters of a memory body.
fn content_head(content: &str) -> String {
    content.chars().take(CONTENT_HEAD_CHARS).collect()
}

/// Build the title-control cases: for each fixture case, the same expected
/// stem, asked with the record's own title instead of the paraphrase.
///
/// The title is read through the app's knowledge parser (`parse_file`), so it
/// is exactly the digest's source title — the `title` front-matter line, with
/// the same fallbacks the indexer uses. A missing record or unreadable title
/// is an error: the control must cover every question, never silently shrink.
pub fn control_cases(cases: &[EvalCase], knowledge_root: &Path) -> Result<Vec<EvalCase>, String> {
    let mut control = Vec::with_capacity(cases.len());
    for case in cases {
        let mut built = None;
        for stem in &case.expect {
            let rel_path = format!("{}/{stem}.md", case.kind);
            let path = knowledge_root.join(&case.kind).join(format!("{stem}.md"));
            let contents = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let record = parse_file(&rel_path, &contents)
                .ok_or_else(|| format!("{} is not a knowledge record", path.display()))?;
            if !record.title.trim().is_empty() {
                built = Some(EvalCase {
                    query: record.title,
                    expect: vec![stem.clone()],
                    kind: case.kind.clone(),
                });
                break;
            }
        }
        control.push(
            built.ok_or_else(|| format!("no readable record title for question {:?}", case.query))?,
        );
    }
    Ok(control)
}

/// Every paraphrased case without an expected record in its top five, with
/// what recall did return. Pure — the review aid for the next memory step.
pub fn misses(cases: &[EvalCase], results: &[Vec<EvalHit>]) -> Vec<Miss> {
    let mut misses = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let hits = results.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let rank = matching_rank(case, hits);
        if rank.is_some_and(|rank| rank <= 5) {
            continue;
        }
        let top = hits
            .iter()
            .take(5)
            .map(|hit| MissHit {
                title: hit.title.clone(),
                tier: hit.tier.clone(),
                class: hit.class.clone(),
                noise: is_noise(hit),
            })
            .collect();
        misses.push(Miss {
            query: case.query.clone(),
            expect: case.expect.clone(),
            top,
            rank,
        });
    }
    misses
}

/// Summarize a miss list: how many expected records never appeared in the
/// returned list at all, and how noisy the misses' own top-five slots were.
pub fn summarize_misses(misses: &[Miss]) -> MissSummary {
    let mut summary = MissSummary {
        misses: misses.len(),
        absent_from_all: 0,
        noise_slots: 0,
        top_slots: 0,
    };
    for miss in misses {
        if miss.rank.is_none() {
            summary.absent_from_all += 1;
        }
        summary.top_slots += miss.top.len();
        summary.noise_slots += miss.top.iter().filter(|hit| hit.noise).count();
    }
    summary
}

/// Parse a fixture file: one JSON object per line; blank lines and `#`
/// comments are skipped. Rejects an empty question, an empty `expect`, and an
/// unknown kind, so a typo cannot silently weaken the set.
pub fn parse_fixture(jsonl: &str) -> Result<Vec<EvalCase>, String> {
    let mut cases = Vec::new();
    for (index, line) in jsonl.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let case: EvalCase = serde_json::from_str(trimmed)
            .map_err(|error| format!("memory eval fixture line {number}: {error}"))?;
        if case.query.trim().is_empty() {
            return Err(format!("memory eval fixture line {number}: empty question"));
        }
        if case.expect.is_empty() {
            return Err(format!("memory eval fixture line {number}: empty `expect`"));
        }
        if !KINDS.contains(&case.kind.as_str()) {
            return Err(format!(
                "memory eval fixture line {number}: unknown kind `{}` (expected one of {})",
                case.kind,
                KINDS.join(", ")
            ));
        }
        cases.push(case);
    }
    Ok(cases)
}

/// Parse the shipped fixture ([`EVAL_CASES_JSONL`]).
pub fn load_fixture() -> Result<Vec<EvalCase>, String> {
    parse_fixture(EVAL_CASES_JSONL)
}

/// Write the eval record, creating the parent directory.
pub fn save_record(path: &Path, record: &EvalRecord) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut encoded = serde_json::to_string_pretty(record)
        .map_err(|error| format!("cannot encode the eval record: {error}"))?;
    encoded.push('\n');
    std::fs::write(path, encoded)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(query: &str, expect: &[&str], kind: &str) -> EvalCase {
        EvalCase {
            query: query.to_string(),
            expect: expect.iter().map(|stem| stem.to_string()).collect(),
            kind: kind.to_string(),
        }
    }

    fn hit(title: &str, path: Option<&str>, content_head: &str) -> EvalHit {
        EvalHit {
            path: path.map(str::to_string),
            tier: "semantic".to_string(),
            title: title.to_string(),
            content_head: content_head.to_string(),
            class: "derived".to_string(),
        }
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn score_counts_hits_at_each_cutoff() {
        let cases = vec![
            case("first", &["alpha"], "bug"),
            case("second", &["beta"], "decision"),
            case("sixth", &["gamma"], "how"),
            case("fourth", &["delta"], "spec"),
        ];
        let results = vec![
            // Rank 1.
            vec![hit("BUG: alpha", Some("bug/alpha.md"), "")],
            // Rank 2.
            vec![
                hit("BUG: other", Some("bug/other.md"), ""),
                hit("DECISION: beta", Some("decision/beta.md"), ""),
            ],
            // Rank 6 — inside the returned list, outside the top five.
            vec![
                hit("BUG: b1", Some("bug/b1.md"), ""),
                hit("BUG: b2", Some("bug/b2.md"), ""),
                hit("BUG: b3", Some("bug/b3.md"), ""),
                hit("BUG: b4", Some("bug/b4.md"), ""),
                hit("BUG: b5", Some("bug/b5.md"), ""),
                hit("HOW: gamma", Some("how/gamma.md"), ""),
            ],
            // No hits at all.
            vec![],
        ];

        let score = score_run(&cases, &results);
        close(score.hit_at_1, 0.25); // only the first question
        close(score.hit_at_5, 0.5); // first + second
        // MRR counts the first match at any rank: 1, 1/2, 1/6, none.
        close(score.mrr, (1.0 + 0.5 + 1.0 / 6.0) / 4.0);
        close(score.per_kind["bug"].0, 1.0);
        close(score.per_kind["decision"].0, 1.0);
        close(score.per_kind["how"].0, 0.0);
        close(score.per_kind["how"].1, 1.0 / 6.0);
        close(score.per_kind["spec"].0, 0.0);
        close(score.per_kind["spec"].1, 0.0);
    }

    #[test]
    fn mrr_uses_the_first_match() {
        let cases = vec![case("q", &["alpha"], "bug")];
        let results = vec![vec![
            hit("BUG: other", Some("bug/other.md"), ""),
            hit("BUG: alpha", Some("bug/alpha.md"), ""),
            hit("BUG: alpha again", Some("bug/alpha.md"), ""),
        ]];

        let score = score_run(&cases, &results);
        close(score.mrr, 0.5);
        close(score.hit_at_1, 0.0);
        close(score.hit_at_5, 1.0);
    }

    #[test]
    fn noise_counts_untyped_and_synthetic_rows() {
        let cases = vec![case("q", &["alpha"], "bug")];
        let results = vec![vec![
            hit("BUG: alpha", Some("bug/alpha.md"), ""),
            hit("BUG: wrapped session", None, "Session involved 4 tool calls across tools: shell."),
            hit("a plain fact", None, ""),
            hit("HOW: something", Some("how/x.md"), ""),
            hit("SPEC: another", Some("spec/y.md"), ""),
        ]];

        let score = score_run(&cases, &results);
        close(score.noise_at_5, 0.4); // the synthetic row + the untyped row
        assert!(!is_noise(&results[0][0]));
        assert!(is_noise(&results[0][1]));
        assert!(is_noise(&results[0][2]));
        assert!(!is_noise(&results[0][3]));
    }

    #[test]
    fn every_fixture_row_names_a_real_record() {
        let cases = load_fixture().expect("the shipped memory eval fixture parses");
        assert_eq!(cases.len(), 40, "the fixture holds 40 questions");

        let mut per_kind: BTreeMap<&str, usize> = BTreeMap::new();
        for case in &cases {
            *per_kind.entry(case.kind.as_str()).or_default() += 1;
        }
        for kind in KINDS {
            assert_eq!(
                per_kind.get(kind).copied().unwrap_or(0),
                10,
                "{kind} needs ten questions"
            );
        }

        // Tests run with the crate root as cwd; the knowledge corpus lives one
        // level up, in the repository root's .coding side-car.
        let knowledge = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits inside the repository")
            .join(".coding/knowledge");
        for case in &cases {
            for stem in &case.expect {
                let path = knowledge.join(&case.kind).join(format!("{stem}.md"));
                assert!(
                    path.is_file(),
                    "{} names a missing {} record: {}",
                    case.query,
                    case.kind,
                    path.display()
                );
            }
        }
    }

    #[test]
    fn control_questions_use_the_record_title() {
        let dir = tempfile::tempdir().expect("a temp knowledge root");
        std::fs::create_dir_all(dir.path().join("bug")).expect("bug dir");
        std::fs::create_dir_all(dir.path().join("how")).expect("how dir");
        std::fs::write(
            dir.path().join("bug/trace-locks.md"),
            "+++\ntitle = \"trace locks are taken in the wrong order\"\ncreated = \"2026-08-22\"\n+++\n\nBody text that must not become the query.\n",
        )
        .expect("the bug record writes");
        std::fs::write(
            dir.path().join("how/retry-the-fix.md"),
            "+++\ntitle = \"retry the fix commit, not the working tree\"\n+++\n\nBody text.\n",
        )
        .expect("the how record writes");

        let cases = vec![
            case("why does the window freeze?", &["trace-locks"], "bug"),
            case("what should the second pass review?", &["retry-the-fix"], "how"),
        ];
        let control = control_cases(&cases, dir.path()).expect("the control builds");

        assert_eq!(control.len(), cases.len());
        assert_eq!(control[0].query, "trace locks are taken in the wrong order");
        assert_eq!(control[0].expect, vec!["trace-locks".to_string()]);
        assert_eq!(control[0].kind, "bug");
        assert_eq!(control[1].query, "retry the fix commit, not the working tree");
        assert_eq!(control[1].kind, "how");
        // A missing record must fail loudly, not silently shrink the control.
        let missing = vec![case("q", &["does-not-exist"], "bug")];
        assert!(control_cases(&missing, dir.path()).is_err());
    }

    #[test]
    fn misses_list_what_was_returned() {
        let cases = vec![
            case("hit", &["alpha"], "bug"),
            case("late", &["beta"], "bug"),
            case("absent", &["gamma"], "spec"),
        ];
        let results = vec![
            vec![hit("BUG: alpha", Some("bug/alpha.md"), "")],
            // No expected record in the top five; beta appears at rank 7.
            vec![
                hit("a plain fact", None, ""),
                hit("BUG: wrapped", None, "Session involved 2 tool calls."),
                hit("BUG: other", Some("bug/other.md"), ""),
                hit("HOW: something", Some("how/x.md"), ""),
                hit("REVIEW: r", Some("review/x.md"), ""),
                hit("BUG: b6", Some("bug/b6.md"), ""),
                hit("BUG: beta", Some("bug/beta.md"), ""),
            ],
            // The expected record never appears at all.
            vec![
                hit("BUG: x", Some("bug/x.md"), ""),
                hit("a plain fact", None, ""),
            ],
        ];

        let misses = misses(&cases, &results);
        assert_eq!(misses.len(), 2);
        assert_eq!(misses[0].query, "late");
        assert_eq!(misses[0].expect, vec!["beta".to_string()]);
        assert_eq!(misses[0].top.len(), 5);
        assert_eq!(misses[0].top[0].title, "a plain fact");
        assert!(misses[0].top[0].noise);
        assert!(misses[0].top[1].noise);
        assert!(!misses[0].top[2].noise);
        assert_eq!(misses[0].top[2].tier, "semantic");
        assert_eq!(misses[0].top[2].class, "derived");
        assert_eq!(misses[0].rank, Some(7));
        assert_eq!(misses[1].query, "absent");
        assert_eq!(misses[1].top.len(), 2);
        assert_eq!(misses[1].rank, None);

        let summary = summarize_misses(&misses);
        assert_eq!(summary.misses, 2);
        assert_eq!(summary.absent_from_all, 1);
        assert_eq!(summary.top_slots, 7);
        assert_eq!(summary.noise_slots, 3);
    }
}

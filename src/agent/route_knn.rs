// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kNN overlay for dispatch routing — online learning with no fine-tune.
//!
//! Laya 0.3.20 has no fine-tune surface, so a served backend never learns from
//! the dispatch log it writes. [`RouteKnn`] closes that gap the same way
//! [`KnnClassifier`](super::failure_triage_knn::KnnClassifier) does for failure
//! triage: it embeds the labelled dispatch corpus with the app's shared
//! embedder, retrieves the K nearest labelled rows for the incoming routing
//! text, and returns a majority vote whose vote share is the answer's
//! confidence. A dispatch asks this overlay before Laya; a cold index, a
//! below-threshold vote or an embedder failure leaves the answer to Laya.
//!
//! # Which rows are labels
//!
//! Only rows carrying a tier label the lead or user actually chose (or that
//! the outcome derivation proves) contribute:
//!
//! - an `outcome` row's derived `label` (the merged step's trustworthy tier);
//! - a `lead_reroute` / `user_reroute` row's `to_tier` (a hand model change is
//!   a learning, even when routing is off) — `custom` is not a tier;
//! - a `lead_override` row's `lead_tier` (the lead chose another tier than
//!   Laya's).
//!
//! Legacy rows keep their bytes on disk and are mapped when read:
//! `simple` → `trivial`, `average` → `routine`, `difficult` → `high`
//! ([`TierLadder::convert_old_id`]). `escalate` is not on the ladder — an
//! escalation row is never indexed, so the overlay can never vote or return
//! it (design §3.1: `escalate` is never a first pick).
//!
//! Rows without their own `text` borrow the `route` row of the same
//! `worker_id`; a label row with no text at all is skipped. A later label for
//! the same worker supersedes the earlier one, so every worker casts at most
//! one vote no matter how often it was rerouted.
//!
//! The index is incremental: when `dispatch_routing.jsonl` grows only the
//! appended rows are embedded, and a vector is reused whenever its row's text
//! is unchanged (a reroute that only changes the label does not re-embed it). A
//! log shrink or an embedder model change rebuilds the whole index. The JSON
//! row shape is parsed field by field here rather than importing Mnemo2's
//! `DispatchRoutingRow`: this crate is the headless kernel and does not depend
//! on the src-tauri application crate.
//!
//! # Never on the dispatch path
//!
//! A refresh can embed a whole cold log, which may outlast a dispatch's
//! classification timeout. [`RouteKnn::classify`] therefore never waits for
//! one: it starts (or joins) a background refresh and votes on the index as it
//! stands now. A cold or still-building index answers `None`, so Laya always
//! gets the remaining dispatch budget; the completed build is published for
//! the next dispatch. [`RouteKnn::indexed_rows`] is the synchronous seam for
//! the offline eval and tests, which may wait for a build.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::Value;

use super::failure_triage_knn::EmbedderGetter;
use super::route_question::OfferedLabels;
use crate::config::endpoints::TierLadder;

/// How many logged neighbours vote. With an inclusive 0.80 gate a 5-neighbour
/// vote needs at least 4 agreeing labels to answer.
pub const ROUTE_KNN_K: usize = 5;

/// The pickable rungs of the tier ladder, cheapest first (interface contract,
/// 2026-11-04). `escalate` is deliberately absent: it is never a first pick, so
/// it is never indexed, voted or returned.
const TIER_LADDER: [&str; 4] = ["trivial", "routine", "medium", "high"];

/// One kNN answer: the winning offered tier, its vote share and the neighbours
/// that voted.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteKnnVote {
    /// The offered tier with the most votes (`trivial`, `routine`, `medium`
    /// or `high` — never `escalate`). Un-offered labels are folded onto their
    /// nearest offered neighbour by [`RouteKnn::classify`].
    pub tier: String,
    /// The winner's share of the votes (`votes_for_winner / k`, 0.0–1.0).
    pub share: f64,
    /// How many neighbours voted (at most [`ROUTE_KNN_K`], exactly the number
    /// of indexed rows when the log is smaller than K).
    pub k: usize,
    /// The vote share of every offered tier that received a vote, keyed by
    /// tier name.
    pub probabilities: BTreeMap<String, f64>,
}

/// One indexed labelled dispatch, in log order.
#[derive(Clone)]
struct RouteEntry {
    /// The routing text the vector came from.
    text: String,
    /// The labelled tier (one of [`TIER_LADDER`]).
    tier: String,
    /// The embedded routing text, shared so an index snapshot is cheap.
    vector: Arc<Vec<f32>>,
}

/// The cached index plus the watermarks a refresh needs.
#[derive(Default)]
struct RouteIndex {
    /// Bytes of complete rows already parsed.
    byte_len: u64,
    /// The embedder model the vectors belong to; a change forces a rebuild.
    model_id: String,
    /// `worker_id` → that worker's `route` row text.
    route_texts: BTreeMap<String, String>,
    /// One entry per worker that has a label, in first-label order.
    entries: Vec<RouteEntry>,
    /// `worker_id` → its slot in `entries`.
    by_worker: BTreeMap<String, usize>,
}

/// The kNN overlay over `dispatch_routing.jsonl`.
///
/// Cheap to clone and safe to share across dispatch tasks: the index is
/// read-locked for lookups and written only by a serialized refresh.
#[derive(Clone)]
pub struct RouteKnn {
    /// Live embedder access — read fresh on every refresh and query, so a
    /// settings swap reaches the overlay without rebuilding it.
    embedder: EmbedderGetter,
    /// The append-only dispatch routing log.
    log_path: PathBuf,
    /// Serializes refreshes: concurrent dispatches do not embed the same new
    /// rows twice, and a background build is not duplicated.
    refresh: Arc<tokio::sync::Mutex<()>>,
    /// The cached index.
    index: Arc<RwLock<RouteIndex>>,
    /// One background refresh at a time (the async mutex is the real
    /// serializer; this flag keeps `classify` from spawning a queued duplicate).
    building: Arc<AtomicBool>,
}

impl RouteKnn {
    /// A cold overlay over `log_path`. The first [`classify`](Self::classify)
    /// starts a background build and votes on whatever is already indexed;
    /// [`indexed_rows`](Self::indexed_rows) waits for a build to complete.
    pub fn new(embedder: EmbedderGetter, log_path: impl Into<PathBuf>) -> Self {
        Self {
            embedder,
            log_path: log_path.into(),
            refresh: Arc::new(tokio::sync::Mutex::new(())),
            index: Arc::new(RwLock::new(RouteIndex::default())),
            building: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The number of workers with a trustworthy label currently indexed.
    ///
    /// Waits for any in-flight background refresh and catches up with the log,
    /// so a caller can report a live corpus size. This is the synchronous seam
    /// for the offline eval and tests; dispatch never calls it.
    pub async fn indexed_rows(&self) -> usize {
        self.refresh().await;
        self.index
            .read()
            .map(|index| index.entries.len())
            .unwrap_or(0)
    }

    /// Vote on `text` among the nearest labelled rows that map onto an offered
    /// tier.
    ///
    /// This never waits for an index build: it starts a background refresh (or
    /// joins the one already running) and votes on the index as it stands now.
    /// `None` when the log holds fewer than `min_rows` labelled workers (the
    /// cold-start / not-enough-learning floor), when no neighbour maps onto an
    /// offered tier, when the embedder returns an all-zero query vector, or on
    /// any I/O / lock failure. The caller compares [`RouteKnnVote::share`] with
    /// its own gate; an under-gate vote still returns `Some` so the eval can
    /// report the overlay's coverage and near-misses.
    pub async fn classify(
        &self,
        text: &str,
        offered: OfferedLabels,
        min_rows: usize,
    ) -> Option<RouteKnnVote> {
        if text.trim().is_empty() || offered.count() < 2 {
            return None;
        }
        self.start_background_refresh();
        let rows = self.index.read().ok()?.entries.len();
        if rows < min_rows {
            return None;
        }
        let embedder = (self.embedder)();
        let query = embedder.embed(text).await;
        if !has_signal(&query) {
            return None;
        }
        self.vote(&query, offered)
    }

    /// Kick off one background build if none is running. The caller (dispatch)
    /// does not await it, so a build that takes longer than the classification
    /// timeout still finishes and publishes its index for the next dispatch.
    fn start_background_refresh(&self) {
        if self
            .building
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.building.store(false, Ordering::Release);
            return;
        };
        let knn = self.clone();
        handle.spawn(async move {
            knn.refresh().await;
            knn.building.store(false, Ordering::Release);
        });
    }

    /// Bring the index up to date with the log.
    ///
    /// A missing log is an empty corpus, not an error: the overlay has simply
    /// learned nothing yet. A file ending in a partial line stops the watermark
    /// before that line, so the next refresh parses it once it is complete.
    /// Callers wait for this (`indexed_rows`, tests, eval) or run it on a
    /// spawned task (`classify`); dispatch never blocks on it.
    async fn refresh(&self) {
        let bytes = match std::fs::read(&self.log_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return,
        };
        let file_len = bytes.len() as u64;
        let embedder = (self.embedder)();
        let model_id = embedder.model_id().to_string();
        {
            let Ok(index) = self.index.read() else {
                return;
            };
            if index.byte_len == file_len && index.model_id == model_id {
                return;
            }
        }
        // Serialize refreshes. Dispatch does not wait here; the offline eval
        // and tests may, and `indexed_rows` waits for an in-flight build.
        let _guard = self.refresh.lock().await;
        // Re-read after taking the refresh lock: the other refresher may have
        // completed while we waited for the fast-path check. Copy the state the
        // refresh needs out of the lock in one scoped section: a std guard must
        // never live across the embeds below.
        let (current_len, mut entries, mut by_worker, mut route_texts, rebuild) = {
            let Ok(index) = self.index.read() else {
                return;
            };
            let current_len = index.byte_len;
            let current_model = index.model_id.clone();
            if current_len == file_len && current_model == model_id {
                return;
            }
            let rebuild = current_model != model_id || file_len < current_len;
            (
                current_len,
                if rebuild {
                    Vec::new()
                } else {
                    index.entries.clone()
                },
                if rebuild {
                    BTreeMap::new()
                } else {
                    index.by_worker.clone()
                },
                if rebuild {
                    BTreeMap::new()
                } else {
                    index.route_texts.clone()
                },
                rebuild,
            )
        };
        let start = if rebuild { 0 } else { current_len as usize };

        // Only whole JSONL lines are consumed. A concurrent append can leave a
        // partial last line; it waits for the next refresh.
        let complete_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|position| position + 1)
            .unwrap_or(0);
        if start >= complete_len && !rebuild {
            return;
        }
        let chunk = &bytes[start.min(complete_len)..complete_len];
        let Ok(text) = std::str::from_utf8(chunk) else {
            return;
        };
        let rows: Vec<Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect();

        // Pass 1: every route row in the chunk supplies its worker's text.
        // Two passes mean a reroute or outcome row appended in the same chunk
        // as its route row still finds it.
        for row in &rows {
            if row.get("event").and_then(Value::as_str) != Some("route") {
                continue;
            }
            let Some(worker_id) = row.get("worker_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(row_text) = row
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            else {
                continue;
            };
            route_texts.insert(worker_id.to_string(), row_text.to_string());
        }

        // Pass 2: apply this chunk's labels in file order. A later label for
        // the same worker replaces its entry (label and, if the text changed,
        // its vector) so each worker votes once.
        for row in &rows {
            let Some(worker_id) = row.get("worker_id").and_then(Value::as_str) else {
                continue;
            };
            // `escalate` and anything else off the pickable ladder is not a
            // training label — the row is not indexed at all, so the overlay
            // can never vote or return it (design §3.1: an escalation is
            // never a first pick).
            let Some(tier) =
                row_label(row).map(logged_tier).filter(|tier| tier_rank(tier).is_some())
            else {
                continue;
            };
            let Some(row_text) = route_texts.get(worker_id).cloned() else {
                continue;
            };
            if let Some(&slot) = by_worker.get(worker_id) {
                let entry = &mut entries[slot];
                if entry.text != row_text {
                    let vector = embedder.embed(&row_text).await;
                    if !has_signal(&vector) {
                        continue;
                    }
                    entry.text = row_text;
                    entry.vector = Arc::new(vector);
                }
                entry.tier = tier.to_string();
            } else {
                let vector = embedder.embed(&row_text).await;
                if !has_signal(&vector) {
                    continue;
                }
                by_worker.insert(worker_id.to_string(), entries.len());
                entries.push(RouteEntry {
                    text: row_text,
                    tier: tier.to_string(),
                    vector: Arc::new(vector),
                });
            }
        }

        if let Ok(mut index) = self.index.write() {
            index.byte_len = complete_len as u64;
            index.model_id = model_id;
            index.route_texts = route_texts;
            index.entries = entries;
            index.by_worker = by_worker;
        }
    }

    /// Majority vote among the K nearest entries that map onto an offered
    /// tier. Ties go to the cheaper tier (the earlier ladder rank), and the
    /// share is `winner_count / k` so a 2–2 tie can never clear an inclusive
    /// 0.80 gate.
    fn vote(&self, query: &[f32], offered: OfferedLabels) -> Option<RouteKnnVote> {
        let index = self.index.read().ok()?;
        let mut neighbors: Vec<(f32, usize)> = index
            .entries
            .iter()
            .filter_map(|entry| {
                let rank = tier_rank(&entry.tier)?;
                let folded = fold_rank(rank, offered)?;
                Some((cosine(query, &entry.vector), folded))
            })
            .collect();
        if neighbors.is_empty() {
            return None;
        }
        // Stable sort: equal similarities keep log order, so the vote is
        // deterministic.
        neighbors.sort_by(|left, right| right.0.total_cmp(&left.0));
        neighbors.truncate(ROUTE_KNN_K.min(neighbors.len()));
        let mut counts = [0usize; TIER_LADDER.len()];
        for (_, rank) in &neighbors {
            counts[*rank] += 1;
        }
        let k = neighbors.len();
        // First maximum, so a tie goes to the cheaper tier.
        let mut winner = 0;
        for rank in 1..TIER_LADDER.len() {
            if counts[rank] > counts[winner] {
                winner = rank;
            }
        }
        let share = counts[winner] as f64 / k as f64;
        let probabilities = (0..TIER_LADDER.len())
            .filter(|rank| counts[*rank] > 0)
            .map(|rank| {
                (
                    TIER_LADDER[rank].to_string(),
                    counts[rank] as f64 / k as f64,
                )
            })
            .collect();
        Some(RouteKnnVote {
            tier: TIER_LADDER[winner].to_string(),
            share,
            k,
            probabilities,
        })
    }
}

/// The tier label a row contributes, if any.
///
/// `outcome` rows use their derived `label`; reroutes use `to_tier` (a
/// `custom` destination is not a tier and is skipped); a `lead_override` uses
/// `lead_tier`. Every other row type is not a training label.
fn row_label(row: &Value) -> Option<&str> {
    match row.get("event").and_then(Value::as_str)? {
        "outcome" => row.get("label").and_then(Value::as_str),
        "lead_reroute" | "user_reroute" => row
            .get("to_tier")
            .and_then(Value::as_str)
            .filter(|tier| *tier != "custom"),
        "lead_override" => row.get("lead_tier").and_then(Value::as_str),
        _ => None,
    }
}

/// The ladder id a stored row carries, mapping the legacy ids on read
/// (`simple` → `trivial`, `average` → `routine`, `difficult` → `high` —
/// interface contract, 2026-11-04: old rows keep their bytes and are
/// converted when read). Anything else stays as it is, so `tier_rank` then
/// refuses `escalate` and unknown strings: they are never indexed, voted or
/// returned.
fn logged_tier(tier: &str) -> &str {
    TierLadder::convert_old_id(tier).unwrap_or(tier)
}

/// The ladder rank of a tier name, `None` for `custom` or any unknown string.
fn tier_rank(tier: &str) -> Option<usize> {
    TIER_LADDER.iter().position(|candidate| *candidate == tier)
}

/// Map a logged label's rank onto an offered tier with the kernel's folding
/// rule ([`route_question`](super::route_question)): an offered label passes
/// through; otherwise the nearest offered label above, else the nearest one
/// below.
fn fold_rank(rank: usize, offered: OfferedLabels) -> Option<usize> {
    let is_offered = |rank: usize| match rank {
        0 => offered.trivial,
        1 => offered.routine,
        2 => offered.involved,
        _ => offered.hard,
    };
    if is_offered(rank) {
        return Some(rank);
    }
    if let Some(above) = (rank + 1..TIER_LADDER.len()).find(|rank| is_offered(*rank)) {
        return Some(above);
    }
    (0..rank).rev().find(|rank| is_offered(*rank))
}

/// Whether an embedding carries a signal. The shared embedder's failure mode
/// is an all-zero vector, which must never win a vote.
fn has_signal(vector: &[f32]) -> bool {
    vector.iter().any(|value| *value != 0.0)
}

/// Cosine similarity; 0.0 for a zero vector or a dimension mismatch (neither
/// can be a useful neighbour).
fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use crate::memory::embedder::Embedder;

    use super::*;

    /// A tiny deterministic bag-of-words embedder: each whitespace token sets
    /// one dimension, so identical texts are identical and shared words pull
    /// vectors together. The counter proves the refresh embeds only new rows.
    struct TokenEmbedder {
        calls: AtomicUsize,
        failing: AtomicBool,
    }

    impl TokenEmbedder {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                failing: AtomicBool::new(false),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl Embedder for TokenEmbedder {
        async fn embed(&self, text: &str) -> Vec<f32> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.failing.load(Ordering::Relaxed) {
                return vec![0.0; 8];
            }
            let mut vector = vec![0.0f32; 8];
            let dims = vector.len();
            for word in text.split_whitespace() {
                let hash = word
                    .bytes()
                    .fold(0usize, |hash, byte| hash.wrapping_mul(31) + byte as usize);
                vector[hash % dims] += 1.0;
            }
            vector
        }

        fn model_id(&self) -> &str {
            "token-test"
        }
    }

    fn all_offered() -> OfferedLabels {
        OfferedLabels {
            trivial: true,
            routine: true,
            involved: true,
            hard: true,
        }
    }

    fn getter(embedder: Arc<TokenEmbedder>) -> EmbedderGetter {
        Arc::new(move || embedder.clone() as Arc<dyn Embedder>)
    }

    fn knn(dir: &Path, embedder: Arc<TokenEmbedder>) -> RouteKnn {
        RouteKnn::new(getter(embedder), dir.join("dispatch_routing.jsonl"))
    }

    fn append(path: &Path, value: Value) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open log");
        writeln!(file, "{value}").expect("append row");
    }

    fn route(path: &Path, worker: &str, text: &str) {
        append(
            path,
            json!({"event": "route", "worker_id": worker, "text": text}),
        );
    }

    fn outcome(path: &Path, worker: &str, label: Option<&str>) {
        append(
            path,
            json!({"event": "outcome", "worker_id": worker, "label": label}),
        );
    }

    fn reroute(path: &Path, worker: &str, to_tier: &str) {
        append(
            path,
            json!({"event": "user_reroute", "worker_id": worker, "to_tier": to_tier}),
        );
    }

    #[tokio::test]
    async fn cold_start_has_no_vote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let embedder = Arc::new(TokenEmbedder::new());
        let cold = knn(dir.path(), embedder.clone());
        assert!(cold
            .classify("implement the routing overlay", all_offered(), 30)
            .await
            .is_none());
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "").expect("empty log");
        let empty_knn = RouteKnn::new(getter(embedder), empty);
        assert!(empty_knn
            .classify("implement the routing overlay", all_offered(), 30)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn consistent_labelled_rows_answer_with_their_share() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let embedder = Arc::new(TokenEmbedder::new());
        let knn = knn(dir.path(), embedder);
        for index in 0..30 {
            let worker = format!("worker-{index}");
            route(&path, &worker, "alpha beta gamma");
            outcome(&path, &worker, Some("routine"));
        }
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta gamma", all_offered(), 30)
            .await
            .expect("vote");
        assert_eq!(vote.tier, "routine");
        assert_eq!(vote.k, ROUTE_KNN_K);
        assert!((vote.share - 1.0).abs() < 1e-9);
        assert!((vote.probabilities["routine"] - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn split_vote_returns_its_share_for_the_caller_to_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        // Five identical-text neighbours: three label trivial, two routine.
        // The share is 0.6, under the default 0.80 kNN gate; the overlay still
        // reports the vote so the caller/eval can see the near miss.
        for index in 0..3 {
            let worker = format!("trivial-{index}");
            route(&path, &worker, "alpha beta gamma");
            reroute(&path, &worker, "trivial");
        }
        for index in 0..2 {
            let worker = format!("routine-{index}");
            route(&path, &worker, "alpha beta gamma");
            reroute(&path, &worker, "routine");
        }
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta gamma", all_offered(), 5)
            .await
            .expect("vote");
        assert_eq!(vote.tier, "trivial");
        assert_eq!(vote.k, 5);
        assert!((vote.share - 0.6).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_tie_cannot_clear_an_inclusive_eighty_percent_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        for (index, tier) in [
            ("a", "trivial"),
            ("b", "trivial"),
            ("c", "high"),
            ("d", "high"),
        ]
        .into_iter()
        {
            route(&path, &format!("worker-{index}"), "alpha beta");
            reroute(&path, &format!("worker-{index}"), tier);
        }
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta", all_offered(), 4)
            .await
            .expect("vote");
        assert_eq!(vote.k, 4);
        assert!((vote.share - 0.5).abs() < 1e-9);
        // Ties go to the cheaper tier.
        assert_eq!(vote.tier, "trivial");
    }

    #[tokio::test]
    async fn a_later_label_supersedes_the_earlier_one_for_the_same_worker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        route(&path, "worker-1", "alpha beta gamma");
        // The lead first overrode to trivial, then the worker merged on a
        // harder tier. One worker casts one vote: high, k = 1.
        append(
            &path,
            json!({"event": "lead_override", "worker_id": "worker-1", "lead_tier": "trivial"}),
        );
        outcome(&path, "worker-1", Some("high"));
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta", all_offered(), 1)
            .await
            .expect("vote");
        assert_eq!(vote.k, 1);
        assert_eq!(vote.tier, "high");
        assert!((vote.share - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn reroute_without_its_own_text_borrows_the_workers_route_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        route(&path, "worker-1", "alpha beta gamma");
        reroute(&path, "worker-1", "trivial");
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta", all_offered(), 1)
            .await
            .expect("vote");
        assert_eq!(vote.tier, "trivial");
        assert!((vote.share - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_custom_reroute_and_a_textless_label_are_not_training_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        route(&path, "worker-1", "alpha beta gamma");
        reroute(&path, "worker-1", "custom");
        outcome(&path, "worker-without-a-route-row", Some("high"));
        assert_eq!(knn.indexed_rows().await, 0);
        assert!(knn.classify("alpha", all_offered(), 1).await.is_none());
    }

    #[tokio::test]
    async fn labels_fold_onto_the_offered_tiers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        route(&path, "worker-1", "alpha beta");
        outcome(&path, "worker-1", Some("high"));
        // `high` is not offered and nothing above it is either: it folds DOWN
        // to the hardest offered tier below it (`routine`).
        let only_bottom_two = OfferedLabels {
            trivial: true,
            routine: true,
            involved: false,
            hard: false,
        };
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta", only_bottom_two, 1)
            .await
            .expect("vote");
        assert_eq!(vote.tier, "routine");
        assert_eq!(vote.k, 1);
        // `trivial` is not offered: it folds UP to the nearest offered tier
        // (`medium`), while `high` stays put; the 1–1 tie goes to the cheaper
        // tier.
        route(&path, "worker-2", "alpha beta");
        reroute(&path, "worker-2", "trivial");
        let only_top_two = OfferedLabels {
            trivial: false,
            routine: false,
            involved: true,
            hard: true,
        };
        knn.indexed_rows().await;
        let vote = knn
            .classify("alpha beta", only_top_two, 1)
            .await
            .expect("vote");
        assert_eq!(vote.tier, "medium");
        assert_eq!(vote.k, 2);
        assert!((vote.share - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn old_tier_ids_are_converted_when_read() {
        // Legacy rows keep their bytes on disk; the overlay maps them onto
        // the ladder when it reads them (interface contract, 2026-11-04), so
        // an old `difficult` outcome votes `high` and the legacy id never
        // leaks into a vote.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        for index in 0..5 {
            let worker = format!("worker-{index}");
            route(&path, &worker, "alpha beta");
            outcome(&path, &worker, Some("difficult"));
        }
        knn.indexed_rows().await;
        let vote = knn.classify("alpha beta", all_offered(), 5).await.expect("vote");
        assert_eq!(vote.tier, "high");
        assert!(!vote.probabilities.contains_key("difficult"), "the legacy id never votes");
        // An old `average` reroute maps onto `routine` the same way: its own
        // text makes it the nearest neighbour, so it lands inside the k=5
        // vote and the converted id is what counts.
        route(&path, "legacy-average", "alpha beta gamma delta");
        reroute(&path, "legacy-average", "average");
        knn.indexed_rows().await;
        let vote =
            knn.classify("alpha beta gamma delta", all_offered(), 6).await.expect("vote");
        assert_eq!(vote.tier, "high");
        assert!(
            vote.probabilities.contains_key("routine"),
            "the old average row voted routine: {:?}",
            vote.probabilities
        );
        assert!(!vote.probabilities.contains_key("average"), "the legacy id never votes");
    }

    #[tokio::test]
    async fn escalate_is_never_indexed_voted_or_returned() {
        // `escalate` is never a first pick: escalation rows are not training
        // labels, so the overlay cannot index, vote or return them.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let knn = knn(dir.path(), Arc::new(TokenEmbedder::new()));
        for index in 0..3 {
            let worker = format!("escalated-{index}");
            route(&path, &worker, "alpha beta");
            reroute(&path, &worker, "escalate");
        }
        assert_eq!(knn.indexed_rows().await, 0, "an escalation row is never a label");
        assert!(knn.classify("alpha beta", all_offered(), 1).await.is_none());
        // Two real labels beside them still vote — never `escalate`.
        for index in 0..2 {
            let worker = format!("worker-{index}");
            route(&path, &worker, "alpha beta");
            outcome(&path, &worker, Some("high"));
        }
        knn.indexed_rows().await;
        assert_eq!(knn.indexed_rows().await, 2);
        let vote = knn.classify("alpha beta", all_offered(), 2).await.expect("vote");
        assert_eq!(vote.tier, "high");
        assert!(
            !vote.probabilities.contains_key("escalate"),
            "escalate is never among the voted tiers"
        );
    }

    #[tokio::test]
    async fn growing_the_log_embeds_only_the_appended_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let embedder = Arc::new(TokenEmbedder::new());
        let knn = knn(dir.path(), embedder.clone());
        for index in 0..30 {
            let worker = format!("worker-{index}");
            route(&path, &worker, "alpha beta gamma");
            outcome(&path, &worker, Some("routine"));
        }
        knn.indexed_rows().await;
        knn.classify("alpha beta", all_offered(), 30)
            .await
            .expect("vote");
        // 30 row embeddings + 1 query embedding.
        assert_eq!(embedder.calls(), 31);
        for index in 30..32 {
            let worker = format!("worker-{index}");
            route(&path, &worker, "alpha beta gamma");
            outcome(&path, &worker, Some("routine"));
        }
        knn.indexed_rows().await;
        knn.classify("alpha beta", all_offered(), 30)
            .await
            .expect("vote");
        // Only the two appended rows re-embed, plus the new query.
        assert_eq!(embedder.calls(), 31 + 2 + 1);
    }

    #[tokio::test]
    async fn embedder_failure_falls_back_to_laya() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let embedder = Arc::new(TokenEmbedder::new());
        let knn = knn(dir.path(), embedder.clone());
        for index in 0..30 {
            let worker = format!("worker-{index}");
            route(&path, &worker, "alpha beta gamma");
            outcome(&path, &worker, Some("routine"));
        }
        embedder.failing.store(true, Ordering::Relaxed);
        assert_eq!(knn.indexed_rows().await, 0);
        assert!(knn
            .classify("alpha beta", all_offered(), 30)
            .await
            .is_none());
    }
}

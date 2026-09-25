// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kNN overlay for failure triage — genuinely online learning (item 4b).
//!
//! The fine-tune path retrains OFFLINE on the logged disposition corpus, so
//! between retrains the served checkpoint is frozen. [`KnnClassifier`]
//! closes that gap with zero retraining: it embeds the incoming failure
//! text with the app's existing embedding backend (the same shared embedder
//! slot the memory store uses — never `laya-serve`, so the overlay is
//! independent of the Laya endpoint), retrieves the [`KNN_K`] most similar
//! logged failures from the failure-triage training log, and majority-votes
//! the [`FailureClass`] — the vote share IS the confidence, gated by the
//! same inclusive 0.80
//! [`TRIAGE_THRESHOLD`](super::failure_triage::TRIAGE_THRESHOLD) the gate
//! applies to every classifier.
//!
//! # Online by construction
//!
//! The neighbor index tracks the training-log FILE, not in-memory events:
//! every classification stats the JSONL
//! (`~/.mnemo/laya/training/failure_triage.jsonl`) and, when it grew, embeds
//! only the new rows. A disposition appended by
//! [`resolve_failure`](super::failure_triage::resolve_failure) — this
//! instance or any other — is retrievable on the NEXT classification, no
//! restart, no retraining. The first classification after startup builds the
//! index lazily from the full log; a cold start (empty log) answers `None`
//! and every caller keeps its pre-classifier behavior.
//!
//! # Overlay semantics
//!
//! [`KnnClassifier`] implements [`Classifier`], so it slots into the existing
//! [`FailureTriageHandle`](super::failure_triage::FailureTriageHandle) gate
//! with no new classifier swap sites and no changes at the triage call
//! sites. The gate consults it BEFORE the shared Laya slot: a confident vote
//! steers the failure locally (no network), and anything below the threshold
//! falls through to the checkpoint. Every logged row votes by the class
//! label it carries — including escalated needs-user/permanent rows, whose
//! counterfactual the offline fine-tune cannot observe but whose logged
//! label the kNN can simply reuse.
//!
//! # Safety of the answer
//!
//! Only the exact [`triage_question`](super::failure_triage::triage_question)
//! is ever answered (`None` for any other question, so the overlay can
//! never leak failure labels into auto-typing or any other classifier
//! consumer); an all-zero query vector (the embedder's failure mode) answers
//! `None`; rows with unknown class labels are never indexed. A tie can
//! never clear the 0.80 gate — two labels sharing the top count mean the
//! share is at most k/2 — so the fixed-order tally only exists for
//! determinism.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::memory::classifier::{Answer, Classifier, Question};
use crate::memory::embedder::Embedder;

use super::failure_triage::{read_rows, triage_question, FailureClass};

/// How many logged neighbors vote. With the inclusive 0.80
/// [`TRIAGE_THRESHOLD`](super::failure_triage::TRIAGE_THRESHOLD), a 4/5
/// majority is the smallest share that can clear the gate — k = 5 keeps one
/// dissenting neighbor from ever reaching the threshold.
pub const KNN_K: usize = 5;

/// How the overlay reaches the app's LIVE embedder: a fresh snapshot per
/// classification, so a Settings-driven or background embedder swap (the
/// bundled-model load) flows into the overlay with no rewire of its own.
/// The app passes a closure over the memory store's shared embedder slot
/// (mirroring the `MemoryStore::now` closure pattern); tests pass closures
/// over stub embedders.
pub type EmbedderGetter = Arc<dyn Fn() -> Arc<dyn Embedder> + Send + Sync>;

/// The local kNN overlay classifier over the failure-triage training log.
///
/// Infallible by design (the [`Classifier`] contract): every not-usable
/// outcome is a `None` answer and callers keep their pre-classifier
/// behavior. Build it with [`KnnClassifier::new`] (a live embedder getter —
/// what the app wires) or [`KnnClassifier::with_embedder`] (one fixed
/// embedder — tests and store-less consumers).
pub struct KnnClassifier {
    /// Live embedder access (see [`EmbedderGetter`]) — read fresh on every
    /// classification so embedder swaps reach the overlay without rewiring.
    embedder: EmbedderGetter,
    /// The training-log file this index tracks (the gate's log — tests pass
    /// a temp path; the app passes the default training-log path).
    log_path: PathBuf,
    /// Serializes index refreshes so concurrent classifications never embed
    /// the same new rows twice. Held across the row-embedding awaits, so it
    /// must be an async lock (the std `RwLock` below is never held across an
    /// await).
    refresh: tokio::sync::Mutex<()>,
    /// The cached neighbor index. Poison-tolerant reads (an optional
    /// classifier must never panic a turn): a poisoned lock degrades to no
    /// refresh / no answer.
    index: RwLock<KnnIndex>,
}

/// The cached index: the parsed-row watermark + the embedded neighbors.
///
/// `rows_seen` counts every PARSED row (label-unknown rows included, which
/// are skipped once and never re-embedded), so an incremental refresh embeds
/// exactly the rows appended since the last one.
#[derive(Default)]
struct KnnIndex {
    /// The training-log byte length the index covers — the refresh trigger
    /// (the log is append-only: growth means new rows).
    byte_len: u64,
    /// How many parsed rows of the log are covered (embedded or skipped).
    rows_seen: usize,
    /// The embedder fingerprint the cached vectors were built with — a
    /// model swap forces a full rebuild so queries and neighbors always
    /// share one vector space.
    model_id: String,
    /// The embedded logged failures, in file order (the vote's tie-break
    /// when similarities tie).
    neighbors: Vec<Neighbor>,
}

/// One indexed logged failure.
struct Neighbor {
    /// The class label the log row carries (the vote's ballot).
    class: FailureClass,
    /// The row's embedded error text (the query's cosine partner).
    vector: Vec<f32>,
}

impl KnnClassifier {
    /// Build the overlay over a live embedder getter and the training log
    /// at `log_path`.
    ///
    /// This is the app's constructor: the getter is a closure over the
    /// memory store's shared embedder slot, so a Settings-driven or
    /// background embedder swap reaches the overlay on the next
    /// classification (the index detects the model-id change and re-embeds
    /// from the log). Tests and store-less consumers can use
    /// [`KnnClassifier::with_embedder`] instead.
    pub fn new(embedder: EmbedderGetter, log_path: impl Into<PathBuf>) -> Self {
        Self {
            embedder,
            log_path: log_path.into(),
            refresh: tokio::sync::Mutex::new(()),
            index: RwLock::new(KnnIndex::default()),
        }
    }

    /// Build the overlay over ONE fixed embedder — the convenience seam for
    /// tests (stub embedders) and library consumers without a memory store.
    /// The embedder never changes for the classifier's lifetime, so the
    /// index never rebuilds for a model swap.
    pub fn with_embedder(embedder: Arc<dyn Embedder>, log_path: impl Into<PathBuf>) -> Self {
        Self::new(Arc::new(move || embedder.clone()), log_path)
    }
    /// Bring the index in line with the training log and `embedder`'s
    /// vector space. Cheap in the common case: one `stat` + one short read
    /// of the index; a classification against an unchanged log and embedder
    /// embeds nothing.
    ///
    /// Refresh protocol (all growth is observed through the log FILE
    /// itself — no in-memory notifications, so any writer's rows are
    /// picked up, this process or another):
    ///
    /// 1. `stat` the log; unchanged byte length + matching model id ⇒ done.
    /// 2. Otherwise take the async refresh lock (serializes concurrent
    ///    classifications so the same new rows are never embedded twice)
    ///    and re-check: the winner of the lock race may have just
    ///    refreshed.
    /// 3. Full rebuild when the embedder's model id changed (the cached
    ///    vectors are from another vector space) or the log shrank
    ///    (rotated / rewritten); otherwise embed only the parsed rows past
    ///    the `rows_seen` watermark — this is what makes the overlay
    ///    online: a row appended by `resolve_failure` costs one embed.
    /// 4. Swap the result in under the index write lock (a sync-only
    ///    section — the embeds happened before it was taken; the std lock
    ///    is never held across an await).
    ///
    /// The watermark converges: a log that grew between the `stat` and the
    /// read leaves `byte_len` behind the real length and the next
    /// classification refreshes again to catch up. A poisoned index lock
    /// (never expected — the locked sections cannot panic) skips the
    /// update; the vote then answers from the cached vectors.
    async fn refresh(&self, embedder: &Arc<dyn Embedder>) {
        let file_len = std::fs::metadata(&self.log_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if let Ok(index) = self.index.read() {
            if index.byte_len == file_len && index.model_id == embedder.model_id() {
                return;
            }
        }
        let _guard = self.refresh.lock().await;
        if let Ok(index) = self.index.read() {
            if index.byte_len == file_len && index.model_id == embedder.model_id() {
                return;
            }
        }
        let rows = read_rows(&self.log_path);
        let (start, rebuild) = match self.index.read() {
            Ok(index) if index.model_id == embedder.model_id() && rows.len() >= index.rows_seen => {
                (index.rows_seen, false)
            }
            _ => (0, true),
        };
        // Embed the rows past the watermark — OUTSIDE any std lock (the
        // awaits happen here). Rows whose class label is not one of the
        // four classes are skipped (but counted, so they are never
        // re-embedded); all-zero vectors (the embedder's failure mode)
        // carry no similarity signal and are skipped too.
        let mut fresh = Vec::new();
        for row in rows.iter().skip(start) {
            let Some(class) = FailureClass::from_label(&row.class) else {
                continue;
            };
            let vector = embedder.embed(&row.error_text).await;
            if has_signal(&vector) {
                fresh.push(Neighbor { class, vector });
            }
        }
        if let Ok(mut index) = self.index.write() {
            if rebuild {
                index.neighbors.clear();
            }
            index.neighbors.extend(fresh);
            index.rows_seen = rows.len();
            index.byte_len = file_len;
            index.model_id = embedder.model_id().to_string();
        }
    }

    /// Vote the query's [`KNN_K`] nearest neighbors: the highest-count class
    /// among the k most similar rows, with the vote share as the
    /// confidence and the per-class shares as the probabilities.
    ///
    /// `None` while the index is empty (cold start) or on a poisoned index
    /// lock. The tally iterates the classes in fixed enum order and keeps
    /// the first maximum, so the answer is deterministic; a tie can never
    /// clear the gate anyway (see the module docs).
    fn vote(&self, query: &[f32]) -> Option<(FailureClass, f64, BTreeMap<String, f64>)> {
        let index = self.index.read().ok()?;
        if index.neighbors.is_empty() {
            return None;
        }
        let k = KNN_K.min(index.neighbors.len());
        let mut sims: Vec<(f32, FailureClass)> = index
            .neighbors
            .iter()
            .map(|neighbor| (cosine(query, &neighbor.vector), neighbor.class))
            .collect();
        // Descending by similarity; equal similarities keep file order
        // (the sort is stable), so the neighbor set is deterministic.
        sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        sims.truncate(k);
        let classes = [
            FailureClass::Transient,
            FailureClass::Permanent,
            FailureClass::NeedsUser,
            FailureClass::FlakyTest,
        ];
        let mut counts = [0usize; 4];
        for &(_, class) in &sims {
            let slot = match class {
                FailureClass::Transient => &mut counts[0],
                FailureClass::Permanent => &mut counts[1],
                FailureClass::NeedsUser => &mut counts[2],
                FailureClass::FlakyTest => &mut counts[3],
            };
            *slot += 1;
        }
        let mut winner = 0;
        for i in 1..counts.len() {
            if counts[i] > counts[winner] {
                winner = i;
            }
        }
        let confidence = counts[winner] as f64 / k as f64;
        let mut probabilities = BTreeMap::new();
        for (i, count) in counts.iter().enumerate() {
            if *count > 0 {
                probabilities.insert(classes[i].label().to_string(), *count as f64 / k as f64);
            }
        }
        Some((classes[winner], confidence, probabilities))
    }
}

#[async_trait]
impl Classifier for KnnClassifier {
    /// Answer the failure-triage question for `state` by kNN vote over the
    /// training log.
    ///
    /// `None` — and therefore the pre-classifier behavior at every call
    /// site — for any question other than
    /// [`triage_question`](super::failure_triage::triage_question) (the
    /// overlay answers nothing else, ever), for an unusable query embedding
    /// (an all-zero vector is the embedder's failure mode), and while the
    /// index is empty (cold start). Otherwise the answer is the majority
    /// vote over the [`KNN_K`] nearest logged failures with the vote share
    /// as the confidence — the gate applies the same threshold it applies
    /// to every classifier.
    async fn classify(&self, state: &str, question: &Question) -> Option<Answer> {
        if question != &triage_question() {
            return None;
        }
        let embedder = (self.embedder)();
        let query = embedder.embed(state).await;
        if !has_signal(&query) {
            return None;
        }
        self.refresh(&embedder).await;
        let (class, confidence, probabilities) = self.vote(&query)?;
        Some(Answer::Choice {
            label: class.label().to_string(),
            confidence,
            probabilities,
        })
    }
}

/// Whether a vector carries any similarity signal: all components finite
/// and at least one non-zero. A zero vector is the embedder's failure mode
/// (defined as no answer, never as a similarity to everything); non-finite
/// components would poison every cosine.
fn has_signal(vector: &[f32]) -> bool {
    vector.iter().all(|x| x.is_finite()) && vector.iter().any(|&x| x != 0.0)
}

/// Cosine similarity, defensively defined: mismatched lengths compare only
/// their overlap (unreachable in practice — a model swap triggers a rebuild
/// first), and any non-finite intermediate or zero norm reads as 0.0 —
/// never NaN.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let overlap = a.len().min(b.len());
    let dot: f32 = a[..overlap]
        .iter()
        .zip(&b[..overlap])
        .map(|(x, y)| x * y)
        .sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if !dot.is_finite()
        || !norm_a.is_finite()
        || !norm_b.is_finite()
        || norm_a == 0.0
        || norm_b == 0.0
    {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::agent::failure_triage::{
        log_failure_at, resolve_failure, triage_failure, FailureKeepReason, FailureLogRow,
        FailureSite, FailureTriage, FailureTriageHandle, TriageAction, TriageDisposition,
        TRIAGE_THRESHOLD,
    };

    /// A token-bucket embedder: every whitespace token hashes into one of
    /// 256 buckets (+1), the vector is unit-normalized — texts sharing
    /// words get a high cosine (the retrieval property the overlay rides),
    /// unrelated texts mostly do not. The model id is configurable so the
    /// swap-triggered rebuild can be proven, and texts containing
    /// [`zero_for`](Self::zeroing)'s marker embed to the zero vector — the
    /// embedder's failure mode.
    struct TokenEmbedder {
        dim: usize,
        model: &'static str,
        zero_for: Option<&'static str>,
    }

    impl TokenEmbedder {
        /// A normal token-bucket embedder under model id `model`.
        fn new(model: &'static str) -> Self {
            Self {
                dim: 256,
                model,
                zero_for: None,
            }
        }

        /// A token-bucket embedder that returns the zero vector for any
        /// text containing `marker` (the embedder's failure mode — used for
        /// the query-side zero guard; index rows never contain it).
        fn zeroing(model: &'static str, marker: &'static str) -> Self {
            Self {
                dim: 256,
                model,
                zero_for: Some(marker),
            }
        }
    }

    #[async_trait]
    impl Embedder for TokenEmbedder {
        async fn embed(&self, text: &str) -> Vec<f32> {
            if self.zero_for.is_some_and(|marker| text.contains(marker)) {
                return vec![0.0; 64];
            }
            let mut vector = vec![0.0_f32; self.dim];
            for token in text.split_whitespace() {
                // FNV-1a over the token's bytes.
                let mut hash: u64 = 0xcbf29ce484222325;
                for byte in token.bytes() {
                    hash ^= byte as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                vector[(hash % self.dim as u64) as usize] += 1.0;
            }
            let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                for value in &mut vector {
                    *value /= norm;
                }
            }
            vector
        }

        fn model_id(&self) -> &str {
            self.model
        }
    }

    /// An embedder that produces only zero vectors — both the row-side
    /// skip and a distinct model id (the swap-rebuild test's "before"
    /// embedder).
    struct ZeroEmbedder;

    #[async_trait]
    impl Embedder for ZeroEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            vec![0.0; 64]
        }

        fn model_id(&self) -> &str {
            "zero"
        }
    }

    /// A BASE (shared-slot) classifier that counts calls — the proof of
    /// which layer a triage outcome came from.
    struct CountingClassifier {
        calls: Arc<AtomicUsize>,
        answer: Option<Answer>,
    }

    #[async_trait]
    impl Classifier for CountingClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.answer.clone()
        }
    }

    fn choice(label: &str, confidence: f64) -> Option<Answer> {
        Some(Answer::Choice {
            label: label.to_string(),
            confidence,
            probabilities: BTreeMap::new(),
        })
    }

    /// Log one complete training row into `log` through the REAL logging
    /// API — the overlay's index source.
    fn write_row(log: &Path, error_text: &str, class: FailureClass) {
        let pending = log_failure_at(
            log,
            FailureSite::ToolBatch,
            Some("search"),
            error_text,
            class,
            0.9,
            TriageAction::Guidance,
        );
        resolve_failure(pending, TriageDisposition::RetrySucceeded);
    }

    /// A live gate with an optional base classifier plus the overlay over
    /// `log`, with the given master / overlay flag states.
    fn gate(
        base: Option<Arc<dyn Classifier>>,
        log: &Path,
        master: bool,
        knn_on: bool,
    ) -> FailureTriageHandle {
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log);
        FailureTriageHandle::new(
            Arc::new(RwLock::new(base)),
            Arc::new(AtomicBool::new(master)),
        )
        .with_knn(Arc::new(knn), knn_on)
    }
    #[tokio::test]
    async fn knn_near_identical_error_text_retrieves_the_stored_class() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        for text in [
            "connection reset by peer while reading the search results",
            "connection reset by peer while reading the tool output",
            "connection reset by peer while streaming the search results",
            "connection reset by peer while reading the file list",
        ] {
            write_row(&log, text, FailureClass::Transient);
        }
        // Four logged transient failures; the query is near-identical to
        // them (their shared words plus one extra). All four vote — share
        // 1.0, well above the gate.
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log.clone());
        let answer = knn
            .classify(
                "connection reset by peer while reading the search results again",
                &triage_question(),
            )
            .await
            .expect("the overlay must answer");
        let Answer::Choice {
            label,
            confidence,
            probabilities,
        } = answer
        else {
            panic!("the answer must be a choice");
        };
        assert_eq!(label, "transient");
        assert_eq!(confidence, 1.0);
        assert_eq!(probabilities.get("transient"), Some(&1.0));
        assert_eq!(probabilities.len(), 1);
        // Through the gate's threshold logic the same answer classifies.
        assert_eq!(
            triage_failure(
                &knn,
                "connection reset by peer while reading the search results again"
            )
            .await,
            FailureTriage::Classified {
                class: FailureClass::Transient,
                confidence: 1.0,
            }
        );
        assert!(TRIAGE_THRESHOLD <= confidence);
    }

    #[tokio::test]
    async fn knn_empty_index_returns_none_cold_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No log file at all — cold start.
        let knn = KnnClassifier::with_embedder(
            Arc::new(TokenEmbedder::new("tokens")),
            dir.path().join("failure_triage.jsonl"),
        );
        assert!(knn
            .classify("connection reset by peer", &triage_question())
            .await
            .is_none());
        // An EMPTY log file behaves identically (zero parsed rows).
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "").expect("write");
        let knn =
            KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), empty.clone());
        assert!(knn
            .classify("connection reset by peer", &triage_question())
            .await
            .is_none());
        // Through the gate: an empty overlay and an empty slot is the
        // strict pre-classifier fallback.
        let handle = gate(None, &empty, true, true);
        assert_eq!(
            handle.triage("connection reset by peer").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn knn_below_threshold_vote_falls_back_to_pre_classifier_rules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        for _ in 0..3 {
            write_row(
                &log,
                "connection reset by peer retry",
                FailureClass::Transient,
            );
        }
        for _ in 0..2 {
            write_row(
                &log,
                "missing file cannot open the path",
                FailureClass::Permanent,
            );
        }
        // Five rows total: k = 5, the vote is 3 transient / 2 permanent —
        // share 0.6, below the inclusive 0.80 gate, so every caller keeps
        // its pre-classifier behavior.
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log.clone());
        let triage = triage_failure(&knn, "connection reset by peer retry").await;
        assert_eq!(
            triage,
            FailureTriage::Fallback {
                reason: FailureKeepReason::LowConfidence(0.6),
            }
        );
        // Gate-level with NO base: the overlay's own reason surfaces (the
        // most informative note the site can get).
        let handle = gate(None, &log, true, true);
        assert_eq!(
            handle.triage("connection reset by peer retry").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::LowConfidence(0.6),
            }
        );
        // Gate-level with a confident base: the base wins — fall-through.
        let base = CountingClassifier {
            calls: Arc::new(AtomicUsize::new(0)),
            answer: choice("permanent", 0.95),
        };
        let calls = base.calls.clone();
        let handle = gate(Some(Arc::new(base)), &log, true, true);
        assert_eq!(
            handle.triage("connection reset by peer retry").await,
            FailureTriage::Classified {
                class: FailureClass::Permanent,
                confidence: 0.95,
            }
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn knn_appended_disposition_is_retrievable_without_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log.clone());
        // Cold: nothing logged yet.
        assert!(knn
            .classify("rate limit exceeded retry later", &triage_question())
            .await
            .is_none());
        // A disposition resolves and appends — the SAME classifier
        // instance (no restart, no rebuild) must retrieve it on the next
        // classification.
        write_row(
            &log,
            "rate limit exceeded retry later",
            FailureClass::NeedsUser,
        );
        let answer = knn
            .classify("rate limit exceeded retry later", &triage_question())
            .await
            .expect("the appended row must be retrievable");
        let Answer::Choice {
            label, confidence, ..
        } = answer
        else {
            panic!("the answer must be a choice");
        };
        assert_eq!(label, "needs_user");
        assert_eq!(confidence, 1.0);
    }

    #[tokio::test]
    async fn knn_confident_answer_short_circuits_the_base_classifier() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        for _ in 0..4 {
            write_row(
                &log,
                "missing file cannot open the path",
                FailureClass::Permanent,
            );
        }
        // The overlay would answer `permanent` at 1.0; a confident base
        // exists but must never be asked.
        let base = CountingClassifier {
            calls: Arc::new(AtomicUsize::new(0)),
            answer: choice("transient", 0.95),
        };
        let calls = base.calls.clone();
        let handle = gate(Some(Arc::new(base)), &log, true, true);
        assert_eq!(
            handle.triage("missing file cannot open the path").await,
            FailureTriage::Classified {
                class: FailureClass::Permanent,
                confidence: 1.0,
            }
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "the base must never be asked"
        );
    }

    #[tokio::test]
    async fn knn_flag_off_keeps_the_byte_identical_base_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        for _ in 0..4 {
            write_row(
                &log,
                "missing file cannot open the path",
                FailureClass::Permanent,
            );
        }
        // The overlay WOULD answer `permanent`; with its flag off the
        // gate is byte-identical to the pre-overlay gate: the base answers
        // and is asked exactly once.
        let base = CountingClassifier {
            calls: Arc::new(AtomicUsize::new(0)),
            answer: choice("transient", 0.95),
        };
        let calls = base.calls.clone();
        let handle = gate(Some(Arc::new(base)), &log, true, false);
        assert_eq!(
            handle.triage("missing file cannot open the path").await,
            FailureTriage::Classified {
                class: FailureClass::Transient,
                confidence: 0.95,
            }
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
    #[tokio::test]
    async fn knn_never_answers_foreign_questions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        write_row(&log, "connection reset by peer", FailureClass::Transient);
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log.clone());
        // Any non-triage question gets no answer — a score question…
        let score = Question::Score {
            instructions: "Score this failure's severity.".to_string(),
            criteria: vec!["low".to_string(), "high".to_string()],
        };
        assert!(knn
            .classify("connection reset by peer", &score)
            .await
            .is_none());
        // …and a different choice question (auto-typing's shape).
        let mut criteria = BTreeMap::new();
        criteria.insert(
            "spec".to_string(),
            "A different choice question.".to_string(),
        );
        let other_choice = Question::Choice {
            instructions: "Not the triage question.".to_string(),
            criteria,
        };
        assert!(knn
            .classify("connection reset by peer", &other_choice)
            .await
            .is_none());
        // Positive control: the exact triage question answers.
        assert!(knn
            .classify("connection reset by peer", &triage_question())
            .await
            .is_some());
    }

    #[tokio::test]
    async fn knn_zero_vector_query_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        write_row(&log, "connection reset by peer", FailureClass::Transient);
        // The index embeds fine; the QUERY text hits the zeroing marker —
        // the embedder's failure mode on the query side must read as no
        // answer, never as a similarity to everything.
        let knn = KnnClassifier::with_embedder(
            Arc::new(TokenEmbedder::zeroing("tokens", "unembeddable")),
            log,
        );
        assert!(knn
            .classify("connection reset by peer unembeddable", &triage_question())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn knn_unknown_class_label_rows_are_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        let row = serde_json::to_string(&FailureLogRow {
            ts: 1,
            site: "tool_batch".to_string(),
            tool: Some("search".to_string()),
            error_text: "connection reset by peer".to_string(),
            class: "banana".to_string(),
            confidence: 0.9,
            action: "guidance".to_string(),
            disposition: Some("retry_succeeded".to_string()),
        })
        .expect("serialize");
        std::fs::write(&log, format!("{row}\n")).expect("write");
        // The row parses, but its class label is outside the taxonomy —
        // never indexed, so the overlay stays cold.
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log.clone());
        assert!(knn
            .classify("connection reset by peer", &triage_question())
            .await
            .is_none());
        // A later good row still works alongside it.
        write_row(
            &log,
            "missing file cannot open the path",
            FailureClass::Permanent,
        );
        let answer = knn
            .classify("missing file cannot open the path", &triage_question())
            .await
            .expect("the good row must vote");
        let Answer::Choice { label, .. } = answer else {
            panic!("the answer must be a choice");
        };
        assert_eq!(label, "permanent");
    }

    #[tokio::test]
    async fn knn_model_id_change_rebuilds_the_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        write_row(&log, "connection reset by peer", FailureClass::Transient);
        // A swappable embedder slot — the app's shared slot shape.
        let slot: Arc<RwLock<Arc<dyn Embedder>>> = Arc::new(RwLock::new(Arc::new(ZeroEmbedder)));
        let getter_slot = slot.clone();
        let knn = KnnClassifier::new(
            Arc::new(move || getter_slot.read().expect("slot lock").clone()),
            log,
        );
        // Under the zero embedder the row vector is skipped — no answer.
        assert!(knn
            .classify("connection reset by peer", &triage_question())
            .await
            .is_none());
        // Swap to a real-token embedder (a distinct model id): the next
        // classification must REBUILD — a stale index would keep the
        // empty vectors and answer nothing.
        *slot.write().expect("slot lock") = Arc::new(TokenEmbedder::new("tokens"));
        let answer = knn
            .classify("connection reset by peer", &triage_question())
            .await
            .expect("the rebuilt index must answer");
        let Answer::Choice {
            label, confidence, ..
        } = answer
        else {
            panic!("the answer must be a choice");
        };
        assert_eq!(label, "transient");
        assert_eq!(confidence, 1.0);
    }

    #[tokio::test]
    async fn knn_vote_share_is_the_confidence_and_the_threshold_is_inclusive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("failure_triage.jsonl");
        for _ in 0..4 {
            write_row(
                &log,
                "connection reset by peer retry",
                FailureClass::Transient,
            );
        }
        write_row(
            &log,
            "missing file cannot open the path",
            FailureClass::Permanent,
        );
        // k = 5 with five rows: a 4/5 vote is exactly 0.80 — inclusive,
        // it classifies.
        let knn = KnnClassifier::with_embedder(Arc::new(TokenEmbedder::new("tokens")), log);
        assert_eq!(
            triage_failure(&knn, "connection reset by peer retry").await,
            FailureTriage::Classified {
                class: FailureClass::Transient,
                confidence: 0.8,
            }
        );
        // The distribution rides the answer's probabilities.
        let answer = knn
            .classify("connection reset by peer retry", &triage_question())
            .await
            .expect("the overlay must answer");
        let Answer::Choice { probabilities, .. } = answer else {
            panic!("the answer must be a choice");
        };
        assert_eq!(probabilities.get("transient"), Some(&0.8));
        assert_eq!(probabilities.get("permanent"), Some(&0.2));
        assert_eq!(probabilities.len(), 2);
    }
}

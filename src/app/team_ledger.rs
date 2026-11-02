// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The team seam of the TeamLead workflow mode (`.coding/design/mnemo2.md`
//! §11 Phase 5.1).
//!
//! A Mnemo2 lead runs its plans through TeamManager-tracked workers the
//! engine cannot see. The plan board and its tools read the team through
//! this trait; the Mnemo2 host implements it over its `TeamRuntime`. Mnemo1
//! never wires one, so the Classic workflow is unaffected.
//!
//! Step indices are 0-based, matching `plan_format::Step::index`
//! (the board tools' `step` argument is 1-based — convert at the
//! call site).

use async_trait::async_trait;

/// A task stamped with a plan that is neither merged nor cancelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTask {
    /// The TeamManager worker id (the exact id the lead's team tools take).
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The TeamManager status label (`working`, `in_review`, ...).
    pub status: String,
    /// The 0-based plan step the task was dispatched under, when known.
    pub step_index: Option<usize>,
}

/// A plan's team review: the evidence a plan closes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReview {
    /// Every task stamped with the plan is merged or cancelled and every
    /// finding is closed. `report` is the Markdown review report; it opens
    /// with `## Verdict: PASS`.
    Ready {
        /// The review report the board writes under `.coding/reviews/`.
        report: String,
    },
    /// Something still blocks the plan — one line per blocker, each naming
    /// the worker id.
    Blocked {
        /// The blockers, for the close notice.
        reasons: Vec<String>,
    },
}

/// The lead's read-only view of its team.
#[async_trait]
pub trait TeamLedger: Send + Sync {
    /// How many worker turns are running now (dispatching, working,
    /// fixing, or merging).
    async fn live_worker_turns(&self) -> usize;

    /// The tasks stamped with `plan_id` — and with `step_index` when it is
    /// `Some` — that are neither merged nor cancelled.
    async fn open_tasks(&self, plan_id: &str, step_index: Option<usize>) -> Vec<OpenTask>;

    /// The plan's team review: `Ready` with a PASS report, or `Blocked`.
    async fn plan_review(&self, plan_id: &str, plan_title: &str) -> PlanReview;
}

/// A scripted ledger for the board and policy tests.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::Mutex;

    pub(crate) struct FakeLedger {
        pub(crate) open: Vec<OpenTask>,
        pub(crate) review: PlanReview,
        pub(crate) asked: Mutex<Vec<(String, Option<usize>)>>,
    }

    impl FakeLedger {
        pub(crate) fn passing() -> Self {
            Self {
                open: Vec::new(),
                review: PlanReview::Ready {
                    report: "## Verdict: PASS\n\nall tasks merged\n".to_string(),
                },
                asked: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn blocked(reasons: &[&str]) -> Self {
            Self {
                review: PlanReview::Blocked {
                    reasons: reasons.iter().map(|r| r.to_string()).collect(),
                },
                ..Self::passing()
            }
        }

        pub(crate) fn with_open(open: Vec<OpenTask>) -> Self {
            Self {
                open,
                ..Self::passing()
            }
        }
    }

    #[async_trait]
    impl TeamLedger for FakeLedger {
        async fn live_worker_turns(&self) -> usize {
            0
        }
        async fn open_tasks(&self, plan_id: &str, step_index: Option<usize>) -> Vec<OpenTask> {
            self.asked
                .lock()
                .unwrap()
                .push((plan_id.to_string(), step_index));
            self.open.clone()
        }
        async fn plan_review(&self, _plan_id: &str, _plan_title: &str) -> PlanReview {
            self.review.clone()
        }
    }
}

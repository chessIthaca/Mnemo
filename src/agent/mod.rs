// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The agentic loop — messages → provider → events → approval → exec → loop.
//!
//! The agent loop orchestrates a full turn:
//! 1. Build the system prompt (constitution + workflow state).
//! 2. Apply the `ToolFilter` before building the request schema.
//! 3. Stream the completion, accumulating text + tool-call deltas.
//! 4. For each tool call: request approval (if needed), execute, feed result back.
//! 5. Repeat until the model stops calling tools.
//! 6. Error recovery: feed errors back as `tool` messages, retry cap.
//!
//! # Module layout
//!
//! - [`loop_impl`] — the `AgentLoop` struct, constructors, and public handles.
//! - [`turn`] — the `run_turn` driver (streaming, summarization, auto-recall).
//! - [`dispatch`] — tool-call dispatch, the approval gate, provider retry.
//! - [`failure_triage`] — the Laya classifier's failure-classification
//!   decision layer (classes, confidence gate, auto-retry policy, the
//!   training log).
//! - [`failure_triage_knn`] — the kNN overlay for failure triage (online
//!   learning from the training log, independent of the Laya endpoint).
//! - [`approval`] / [`context`] / [`factory`] / [`prompt`] — pre-existing
//!   submodules (unchanged by the split).
//!
//! The loop's tests live in [`tests`] (a `#[cfg(test)]`-only submodule).

pub mod approval;
pub mod optimizer;
pub mod context;
pub mod factory;
pub mod failure_triage;
pub mod failure_triage_knn;
pub mod prompt;
pub mod review_scope;
pub mod steering_stats;

mod dispatch;
mod loop_impl;
mod turn;

pub use loop_impl::{drop_cancelled_steers, AgentLoop, AgentLoopConfig, StopReason, TurnOutcome};

/// Maximum consecutive tool-EXECUTION error INTERACTIONS (tool batches)
/// before surfacing to the user and stopping the turn. A batch with any tool
/// that ran and returned `success: false` (not a user denial/interrupt —
/// those are excluded) counts ONCE, however many calls in it failed — three
/// identical calls issued at the same time are one error event, and the
/// model gets a repair chance between interactions (backlog 7f72d3d7). The
/// failed results are fed back to the model as tool messages and the model
/// retries on the next iteration; this cap prevents an infinite loop when
/// the model repeatedly makes the same mistake. The counter resets on any
/// batch without a failure (but with at least one success) so a long turn
/// with occasional failures doesn't accumulate to the cap unfairly.
///
/// This does NOT count LLM-produced malformed tool-call arguments (bad JSON) —
/// those are a transient output issue the model can recover from by emitting
/// valid JSON, so they use the separate, higher [`MAX_BAD_JSON_RETRIES`] cap.
pub(crate) const MAX_RETRIES: u32 = 3;

/// Maximum consecutive `write_review_report` failures before aborting the
/// turn (backlog 5b46674d). Distinct from [`MAX_RETRIES`]: this counter is
/// NOT reset by other tools' successes — a reviewer that keeps failing
/// verdict validation (interleaved with successful reads) is stuck on its
/// single output channel and must fail loudly with a distinct error instead
/// of exhausting its turns and finishing silently report-less
/// (live-observed 2026-12-30). Resets only on a successful
/// `write_review_report`.
pub(crate) const REVIEW_REPORT_MAX_FAILURES: u32 = 3;

/// Maximum consecutive LLM-produced malformed/truncated tool-call arguments
/// (bad JSON) before aborting the turn. The model can recover from a bad-JSON
/// error by simply emitting valid JSON on the next iteration, so this cap is
/// deliberately higher than [`MAX_RETRIES`] — three strikes does not make
/// sense for an error the model can self-correct. It still exists as a
/// safeguard against a pathological model that loops forever on broken
/// arguments. The counter resets as soon as the model produces valid tool
/// calls. Distinct from [`MAX_RETRIES`] (tool-execution failures) and from
/// `MAX_PROVIDER_TURN_ATTEMPTS` (provider/network errors).
pub(crate) const MAX_BAD_JSON_RETRIES: u32 = 8;

/// Test-only re-export of the denial classifier used by `run_turn` (review H1).
#[cfg(test)]
pub(crate) use turn::is_user_denial_tool_output as turn_denial_for_test;
pub(crate) use turn::StatsRow;

#[cfg(test)]
mod tests;

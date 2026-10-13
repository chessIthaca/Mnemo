// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! G6 tier 3 (master-spec §5.9.6) — the external policy's public wire
//! vocabulary. The TYPES live kernel-side (`channels.rs` for the event
//! payloads, `policy.rs` for the spec) so the runtime never imports from
//! the headless module (row 34's kernel-only build); this module
//! re-exports them as the binding surface.
//!
//! The model: the session's owner implements the policy OUTSIDE the
//! process. For each selected [`HookKind`] the kernel emits a
//! `PolicyHook` session event and waits for the matching
//! [`PolicyAnswer`] command — the same request/response shape as
//! approvals, questions, and host tools, so an orchestrator in any
//! language can gate calls (including auto-continue via a `Continue`
//! continuation at `TurnEnd` — implemented outside the process).

pub use crate::agent::policy::{external_policy_default_timeout_ms, ExternalPolicySpec, TimeoutAction};
pub use crate::runtime::channels::{HookKind, PolicyAnswer};
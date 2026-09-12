# Plan: Fix: 429 fallback permanently suppresses per-phase model overrides

## Goal
Restore per-phase model switching ([models.planning]/[models.executing]/[models.reviewing]/[models.complete]) while keeping 429 fallback functional: replace the picker-pin hijack with model-id-keyed endpoint stickiness consulted only at provider-build time.

## Kind
bug_fixing

## Context
Diagnosed via graph/code reading 2026-12-05. Mechanism: resolve_turn_provider (src/agent/loop_impl.rs:807) priority = 1. skill override, 2. explicit_provider pin (set by per-agent picker AND by try_429_fallback), 3. forced model, 4. resolver chain ([models.planning]/executing/reviewing/complete/subagent). The 429 fallback (commit 914ca32 "429 → automatic cross-provider model fallback", plan 6d221559) reuses the never-cleared picker pin to make the fallback sticky → one 429 permanently kills phase model switching for the loop's lifetime (main agent loop = process lifetime, so it spans conversations; explains "no switch after starting up"). Existing tests for the 429 path: src/runtime/agent.rs::rate_limit_429_falls_back_to_alternate_endpoint (1276), rate_limit_429_with_no_alternate_fails_with_clear_message (1395), rate_limit_429_fallback_also_429s_fails_without_cascade (1504), rate_limit_429_falls_back_on_default_provider_path (1614) + the FallbackModelResolver double at agent.rs:1211. set_explicit_provider is legitimately used by the picker only (src-tauri/src/ipc/config_io.rs swap_provider_into_loop + turn.rs PendingSwap completion). Windows/PowerShell env; cargo test must pass warning-free (deny(warnings)); every bug fix needs a regression test that fails without the fix.

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
Phase model switching no longer fires: after a single 429-triggered automatic fallback, the agent stays on the fallback provider forever — no switch to [models.planning]/[models.executing]/etc. on state transitions (e.g. plan→execute) or at conversation start. Root cause: try_429_fallback (src/agent/loop_impl.rs:1225, commit 914ca32) pins the fallback via set_explicit_provider; that pin outranks all state overrides and is never cleared.

## Regression test
state_override_survives_429_fallback

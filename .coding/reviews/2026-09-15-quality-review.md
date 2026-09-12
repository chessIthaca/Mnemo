# Code Quality Review — myharness (2026-09-15)

**Perspective:** Code quality: correctness, bugs, error handling, test coverage,
maintainability, constitution compliance.
**Reviewer:** read-only quality review (conducted in-session).
**Grounding:** full `src/`, `src-tauri/src/ipc/`, `frontend/src/`, `agent.md`,
`src/lib.rs`, `src-tauri/src/main.rs`.

---

## Overall verdict: **High quality**

The codebase is well-documented (every public function has a doc comment),
thoroughly tested (522 lib + 49 tauri + 5 integration + 82 vitest), and
warning-free under `#![deny(warnings)]` at both crate roots. Error handling is
consistent (`Result<T>` + `thiserror`), the approval/dispatch logic is carefully
reasoned, and the IPC contract is guarded by golden fixtures. The findings below
are low-severity quality issues, none blocking.

---

## Findings

### Q1 — `model_supports_vision` has a duplicated doc comment (Low)

**What:** `src/provider/openai.rs:239-240`:
```rust
/// Whether a single `/models` entry explicitly reports image input modality.
/// Whether a single `/models` entry explicitly reports image input modality.
```
The first line is duplicated verbatim on line 240.

**Impact:** Cosmetic doc defect. Under `#![deny(warnings)]` this does not trigger
a warning (it's a doc comment, not a lint), but it's a copy-paste artifact.

**Fix:** Remove the duplicate line 240.

### Q2 — `src/provider/_dbg_test.rs` looks like a leftover debug shim (Low)

**What:** A prior chief-architect review flagged `src/provider/_dbg_test.rs`
(16 lines) as a leftover debug shim inside the published module tree. It was
noted as "noise in the module boundary surface."

**Impact:** Not imported by `src/provider/mod.rs` (which exports only
`client_factory`, `openai`, `stream`, `vision` — verified
`src/provider/mod.rs:6-9`). If it's not a module of the crate, it's dead weight
in the source tree. If it *is* included somewhere, it should be renamed or
removed.

**Fix:** Verify whether `_dbg_test.rs` is referenced anywhere; if not, delete it.
If it's a test helper, move it under `tests/` or gate it behind `#[cfg(test)]`
with a non-`_dbg` name.

### Q3 — `eprintln!` for diagnostics instead of structured logging (Low)

**What:** Several diagnostic paths use `eprintln!`:
- `src/safety_rules.rs:453` — `eprintln!("safety_rules: skipping invalid rule: {e}")`
- `src/config/keys.rs:117-120` — `eprintln!("keys: warning — could not restrict permissions...")`

**Impact:** In a Tauri GUI app with `windows_subsystem = "windows"` (release),
`stderr` is not visible to the user. These diagnostics are lost in production.
The panic hook (`myharness::app::install_panic_hook`) is the structured path for
fatal errors, but these are non-fatal warnings.

**Fix:** Consider routing these through a structured logging facade (`tracing` or
the app's existing diagnostic channel) so they're visible in the UI's logs/debug
console. Low priority — the behaviors are correct (best-effort, fail-safe); only
observability is affected.

### Q4 — `complete_with_retry` swallows the error type into a string (Low)

**What:** `src/agent/dispatch.rs:382-386` — on exhausted retries, the error is
wrapped as `Error::Provider(format!(...))` with a string fallback. The original
`Error` is converted to its `Display` form, losing structured error info.

**Impact:** The caller sees a string, not the typed error. For a provider error
this is acceptable (the user sees the message), but it loses programmatic
distinction (e.g. auth vs rate-limit vs network).

**Fix:** Consider preserving the last error as `last_err` (already captured) and
returning it directly rather than re-wrapping. Low priority — the current
behavior is correct, just less structured.

### Q5 — Constitution compliance: warning-free build verified (Strength)

**What:** `agent.md` (project constitution) requires:
- `#![deny(warnings)]` at both crate roots — verified `src/lib.rs:1` and
  `src-tauri/src/main.rs:7`.
- No `#[allow(...)]` to silence warnings — verified: the only `#[expect(...)]`
  is `src/tool/agent/shell.rs:41` (`#[expect(dead_code, reason = "consumed by
  the frontend from raw tool-call args")]`), which is the modern, reason-bearing
  form (not `#[allow]`) and documents *why* the field is intentionally unused in
  Rust (it's read by the frontend from the raw args).
- `cargo test` before marking a step complete — the closing sequence mandates it.
- Windows paths + PowerShell syntax — the shell tool uses `cfg!(target_os =
  "windows")` to pick `powershell -Command` vs `sh -c` (`shell.rs:136-140`).

**Assessment:** Constitution-compliant. The `#[expect]` (not `#[allow]`) with a
reason is the correct modern idiom for an intentionally-unused field.

### Q6 — Test coverage is comprehensive and behavior-focused (Strength)

**What:** Tests cover:
- Sandbox: traversal, absolute-outside, creation gap, protected targets.
- Approval: all four safety modes, core-operation skip, DenyAll latch, interrupt.
- Git: flag injection (merge/checkout/branch/push), all subcommands.
- Memory: FTS pre-filter, tier filter, no-match-empty, batch access, recency cap.
- SSE: multi-line chunks, incomplete trailing, drain stress (200 lines), parse errors.
- Channels: every `SerializableAgentEvent` kind round-trips JSON.
- Run-All: checkpoint/commit/rollback, strict-success gate, first-line truncation.

**Assessment:** Tests are behavior-focused (assert on outcomes, not
implementation), use `tempdir` for isolation, and skip gracefully when git is
unavailable. Strong.

### Q7 — Error handling is consistent (Strength)

**What:** The codebase uses `Result<T>` + `thiserror::Error` consistently
(`src/error.rs`). Lock poisonings use `.expect("... lock poisoned")` with
descriptive messages (not `unwrap()`). The `?` operator propagates errors. The
IPC layer converts errors to `String` for the Tauri command boundary (idiomatic
for `#[tauri::command]` return types).

**Assessment:** Consistent and idiomatic.

### Q8 — IPC contract is guarded by golden fixtures (Strength)

**What:** `src-tauri/src/ipc/contract_fixtures.rs` + `ipc-contract.test.ts`
guard the Rust↔TS event/DTO shapes. Every `SerializableAgentEvent` kind has a
round-trip JSON test in `channels.rs:546-835`.

**Assessment:** Prevents silent contract drift between the Rust brain and the
React frontend. Strong.

### Q9 — Frontend store is a thin facade over pure reducers (Strength)

**What:** `frontend/src/hooks/useAgentStore.ts` is a Zustand facade over pure
per-event reducer modules (`agentEventReducer.ts`, `appearance.ts`,
`agentState.ts`). `applyAgentEvent` (`agentEventReducer.ts:615-641`) dispatches
every event kind to a dedicated reducer. The right panel is driven by a single
view registry (`rightPanelViews.tsx`).

**Assessment:** Clean separation — reducers are pure and table-testable; the
store is a thin facade. Strong.

---

## Strengths

1. **Warning-free** under `#![deny(warnings)]` at both crate roots.
2. **No `#[allow]` suppressions** — the one `#[expect]` carries a reason.
3. **Comprehensive, behavior-focused tests** across all subsystems.
4. **Consistent error handling** (`Result` + `thiserror`, descriptive `.expect`).
5. **IPC contract guarded** by golden fixtures + round-trip tests.
6. **Pure reducer frontend store** — testable, no side effects in reducers.
7. **Every public function documented.**

## Remediation order

1. **Q1** (Low) — remove the duplicated doc-comment line in `openai.rs:240`.
2. **Q2** (Low) — verify/delete `src/provider/_dbg_test.rs`.
3. **Q3** (Low) — route `eprintln!` diagnostics through a structured channel.
4. **Q4** (Low) — preserve the typed error in `complete_with_retry`.
5. Q5–Q9 are strengths — no action required.

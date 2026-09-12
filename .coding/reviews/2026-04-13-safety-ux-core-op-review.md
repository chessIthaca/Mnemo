# Review: Safety UX — rule-based "Allow for project", mode-change re-evaluation, hide no-op core-op buttons

**Date:** 2026-04-13
**Scope:** All uncommitted changes in the working tree (`git diff HEAD`): 22 modified files + 1 new plan file.
**Reviewer:** read-only subagent

## Summary

The change set implements three coupled safety-UX fixes sharing a new
`core_operation: bool` flag. The **brain crate** (`myharness`) is correct and
all 528 of its tests pass; the **frontend** is correct (tsc clean, 82 vitest
pass). However, the **Tauri shell crate** (`myharness-app`) **does not
compile** — neither `cargo build` nor `cargo test` for that package succeeds.
This is a build-breaking defect that must be fixed before commit.

---

## Correctness

### C1 — CRITICAL (build-breaking): `PendingEntry` does not implement `Debug`, breaking the `#[derive(Debug)]` on `PendingApprovals`

**Where:** `src-tauri/src/ipc/approval.rs:31-43`

`PendingApprovals` derives `Debug` (line 41: `#[derive(Debug, Default)]`), and
its sole field is `map: Mutex<HashMap<(AgentId, String), PendingEntry>>`. The
new `PendingEntry` struct (lines 31-38) does **not** derive `Debug`. Since
`HashMap<K, V>` requires `V: Debug` and `Mutex<T>` requires `T: Debug`, the
derive expansion fails:

```
error[E0277]: `PendingEntry` doesn't implement `Debug`
 --> src-tauri\src\ipc\approval.rs:43:5
  |
41 | #[derive(Debug, Default)]
  |          ----- in this derive macro expansion
42 | pub struct PendingApprovals {
43 |     map: Mutex<HashMap<(AgentId, String), PendingEntry>>,
  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ the trait `Debug` is not implemented for `PendingEntry`
```

The original code stored `oneshot::Sender<Approval>` directly as the map
value; `tokio::sync::oneshot::Sender<T>` *does* implement `Debug`, so the
original `#[derive(Debug)]` compiled. Wrapping the sender in a new struct
without `#[derive(Debug)]` regressed this.

**Fix:** Add `#[derive(Debug)]` to `PendingEntry` (its fields —
`oneshot::Sender<Approval>`, `String`, `Value`, `bool` — all implement
`Debug`). Alternatively, remove the `Debug` derive from `PendingApprovals`
if it is unused, but `Debug` is cheap and the struct is `pub`, so deriving it
on `PendingEntry` is the safer fix.

**Verification:** `cargo build -p myharness-app` fails with exit 101; the
original (stashed) code builds cleanly.

### C2 — CRITICAL (build-breaking): test module uses `tempfile`, which is not a dependency of `myharness-app`

**Where:** `src-tauri/src/ipc/approval.rs:174` (`use tempfile::tempdir;`)

The new `#[cfg(test)] mod tests` block in `approval.rs` (part of the
`myharness-app` Tauri shell crate) imports `tempfile::tempdir`. `tempfile` is
a `[dev-dependencies]` entry in the **brain** crate's `Cargo.toml`
(`C:\myHarness\Cargo.toml`), but it is **not** listed in
`src-tauri/Cargo.toml`. The Tauri shell crate therefore cannot resolve it:

```
error[E0432]: unresolved import `tempfile`
 --> src-tauri\src\ipc\approval.rs:174:5
```

**Fix:** Either (a) add `tempfile` to `[dev-dependencies]` in
`src-tauri/Cargo.toml`, or (b) avoid `tempfile` in these tests by using a
fixed temp path under `std::env::temp_dir()` with a unique suffix, or (c)
move the `re_evaluate` unit tests into the brain crate's
`src/agent/approval.rs` test module (where `tempfile` is already available and
`needs_approval`/`is_project_scoped` live). Option (c) is cleanest because
`re_evaluate` is pure logic over `needs_approval` and does not depend on any
Tauri-shell-specific state.

### C3 — CRITICAL (build-breaking): bare `json!` macro not in scope in test module

**Where:** `src-tauri/src/ipc/approval.rs:218-219`

```
pending.insert(1, "call_a".into(), tx_a, "file_edit".into(), json!({}), false);
pending.insert(2, "call_b".into(), tx_b, "file_edit".into(), json!({}), false);
```

The `json!` macro is used unqualified, but the test module only has
`use super::*;` (line 173), which re-exports `use serde_json::Value;` (the
**type**, line 20) — not the `json!` **macro**. Every other call site in the
same file uses the fully-qualified `serde_json::json!(...)` (e.g. lines 191,
253, 274). These two lines are inconsistent and fail to compile:

```
error: cannot find macro `json` in this scope
 --> src-tauri\src\ipc\approval.rs:218:46
  |
218 |         pending.insert(1, "call_a".into(), tx_a, "file_edit".into(), json!({}), false);
  |                                                              ^^^^
```

**Fix:** Replace `json!({})` with `serde_json::json!({})` on lines 218-219, or
add `use serde_json::json;` to the test module.

### C4 — Correctness of `re_evaluate` core-op skip: PASS

**Where:** `src-tauri/src/ipc/approval.rs:123-149`

`re_evaluate` filters with `!entry.core_operation && !needs_approval(...)`.
Core ops (`core_operation == true`) are short-circuited out before
`needs_approval` is even consulted, so they can never be auto-resolved
regardless of mode. This is correct and matches the design intent. The
`core_operation` flag originates from `force_prompt` in `dispatch.rs:140`
(`tool.never_auto() || tool.never_auto_for(&args)`), which is the trustworthy
source of truth for core-op gating. Test `re_evaluate_skips_core_operations`
and `re_evaluate_mixed_core_and_non_core` cover this.

### C5 — Correctness of `SafetyLevel::NeedsApproval` hardcoding: PASS

**Where:** `src-tauri/src/ipc/approval.rs:132-133`

`re_evaluate` hardcodes `SafetyLevel::NeedsApproval` when calling
`needs_approval`. This is correct: only `NeedsApproval`-level tools ever
reach the approval prompt (dispatch.rs:142 — `AutoRun` tools return
`needs_approval == false` and execute directly, never entering
`PendingApprovals`). So every entry in the map is effectively
`NeedsApproval`, and the hardcoding accurately reflects the in-flight
population.

### C6 — Correctness of `add_rule_broad` pattern anchoring: PASS

**Where:** `src/safety_rules.rs:211-212`

The pattern `format!("^{}:", regex::escape(tool))` produces e.g.
`^file_edit:`. The signature format is `<tool>:<key_arg>`
(`safety_rules.rs:256-258`), so `^file_edit:` matches any `file_edit:...`
signature. The `:` anchor after the tool name prevents prefix collisions:
`^file_edit:` does **not** match `file_append:...` (the char after
`file_edit` is `_`, not `:`) nor `file_write:...`. There is no tool named
`file_edit_append` in the registry (tool names are `file_edit`, `file_append`,
`file_write`, `file_read`), so no false match is possible. Tests
`add_rule_broad_does_not_match_other_tools` and
`add_rule_broad_matches_multiple_paths` verify this.

### C7 — Lock handling in `re_evaluate`: PASS

**Where:** `src-tauri/src/ipc/approval.rs:123-148`

The method collects keys to resolve into a `Vec` first (lines 128-141), then
removes + sends in a separate loop (lines 142-147). This correctly avoids
mutating the `HashMap` while iterating it. The `oneshot::Sender::send` is
non-blocking (it consumes `self` and either delivers or returns `Err` if the
receiver was dropped), so holding the `std::sync::Mutex` guard across the
sends is safe — no deadlock risk, no await-under-lock (this is a sync mutex,
not a tokio mutex). The guard is held for the whole method, which is brief.

---

## Bugs

### B1 — Race between `insert` and `re_evaluate`: benign (no fix required)

**Where:** `src-tauri/src/ipc/events.rs:349` (insert) vs
`src-tauri/src/ipc/agent.rs:228` / `settings.rs:973` (re_evaluate)

`insert` runs in the event-forwarder task; `re_evaluate` runs in the IPC
command handler. Both contend on the same `std::sync::Mutex`, so they are
serialized — no data race. If a new approval arrives *between* the mode
update and the `re_evaluate` call, it is simply inserted and then evaluated
in the same `re_evaluate` pass (or, if it arrives after, it remains pending
and the user answers it). If it arrives *after* `re_evaluate` returns, it
was inserted under the new mode and the agent loop's own dispatch will
re-check `needs_approval` against the current mode before prompting — so a
call that would auto-run under the new mode won't even prompt. No incorrect
resolution is possible. This is acceptable.

### B2 — `is_project_scoped` unused-import warning in non-test build

**Where:** `src-tauri/src/ipc/approval.rs:23`

```
warning: unused import: `is_project_scoped`
  --> src-tauri\src\ipc\approval.rs:23:23
   |
23 | use myharness::agent::approval::{is_project_scoped, needs_approval};
   |                ^^^^^^^^^^^^^^^^^
```

`is_project_scoped` is imported in non-`#[cfg(test)]` code (line 23) but is
only referenced inside the `#[cfg(test)] mod tests` block (line 368). The
comment at lines 362-363 claims the `is_project_scoped_is_accessible` test
"silences" the warning — this is **incorrect**: a `#[cfg(test)]` use does
not silence an `unused_imports` warning emitted against non-test code in a
non-test build. The warning is emitted by `cargo build -p myharness-app`.

**Fix:** Remove `is_project_scoped` from the non-test import (line 23 →
`use myharness::agent::approval::needs_approval;`) and add a
`#[cfg(test)] use myharness::agent::approval::is_project_scoped;` inside the
test module, or simply remove the `is_project_scoped_is_accessible` test
entirely (it tests nothing about this change — it just checks the function
is callable) and drop the import. `needs_approval` internally calls
`is_project_scoped`, so the latter does not need to be imported at the call
site.

### B3 — No deadlock from `re_evaluate` under mutex: PASS (no finding)

Confirmed in C7 — oneshot send is non-blocking; the std mutex is held briefly
with no awaits. No deadlock.

---

## Security

### S1 — Core-op gating is trustworthy: PASS

**Where:** `src/agent/dispatch.rs:140` (`force_prompt`), threaded to
`core_operation` at `dispatch.rs:174`.

`core_operation` derives from `force_prompt = tool.never_auto() ||
tool.never_auto_for(&parsed_call.arguments)`. This is computed in the
trusted dispatch layer from the tool registry's own `never_auto_for`
implementation (overridden only by `GitTool` for `merge`/`push` at
`src/tool/agent/git.rs:173`). The flag cannot be spoofed by the model or the
frontend — it originates server-side from the tool's args. `re_evaluate`
correctly refuses to auto-resolve any entry with `core_operation == true`,
so a mode change to Autonomous cannot silently approve a `git merge`/`push`.
This preserves the constitution's core-op invariant.

### S2 — Mode change to Autonomous auto-resolves `shell` approvals: intended behavior, PASS

**Where:** `src/agent/approval.rs:80` (`SafetyMode::Autonomous => false`)

Switching to Autonomous makes `needs_approval` return `false` for all
non-core tools, including `shell`. So a pending `shell` approval would be
auto-resolved as Approved by `re_evaluate`. This is the **intended**
semantics of Autonomous mode (auto-run everything except core ops), and
`shell` is not a core op (`never_auto_for` returns false for shell). The
task description explicitly confirms this is desired. No finding.

### S3 — `re_evaluate` cannot auto-approve something it shouldn't: PASS

The only calls that can be auto-resolved are those already pending in
`PendingApprovals` (i.e. they already prompted and are awaiting a user
answer) AND for which `needs_approval(new_mode, ...)` returns `false` AND
`core_operation == false`. This is exactly the set of calls the new mode
would auto-run without prompting. Resolving them as Approved is consistent
with the mode's contract. No over-approval is possible.

---

## Constitution compliance

### CC1 — Public functions have doc comments: PASS (with one note)

All new public functions have doc comments:
- `SafetyRules::add_rule_broad` (`src/safety_rules.rs:202-210`) ✓
- `PendingApprovals::re_evaluate` (`src-tauri/src/ipc/approval.rs:109-122`) ✓
- `add_safety_rule_broad` IPC (`src-tauri/src/ipc/agent.rs:73-81`) ✓
- `addSafetyRuleBroad` TS wrapper (`frontend/src/lib/tauri.ts:98-104`) ✓
- `core_operation` fields documented on `AgentEvent::ApprovalRequest`
  (`src/runtime/channels.rs:66-70`), `SerializableAgentEvent::ApprovalRequest`
  (`channels.rs:198-202`), and `PendingApproval.coreOperation`
  (`frontend/src/hooks/agentState.ts:24-29`) ✓

Note: `PendingEntry` (private struct) has doc comments on its fields but no
`#[derive(Debug)]` — see C1.

### CC2 — Tests run before marking complete: PARTIAL FAIL

The constitution requires `cargo test` to pass before marking a step
complete. The brain crate tests pass (528 lib + integration), and the
frontend tests pass (82 vitest, tsc clean). **However**, the Tauri shell
crate (`myharness-app`) tests do **not compile** (`cargo test -p myharness-app
--no-run` fails with 4 errors: C1, C2, C3, plus the B2 warning). A bare
`cargo test` from the workspace root would fail on the `myharness-app`
package. The "528 passed" figure reflects only `cargo test -p myharness`,
not the full workspace. This must be fixed and the full `cargo test` re-run
before the step can be marked complete.

### CC3 — Line-ending style / file_edit preference: PASS

The diff uses targeted `file_edit`-style changes consistent with the
existing code style. No full-file rewrites of existing files. Fixture JSONs
were appended with a new field preserving the existing no-trailing-newline
style.

### CC4 — No commit to main: N/A (review only)

No commits were made; this is a review pass.

---

## Required fixes before commit (blocking)

1. **C1** — Add `#[derive(Debug)]` to `PendingEntry`
   (`src-tauri/src/ipc/approval.rs:31`).
2. **C2** — Resolve the `tempfile` dependency for the `myharness-app` test
   module (add to `src-tauri/Cargo.toml` `[dev-dependencies]`, or move the
   `re_evaluate` tests to the brain crate, or avoid `tempfile`).
3. **C3** — Replace bare `json!({})` with `serde_json::json!({})` at
   `src-tauri/src/ipc/approval.rs:218-219` (or import the macro).
4. **B2** — Remove the unused `is_project_scoped` import from non-test code
   (`src-tauri/src/ipc/approval.rs:23`); move it to a `#[cfg(test)]` import
   or drop the `is_project_scoped_is_accessible` test.
5. Re-run **`cargo test`** (full workspace, not just `-p myharness`) and
   confirm it passes before marking complete.

## Non-blocking observations

- The `is_project_scoped_is_accessible` test (lines 364-373) tests nothing
  about this change — it merely asserts `is_project_scoped("search", ...)`
  is true. Consider removing it once B2 is addressed.
- The two `eprintln!` log lines in `set_safety_mode` (agent.rs:230) and
  `save_settings` (settings.rs:975) are consistent with the codebase's
  existing `eprintln!` logging style. Fine.

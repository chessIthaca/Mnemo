## Verdict: PASS

# Re-review round 2 — Fix console mode on Windows: attach/alloc console + rewire stdio (plan 660cc537, bug_fixing)

Scope: commit `eb17da9` (everything from this plan is committed; reviewed via `git show eb17da9` + final file states of `src-tauri/src/console.rs` and `src-tauri/src/main.rs`). The three round-1 findings (report `.coding/reviews/2026-08-23-console-attach-rewire-review.md`) and the claimed fixes were each verified in the committed code, then the final state was re-checked holistically. The parallel session's untracked files (`.coding/backlog.jsonl`, `.coding/knowledge/`, the knowledge-files recheck report) were ignored as instructed.

---

## Round-1 findings — all three fixed correctly

### HIGH-1 (ungated probe consts → macOS build break) — FIXED
`src-tauri/src/console.rs:2498-2501`: both `PROBE_PASS` and `PROBE_FAIL` now carry `#[cfg(windows)]`, with a doc comment recording exactly why ("ungated they would be dead code (a hard error under `deny(windows)`) on non-Windows hosts"). Re-checked the cfg algebra for the whole Windows regression block added by this plan: `null_std_handles` (2509), `probe_console_handles` (2532), and the test itself (2570-2571) are all gated; nothing outside the block references any of them, and nothing ungated-but-unused remains. On a non-Windows test build the entire block compiles away; on Windows every item is used (consts at 2549/2556/2607, helper at 2581). No other ungated Windows-only items were introduced. Multi-platform neutrality restored.

### LOW-1 (stale pre-fix doc paragraph) — FIXED
The old paragraph ("Attach to the launching terminal's console … makes the REPL visible … the failure is ignored") is deleted — confirmed both in the commit diff (the lines are removed, not stacked) and in the final file. `attach_console`'s doc (console.rs:1502-1530) is now a single coherent comment opening "Give this process a working console before the first output," whose three numbered steps match the code exactly (guard → attach/alloc → rewire-absent-only), including the both-fail bail-out. Accurate; no contradiction remains.

### LOW-2 (rewiring clobbered redirected std handles) — FIXED
- `GetStdHandle` added to the FFI block (console.rs:1541) with a correct signature.
- `handle_is_absent` (1592-1595) treats NULL **and** INVALID_HANDLE_VALUE as absent — matching `GetStdHandle`'s two failure returns.
- Every `SetStdHandle` is gated: stdin (1618-1623, also gated on a valid conin), stdout (1625-1627), stderr (1628-1630). Redirect targets are preserved; the bug condition (PowerShell's NULL handles) still triggers the rewire. Doc step 3 (1522-1526) updated to say exactly this.
- Closure mechanics are clean: `handle_is_absent` captures nothing; `open_console_device` mutably captures `inheritable` and is called twice sequentially (conin, conout) — no borrow conflict, no aliasing.

### Test adjustment for LOW-2 — correct and necessary
The new `#[cfg(windows)] null_std_handles()` helper (2509-2525, raw `SetStdHandle(NULL)` × 3) called by the probe child at 2581 before `attach_console()` at 2582 is not cosmetic: `Stdio::null()` provides **valid** NUL-device handles that the new guard correctly preserves, so without the nulling the test would fail even against the fixed code (the probe's `GetConsoleMode` rejects NUL-device handles). The child now reproduces the exact PowerShell NULL-handle state, keeping the regression test faithful to the defect. The why is documented on both the helper and the test.

---

## Holistic verification

**Regression test still exercises the changed path end-to-end** (console.rs:2559-2611): child spawned with `DETACHED_PROCESS` (0x8 — no console, the GUI-subsystem launch state); `MNEMO_CONSOLE_PROBE=1` flips it into probe mode and it exits via `process::exit` *before* reaching the spawn code — no recursion; full-path `console::tests::…` filter + `--exact` prevents the run-zero-tests-exit-0 false pass (hazard documented in-line); probe requires all three handles to be non-NULL/invalid **and** pass `GetConsoleMode`; the parent asserts `status.code() == Some(PROBE_PASS)`, so a child panic/abort also fails. Reported result: passes (1/1). Under the attach-only implementation the nulled handles stay NULL → probe fails → the test still reproduces the defect.

**main.rs ordering intact after the fixes** (main.rs:107-141): `std::env::args()` (no stdio) → `attach_console()` first statement in the process → migration (may `eprintln`) after the attach but still before `build_brain`'s first config read; GUI path keeps migration as its first statement before `browser_inspection_enabled()`/`build_brain()`; `inherit_shell_path()` non-Unix twin intact.

**Rename complete**: no code references to the old `attach_parent_console` remain (only historical, dated `.coding/analysis/` and plan-context records, which correctly describe their own time). Non-Windows no-op twin present (console.rs:1637-1638).

**No new issues introduced**: no `#[allow(...)]`; all new FFI (`SetStdHandle` in the helper, `GetConsoleMode` in the probe) is inside `#[cfg(windows)]` items; imports in each gated item are used on the platform where the item compiles; `std::process::exit` in the probe child skips destructors, which is fine for an exit-code-only child. Minor note, not a finding: CONIN$/CONOUT$ are opened even when the corresponding std handle is present (redirect case) — two unused handles held for the process lifetime, bounded and negligible.

**Bug-plan checklist**: regression test exercises the changed path ✓; root cause documented (doc comments on `attach_console` + the test) ✓; BUG: memory record live in semantic memory (id `4c2b0805-…`) ✓.

**Constitution / doc sync**: doc comments present and now accurate; warning-free on Windows (src-tauri suite green under `deny(warnings)`); macOS compile risk from HIGH-1 eliminated by inspection (all gated); README's console-mode line remains accurate — the fix makes the documented feature work; no README/PLAN.md changes needed.

**Verification note (process, not a finding)**: the full suites (src-tauri 154 passed; root 1430 passed / 1 pre-existing ignored) were run *before* the round-1 fixes landed; those fixes touched only `src-tauri/src/console.rs`, and the post-fix regression-test run recompiled the whole src-tauri test binary warning-free (deny(warnings)) and re-verified the changed path (1/1). Residual risk is minimal, but per the closing sequence the main agent should re-run the full `cargo test` in the final pass before `finish`.

---

## Summary

All three round-1 findings are correctly fixed in commit `eb17da9` — the cfg gating restores macOS buildability, the stale doc is gone, and the absent-only rewiring preserves redirects while the adjusted regression test still reproduces the exact PowerShell NULL-handle defect (the `null_std_handles` child-side step is required for the test to remain faithful post-LOW-2, and it is). No new issues found.

## Verdict: FINDINGS (1 high, 2 low)

# Review — Fix console mode on Windows: attach/alloc console + rewire stdio (plan 660cc537, bug_fixing)

Reviewed ALL uncommitted changes: `src-tauri/src/console.rs` (attach_console rewrite + regression test), `src-tauri/src/main.rs` (console check moved to top of main), plus the `.coding/plans/stack.json` bookkeeping change (expected, not reviewed as code).

---

## Findings

### HIGH-1 — Ungated `PROBE_PASS`/`PROBE_FAIL` consts break `cargo test` on macOS (multi-platform neutrality)

`src-tauri/src/console.rs:2487-2488` — the two probe exit-code consts sit in `mod tests` WITHOUT `#[cfg(windows)]`, while every use of them (`probe_console_handles`, line 2495, and the test, line 2534) IS `#[cfg(windows)]`-gated. On a non-Windows host both consts become unused private constants → the `dead_code` lint fires ("constant `PROBE_PASS` is never used") → under `#![deny(warnings)]` (src-tauri/src/main.rs:11) that is a hard compile ERROR in the test profile. The verification for this change ran only on this Windows machine; README (§Building, lines 100/108) states CI tests BOTH platforms, and the project constitution explicitly requires macOS + Windows to build. `cargo test` on macOS will fail.

**Fix:** add `#[cfg(windows)]` to the two consts (or move them inside `probe_console_handles`). One-line change; no behavior impact on Windows.

### LOW-1 — Stale pre-fix doc paragraph left on `attach_console`, contradicting the fix

`src-tauri/src/console.rs:1502-1512` — the new doc comment was appended AFTER the old one instead of replacing it, so the function's rendered doc is two contradictory paragraphs. The surviving old text still asserts: "`AttachConsole(ATTACH_PARENT_PROCESS)` binds the process to the parent's console; Rust's stdout handle is resolved lazily on first use, so doing this before any print **makes the REPL visible**" and "double-clicked launches … make the call fail harmlessly — **the failure is ignored**." Both claims are exactly what this bug fix disproves (attach alone never made the REPL visible; failure now falls back to `AllocConsole` + rewiring). A future reader of the first paragraph gets the pre-fix misconception that caused the bug.

**Fix:** delete lines 1502-1512 (the old paragraph); keep the new doc starting "Give this process a working console before the first output."

### LOW-2 — Rewiring clobbers valid redirected std handles (`mnemo --console > out.txt`)

`src-tauri/src/console.rs:1614-1622` — once a console is attached/allocated, the code rewires stdin/stdout/stderr unconditionally. But launchers DO honor redirection for GUI-subsystem children: with `mnemo --console > out.txt` or `... 2> err.log` from cmd/PowerShell, the process starts with VALID file/pipe std handles (and no console, so the `GetConsoleWindow` guard passes and `AttachConsole(PARENT)` succeeds). The fix then replaces those handles with `CONIN$`/`CONOUT$`, so redirected output silently goes to the terminal instead of the file — a regression vs. the pre-fix code, where this scenario worked (handles were the redirect targets). The C-runtime convention is to install console handles only when the current std handle is absent/invalid — which is precisely the bug condition (PowerShell's NULL handles).

**Fix (suggested):** gate each `SetStdHandle` on the current `GetStdHandle(id)` value being NULL or INVALID_HANDLE_VALUE. Preserves the fix for the NULL-handle launch state while keeping user redirection intact. Edge case, but cheap to get right.

---

## Verified correct (no findings)

**Win32 usage** (`attach_console`, console.rs:1541-1624):
- Constants all correct: `STD_INPUT/OUTPUT/ERROR_HANDLE` = (DWORD)-10/-11/-12 → `0xFFFF_FFF6/F5/F4` ✓; `GENERIC_READ` 0x80000000 / `GENERIC_WRITE` 0x40000000 ✓; `FILE_SHARE_READ|WRITE` = 1|2 ✓; `OPEN_EXISTING` = 3 ✓; `INVALID_HANDLE_VALUE` = (HANDLE)-1 ✓. `ATTACH_PARENT_PROCESS = usize::MAX` on the pre-existing `usize`-param declaration reads as 0xFFFFFFFF in the low 32 bits on both x64 and x86 — what the DWORD callee sees; unchanged from before, works.
- `CreateFileW` on `CONIN$`/`CONOUT$` with GENERIC_READ|GENERIC_WRITE, share read|write, OPEN_EXISTING, inheritable SECURITY_ATTRIBUTES = the MSDN console-device recipe; `#[repr(C)]` struct layout (4 + pad + 8 + 4 + pad) matches C; `n_length = size_of` set correctly.
- Sharing one CONOUT$ handle for stdout+stderr is what the CRT does (no `CONERR$` exists); Rust's stdio wraps borrowed handles and never `CloseHandle`s them, so no double-close.
- `GetConsoleWindow() != NULL` as the has-console probe ✓ (skips debug builds and already-console launches). Short-circuit `AttachConsole == 0 && AllocConsole == 0 → return` is correct: bails only when BOTH fail, leaves handles untouched (no crash, pre-fix behavior) ✓. `null` + `INVALID_HANDLE_VALUE` checks before each `SetStdHandle`; ignored SetStdHandle returns are acceptable best-effort ✓.

**main() reordering** (main.rs:107-141):
- Console branch: `attach_console()` is the first statement in the process — `std::env::args()` prints nothing, so no print precedes the attach and Rust's lazily-cached stdio picks up the rewired handles ✓. `migrate_legacy_config_dir()` still precedes the first config read (`run_console` → `crate::build_brain()` → `Config::load`) ✓, and now sits AFTER the attach so its eprintln can't pin stale handles ✓.
- GUI path: migration remains the first GUI-path statement, still before `browser_inspection_enabled()` and `build_brain()` ✓; behavior otherwise unchanged (attach never runs on the GUI path) ✓.

**Regression test** (console.rs:2483-2571):
- Genuinely exercises the changed path: the child is spawned with `DETACHED_PROCESS` + NUL stdio — the exact launch state of a GUI-subsystem binary — and the probe requires all three handles to pass `GetConsoleMode`. Under the attach-only implementation the handles stay NUL-device (non-NULL but non-console) → `GetConsoleMode` fails → exit 3 (matches the recorded pre-fix failure); only the rewiring passes it. No false pass: the `--exact` + full module-path filter is required for the child to run the test at all, and the hazard is documented in-line; the env-var flip exits before any spawn, so no recursion; the exit-code contract (0/3) is asserted via `status.code()` so a panic/abort in the child also fails. `#[cfg(windows)]` on both the test and `probe_console_handles` — safely skipped on non-Windows (modulo HIGH-1's ungated consts). Minor note, not a finding: on a console-less harness (hypothetical headless Windows CI), `AttachConsole` fails and `AllocConsole` carries the test — fine wherever AllocConsole succeeds.

**Bug-plan checklist:**
- Regression test exercises the changed path ✓ (above).
- Root cause documented ✓ — doc comment on `attach_console`, the test's doc comment (root cause + why NUL handles stand in for NULL), and the plan file.
- BUG: memory written ✓ — semantic record id `4c2b0805-586b-4ee5-860d-4c0dfcbd5d2a`, "BUG: --console from PowerShell invisible (AttachConsole doesn't populate std handles)", verified via memory_recall.

**Security / soundness:**
- The FFI block is `#[cfg(windows)]`-gated inside a `#[cfg(windows)]` fn; all pointers passed are live and correctly typed for the call; wide strings are NUL-terminated via `.chain([0])`. No unsoundness.
- `bInheritHandle = 1`: console device handles are inheritable so tool-spawned children share the console — standard CRT behavior; the console is the user's own terminal, no privilege surface opened. Acceptable and commented.

**Constitution compliance:**
- Doc comments present on the function and helpers (content issue = LOW-1); no `#[allow(...)]` anywhere in the diff ✓; Windows-green `cargo test` under `#![deny(warnings)]` proves warning-free on Windows ✓ (macOS gap = HIGH-1); all new Windows FFI sits behind `#[cfg(windows)]` with the non-Windows no-op twin retained ✓.
- Doc sync: README:71 "Console mode (`-console` / `--console`): the same brain as a terminal REPL, no GUI" remains accurate — this fix makes the documented feature work; no README/PLAN.md/module-doc updates required beyond the in-code fix of LOW-1.

---

## Summary

The core fix is correct and well-constructed: the Win32 recipe matches the documented console-device pattern, the main() ordering satisfies both the no-print-before-attach and migration-before-config-read constraints, and the regression test genuinely reproduces the defect (verified pre-fix failure / post-fix pass, no false-pass or recursion vectors). Three findings must be addressed before commit: gate the probe consts for non-Windows (HIGH — macOS `cargo test` build break under deny(warnings)), drop the stale pre-fix doc paragraph, and consider gating the SetStdHandle rewiring on absent/invalid current handles so redirection still works.

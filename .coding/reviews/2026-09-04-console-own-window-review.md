## Verdict: FINDINGS (0 high, 1 low)

# Review — Console REPL owns its window on Windows (wt/console-own-window)

Scope: ALL uncommitted changes vs HEAD — `src-tauri/src/console.rs` (AllocConsole-only
`attach_console` + new regression probe), `src-tauri/src/main.rs` (comment), `README.md`
(console-mode note), untracked `.coding/knowledge/bug/2026-08-23-console-attachconsole-shared-powershell-keystrok.md`
and `.coding/plans/b4288ccf-a999-47d8-9ada-4bbeccccfcaa.md`.

### LOW-1 — Spurious console window on redirected/piped release launches

`src-tauri/src/console.rs:1583` (`attach_console`, step 2).

A GUI-subsystem release binary launched from a terminal with redirection
(`mnemo-app.exe --console > out.txt`, or piped into another command) has NO console
(`GetConsoleWindow()` == NULL) but valid std handles for the redirected streams. New
code path: `AllocConsole()` unconditionally opens a console **window** that stays
visible for the whole session, even though nothing is printed to it (the valid file
handles are correctly preserved by the absent-handle guard). The old
`AttachConsole(parent)`-first path attached silently to the parent console in this
scenario and opened no window — so this is a behavior delta the docs don't cover: the
doc comment (`console.rs:1526-1527`) and README claim redirect protection only for the
*handles*, not the window.

Functionally output/input still work correctly (arguably better — stdin in a partially
redirected launch now comes from the owned console instead of stealing the parent's
CONIN$, which the old path did). Purely cosmetic/UX. Suggested fix: early-return before
`AllocConsole` when ALL three std handles are already valid (a fully redirected launch
needs no console), or extend the doc comment/README to mention the window in redirect
launches.

## Correctness, bugs, security — checked, no findings

- **Core fix is correct.** `GetConsoleWindow() != NULL` early-return keeps debug
  (console-subsystem) builds sharing the launching terminal — correct, since the shell
  *waits* for console-subsystem children, so sharing is safe there. The release path
  now allocates a console the process fully owns; PowerShell/cmd cannot split
  keystrokes with an owned console. The CONIN$/CONOUT$ rewire (absent-handles-only
  guard, INVALID_HANDLE_VALUE treated as absent, stderr sharing CONOUT$) is unchanged
  and still correct.
- **First bug (no output) not regressed.** `attach_console_rewires_std_handles_in_
  consoleless_child` still exercises the rewire on the same DETACHED + null-handles
  topology; acquisition via AllocConsole covers what AttachConsole's fallback did.
- **Blast radius.** Single production call site (`main.rs` `-console` branch, before
  any print — ordering preserved); no other callers relied on attach-to-parent
  behavior. (Codegraph still shows an `AttachConsole` edge — stale rebuildable index,
  not a code issue.)
- **Security.** FFI surface shrank (AttachConsole import + ATTACH_PARENT_PROCESS const
  removed). The env-var-driven `process::exit` branches exist only inside
  `#[cfg(test)] mod tests`. No new unsafe in production paths; inheritable console
  handles pre-existing.

## Regression test + bug-plan checks — PASS

- **Exercises the changed path:** the DETACHED grandchild runs `attach_console()`
  itself after nulling its std handles — the exact GUI-subsystem launch state
  (`console.rs:2657-2671`).
- **Discriminates (fails without the fix):** under the old code the grandchild's
  `AttachConsole(ATTACH_PARENT_PROCESS)` succeeds against the intermediate's
  CREATE_NEW_CONSOLE console → its HWND equals `MNEMO_PARENT_CONSOLE_HWND` →
  PROBE_FAIL; AllocConsole-only yields a distinct live HWND → PASS. Verified against
  both implementations, not just the comment.
- **Recursion/cross-talk safe:** PROBE env checked first; intermediate env removed for
  the grandchild; `--exact` full-path filters; distinct env names vs the sibling
  `MNEMO_CONSOLE_PROBE` test (safe even under cargo's parallel threads — each child
  runs `--exact` on its own test only).
- **Fail-closed:** intermediate without a console exits PROBE_FAIL; grandchild
  crash/kill maps to `unwrap_or(PROBE_FAIL)` / panic exit 101 ≠ PROBE_PASS.
- **Root cause documented:** knowledge file (symptom → root cause → fix → regression →
  path) + thorough doc comments in `console.rs` and `main.rs`.
- **BUG: memory written:** semantic record e3667323 + the untracked knowledge file.

## Constitution checks — PASS

- **Multi-platform neutrality:** every new/changed Windows-only item is
  `#[cfg(windows)]`-gated (`attach_console` body, new test, new
  `get_console_window_usize` helper); the `#[cfg(not(windows))]` no-op twin is intact
  and still called from `main.rs` (not dead code). `PROBE_PASS`/`PROBE_FAIL` remain
  gated — the prior HIGH-1 has not regressed. No `cfg(windows)` additions outside the
  console path; no `#[allow(...)]` anywhere; macOS build stays warning-free.
- **Doc comments:** changed function and new helpers documented; regression-test rule
  satisfied; full `cargo test` green in both crates proves zero warnings under
  `deny(warnings)`.
- **Documentation sync:** README note is accurate (release = own window, debug =
  launching terminal). PLAN.md's console mentions (lines 349, 753) concern model/
  effort selection — nothing stale there; endpoints.toml N/A.

## Non-blocking notes

- `cargo test` now briefly flashes console windows (CREATE_NEW_CONSOLE intermediate +
  grandchild AllocConsole) — inherent to a topology that can discriminate
  AttachConsole; cosmetic only.
- Optional memory hygiene (not a finding — history may coexist): older PLAN digests
  (4efed84e, 660cc537) and BUG a79616c6 describe "attach (or allocate)" as the fix;
  the new BUG e3667323 documents current behavior. The SPEC 1fb9d165 gist remains
  accurate (rewiring is still required) but could optionally be refreshed to
  AllocConsole-only.

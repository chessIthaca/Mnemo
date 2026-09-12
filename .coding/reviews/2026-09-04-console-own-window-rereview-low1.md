## Verdict: FINDINGS (0 high, 1 low)

# Re-review — Console REPL owns its window on Windows (wt/console-own-window), after the LOW-1 fix

Scope: ALL uncommitted changes vs HEAD — `src-tauri/src/console.rs` (AllocConsole-only
`attach_console` + redirect skip + two regression probes), `src-tauri/src/main.rs` (comment),
`README.md` (console-mode note), untracked `.coding/knowledge/bug/2026-08-23-console-attachconsole-shared-powershell-keystrok.md`,
`.coding/plans/b4288ccf-a999-47d8-9ada-4bbeccccfcaa.md`, prior review report. Previous verdict:
FINDINGS (0 high, 1 low) — spurious console window on fully redirected launches.

## Previous LOW-1 — RESOLVED

`console.rs:1591-1596`: after the `GetConsoleWindow()` early-return, if ALL three std handles
are valid (non-NULL, ≠ `INVALID_HANDLE_VALUE`), `attach_console` returns **before**
`AllocConsole` — a fully redirected/piped launch gets no spurious window. The
`handle_is_absent` closure is hoisted above the skip (1584-1587) and reused verbatim by the
step-4 rewire guards (1638, 1643, 1646), so the skip and the rewire guard share one
definition — no semantic drift. Doc comment rewritten to steps 1-4 (1517-1533) and matches the
code line-for-line. The fix is exactly the remedy the prior review proposed ("early-return
before `AllocConsole` when ALL three std handles are already valid"). Verified behavior matrix:
debug/console-subsystem → step-1 return; interactive release (all handles NULL) → own console +
full rewire (goal of the plan, unchanged); fully redirected → step-2 skip, handles keep pointing
at redirect targets; partially redirected → window + absent-handles-only rewire, documented as
intended; `AllocConsole` failure → handles untouched, no crash.

## LOW-1 (this round) — redirect-skip branch has no regression test

`console.rs:1588-1596` (new step-2 early-return).

The LOW-1 fix is a new behavior branch in production code, and **neither existing regression
test reaches it**:

- `attach_console_rewires_std_handles_in_consoleless_child` — the child calls
  `null_std_handles()` *specifically to route around* the redirection guard (its own comment,
  `console.rs:2596-2598`: the NUL-device handles `Stdio::null()` provides are VALID and would be
  kept) → all handles absent → skip does not fire.
- `attach_console_allocates_own_console_not_parent` — the grandchild likewise calls
  `null_std_handles()` (2673) → skip does not fire.

Deleting the step-2 early-return today leaves the entire suite green — the defect LOW-1
described (spurious window on fully redirected launches) could silently reappear. The project
constitution requires a regression test for every fixed defect ("must fail without the fix and
pass with it, so the defect can never silently reappear"), and the bug-plan check requires the
regression test to exercise the changed path; this changed sub-path is unexercised.

Suggested test (deterministic, reuses existing helpers): host spawns a `DETACHED_PROCESS`
child with all three stdio **piped** (`Stdio::piped()` — valid pipe handles = the exact
fully-redirected launch state; do NOT null handles this time), child runs `attach_console()`
and reports via exit code; assert `get_console_window_usize() == 0` — no window allocated.
Fails without the fix (`AllocConsole` succeeds → HWND ≠ 0), passes with it (skip → 0). Follow
the established probe pattern (`--exact` full-path filter, distinct env var, e.g.
`MNEMO_CONSOLE_REDIRECT_PROBE`, `#[cfg(windows)]`, fail-closed exit codes).

## Correctness, bugs, security — checked, no findings beyond the above

- **Fix logic correct.** `GetStdHandle` NULL *and* `INVALID_HANDLE_VALUE` both treated as
  absent, consistently in skip and rewire. Partial-redirect residual (window opens, only
  absent handles rewired) is documented and matches the prior review's accepted remedy.
- **Interactive path untouched.** All-NULL launch still gets AllocConsole + CONIN$/CONOUT$
  rewire; `attach_console_allocates_own_console_not_parent` (reported PASS after the fix)
  still discriminates AttachConsole vs AllocConsole by distinct HWND.
- **No dangling references.** `AttachConsole` FFI import and `ATTACH_PARENT_PROCESS` const are
  fully removed; all 11 remaining mentions are doc-comment/test prose. `handle_is_absent` is
  used on all platforms' only path (inside the `#[cfg(windows)]` body) — no dead code.
- **Security.** FFI surface shrank further; no new unsafe; env-var `process::exit` branches
  remain inside `#[cfg(test)]`-gated code only.

## Constitution checks — PASS

- **Multi-platform neutrality:** all new code lives inside the `#[cfg(windows)]`
  `attach_console` body; the `#[cfg(not(windows))]` no-op twin is intact (`console.rs:1653-1656`).
  `PROBE_PASS`/`PROBE_FAIL` (2517-2520), `null_std_handles` (2528), `probe_console_handles`
  (2551), `get_console_window_usize` (2634) all remain `#[cfg(windows)]`-gated — the earlier
  HIGH-1 has not regressed. No `#[allow(...)]`; no Windows assumptions outside the console path.
- **Doc comments:** `attach_console` steps 1-4 doc, `main.rs` comment, and helpers all
  documented and accurate.
- **Documentation sync:** README note (own console window on Windows release builds, debug
  keeps the launching terminal) is accurate at feature granularity; the redirect-skip is an
  implementation detail properly covered by the code doc comment. PLAN.md unaffected.
- **Bug-plan checks:** root cause documented (knowledge file: symptom → root cause → fix →
  regression → path; plus thorough doc comments); BUG: memory e3667323 written; plan is
  kind bug_fixing with all steps checked and the regression test recorded. The
  "regression exercises the changed path" check is satisfied for the AllocConsole path but not
  the new skip branch — that is the LOW-1 finding above.

## Non-blocking notes

- Reported-green tests rely on the main agent's unpiped `cargo test` runs (both console
  regressions PASS after the LOW-1 fix); reviewer is read-only and did not re-execute.
- Optional, not a finding: the knowledge file's `fix:` line could mention the redirect-skip,
  and SPEC 1fb9d165's gist still says "attach/alloc" — a `memory_supersede`/`memory_update`
  refresh (AllocConsole-only) would keep history tidy.

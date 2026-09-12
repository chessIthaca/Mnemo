## Verdict: PASS

# Final review — Console REPL owns its window on Windows (wt/console-own-window)

Scope: ALL uncommitted changes vs HEAD — `src-tauri/src/console.rs` (AllocConsole-only
`attach_console` + redirect skip + three regression probes), `src-tauri/src/main.rs` (comment),
`README.md` (console-mode note), untracked `.coding/knowledge/bug/2026-08-23-console-attachconsole-shared-powershell-keystrok.md`,
`.coding/plans/b4288ccf-a999-47d8-9ada-4bbeccccfcaa.md`, and the two prior review reports.
Prior verdicts: FINDINGS (0 high, 1 low) ×2.

## Prior finding 1 (LOW-1, round 1) — RESOLVED

Spurious console window on fully redirected/piped launches. `console.rs:1584-1596`: after the
`GetConsoleWindow()` early-return, if ALL three std handles are valid (non-NULL and
≠ `INVALID_HANDLE_VALUE`), `attach_console` returns BEFORE `AllocConsole`. The
`handle_is_absent` closure is hoisted above the skip and reused verbatim by the step-4 rewire
guards (1638, 1643, 1646) — one shared definition, no drift between skip and guard. Verified
behavior matrix: debug/console-subsystem → step-1 return; interactive release (all NULL) →
own console + full rewire (plan goal, unchanged); fully redirected → step-2 skip, handles keep
pointing at redirect targets; partially redirected → window + absent-handles-only rewire
(documented as intended); `AllocConsole` failure → handles untouched, no crash.

## Prior finding 2 (LOW-1, round 2) — RESOLVED

Redirect-skip branch had no regression test. New
`attach_console_skips_alloc_when_std_handles_already_valid` (`console.rs:2744-2789`) exercises
exactly the previously-unreached branch:

- **Reaches the skip:** DETACHED child (no console → step 1 does not return) with all three
  stdio **piped** — valid pipe handles, and the child deliberately does NOT call
  `null_std_handles()` (which would route around the guard, the gap the round-2 review
  identified). This is precisely the fully-redirected launch state.
- **Discriminates:** child runs `attach_console()` then asserts `GetConsoleWindow() == 0`.
  Without the skip, `AllocConsole` succeeds in the DETACHED child → HWND ≠ 0 → PROBE_FAIL;
  with the skip → PROBE_PASS. Fails without the fix, passes with it.
- **Sound probe mechanics:** parent uses `.spawn()` + `child.wait()`, so the `Child` struct
  holds the pipe ends open for the child's lifetime (and the child's own handle-table entries
  are valid values regardless); distinct env var `MNEMO_CONSOLE_REDIRECT_PROBE` vs the three
  sibling probes; `--exact` full-path filter matches the actual test path (same established
  pattern as the two siblings); fail-closed (child crash → 101/None ≠ PROBE_PASS);
  `#[cfg(windows)]`-gated. `let mut child` is consumed by `child.wait(&mut self)` — no
  unused-mut warning.

Deleting the step-2 early-return now fails the suite — the LOW-1 defect cannot silently
reappear.

## Correctness, bugs, security — no findings

- **Core fix correct and stable.** Step-3 `AllocConsole`-only (never attach the parent) with
  step-4 absent-handles-only CONIN$/CONOUT$ rewire is unchanged since the prior reviews, which
  verified it in depth; the redirect skip slots in before it without touching the interactive
  path (`attach_console_allocates_own_console_not_parent` still discriminates
  AttachConsole-vs-AllocConsole by distinct HWND).
- **No dangling references.** The `AttachConsole` FFI import and `ATTACH_PARENT_PROCESS` const
  are fully removed; all 11 remaining `AttachConsole` mentions are doc/test prose. No dead
  code (`handle_is_absent` used by both skip and rewire; `get_console_window_usize` used by
  two tests).
- **Blast radius.** Single production call site — `main.rs:123` `-console` branch, before any
  print (ordering preserved, Rust's cached stdio resolves after rewiring); every other
  reference is test/doc.
- **Security.** FFI surface smaller than pre-fix; no new unsafe in production paths; the
  env-var `process::exit` branches live only inside `#[cfg(test)] mod tests`; inheritable
  console handles are pre-existing and intentional (children may inherit).

## Constitution checks — PASS

- **Multi-platform neutrality:** every new/changed Windows-only item is `#[cfg(windows)]`-gated
  (the `attach_console` body, the new test, `get_console_window_usize`, and the pre-existing
  helpers `PROBE_PASS`/`PROBE_FAIL`/`null_std_handles`/`probe_console_handles`); the
  `#[cfg(not(windows))]` no-op twin is intact (`console.rs:1653-1656`) and still called from
  `main.rs:123`, so macOS stays warning-free with nothing ungated/dead. No `cfg(windows)`
  additions outside the sanctioned console path; no `#[allow(...)]` anywhere.
- **Doc comments:** `attach_console`'s steps 1-4 doc matches the code line-for-line; the new
  test's doc comment covers symptom, probe topology, and discrimination; `main.rs` comment
  updated; helpers documented. Public-function doc rule satisfied.
- **Documentation sync:** README note (`README.md:73`) is accurate at feature granularity —
  release = own window, debug = launching terminal. The redirect-skip is an implementation
  detail properly covered by the code doc comment. PLAN.md console mentions concern
  model/effort selection (checked in round 1); endpoints.toml N/A.
- **Regression-test rule:** the plan's defect (keystroke splitting) has
  `attach_console_allocates_own_console_not_parent`; the LOW-1 remediation now has
  `attach_console_skips_alloc_when_std_handles_already_valid`; the original rewire bug keeps
  `attach_console_rewires_std_handles_in_consoleless_child`. All three changed paths covered.

## Bug-plan checks — PASS

- **Regression exercises the changed path(s):** AllocConsole-only path — yes (HWND ≠ parent);
  redirect-skip sub-path — yes (new test, handles kept valid, no nulling).
- **Root cause documented:** knowledge file carries symptom → root cause → fix → regression →
  path, plus thorough doc comments in `console.rs` and `main.rs`.
- **BUG: memory written:** semantic record e3667323 (visible in project memory) + the
  untracked knowledge file. Plan is kind bug_fixing with all steps checked and the regression
  test recorded.

## Non-blocking notes

- The reviewer is read-only and did not re-execute the suite; the three attach_console
  regressions are reported PASS by the main agent's unpiped `cargo test` runs, consistent with
  prior rounds. Re-run `cargo test` (unpiped, per project rules) before commit as usual.
- Optional tidy-up (not findings): the knowledge file's `regression:` line could also list
  `attach_console_skips_alloc_when_std_handles_already_valid`; SPEC 1fb9d165's gist still says
  "attach/alloc" and could be refreshed to AllocConsole-only via `memory_supersede`.

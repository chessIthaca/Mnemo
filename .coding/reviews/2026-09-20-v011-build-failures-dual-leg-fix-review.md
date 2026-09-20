## Verdict: FINDINGS (0 high, 4 low)

Both fixes are correct, minimal, and verified; both regression tests genuinely pin their defect classes (fail-pre / pass-post independently reconstructed and confirmed); a full item-by-item gating audit of `src-tauri/src/watchdog.rs` (1315 lines) finds **no remaining ungated Windows-only code**, so macOS compilability is restored; no documentation updates are required; the shell-surgery justification holds and the mangled-comment repair is verified clean in-tree. The four LOWs are hygiene/robustness polish — none blocks the merge or the v0.1.1 re-release.

**Scope reviewed:** `git show` of all three commits (4ee6d7d, 32501d9, 02ec76b) in full; `git log` + `git diff HEAD` + `git status` (one uncommitted change: the plan file's step-4 tick); the complete post-fix `.github/workflows/build.yml` (219 lines); the complete `src-tauri/src/watchdog.rs` (1315 lines) audited item by item; both regression test files in full; `src-tauri/Cargo.toml` (windows-sys target gating); `.coding/plans/2327546a.md`; both BUG knowledge records (carried in the commit diffs); a code-graph caller check on `resolve_frame`; and repo-wide searches for other `CI:` assignments and stale doc references.

### 1. Fix 1 — CI env value (4ee6d7d + repair 32501d9): CORRECT

- The workflow-level `env: CI: "true"` (build.yml L39) is the right fix. The tauri CLI's `--ci` clap bool reads the `CI` env var and accepts only true/false; `"true"` parses cleanly. `"true"` is also exactly the value GitHub Actions itself injects by default, so every other CI-aware tool in the run (npm, tsc, vitest) sees the same truthiness it always did — no behavior change beyond un-breaking `npx tauri build`. Both jobs' tauri invocations benefit (windows L67 `npx tauri build`; macos L171 `npx tauri build --target aarch64-apple-darwin`).
- Searched `.github/**` for `CI:` — line 39 is the **only** env assignment (other hits are `npm ci` run steps and comments). Nothing else in the workflow reads or compares `CI`, so nothing else needed to change. Minimal fix, no scope creep.
- The inline comment (L36-38) documents the invariant and names the regression test — good in-place documentation.
- The 32501d9 repair is verified: current L36-38 is a well-formed three-line comment with the backticks intact (`"1" kills `npx tauri build` at startup`), `CI: "true"` at L39, valid YAML structure throughout (my own read confirms the PyYAML validation claim).

### 2. Fix 2 — cfg(windows) gate move (02ec76b): CORRECT, no Windows behavior change, nothing dangling on macOS

- The diff is a pure 5-line move: resolve_frame's doc comment + `#[cfg(windows)]` relocated from above `module_name_for`'s doc down to `fn resolve_frame` (now L799-803). Post-fix, `module_name_for` retains exactly its own gate (L777) and `resolve_frame` carries exactly one (L803). Pre-fix, `module_name_for` was double-gated (harmless on both platforms) and `resolve_frame` was ungated — the E0425 on macOS.
- **Windows: no behavior change.** Both functions compiled before (types in scope) and after (gated); same symbols, same bodies, no code edits.
- **macOS: nothing dangles.** `resolve_frame` is a private fn whose only caller is `capture_thread_stack_once` (L727 call; L654 def, gated at L653) — confirmed both by reading the full file and by the code graph (incoming calls: `capture_thread_stack_once` only; its own outgoing calls `ensure_symbols_ready`/`module_name_for` are both gated). With the gate restored, neither the fn nor any reference to it exists on non-Windows targets.
- **Full gating audit (multi-platform neutrality):** every Windows-only top-level item in watchdog.rs is gated — `thread_times_ms` (L518), `filetime_to_ms` (L555), all five `use windows_sys::…` statements (L568, 570, 576, 578, 580), `MAX_STACK_FRAMES` (L588), `ThreadGuard` (L595), `impl Drop for ThreadGuard` (L598), `capture_thread_stack` (L629), `AlignedContext` (L648), `capture_thread_stack_once` (L653), `ensure_symbols_ready` (L738), `module_name_for` (L777), `resolve_frame` (L803 — the fix), `mod wct` (L873), `capture_wait_chain` (L1020), and the two Windows-only tests (L1180, L1281). The cross-platform items (`Watchdog`, `detector_loop`, `start_with_threshold`, `ReportData`, `format_report`, etc.) reference Windows APIs only inside inline `#[cfg(windows)]` blocks with fully-qualified `windows_sys::` paths. `windows-sys` is a `[target.'cfg(windows)'.dependencies]` entry (src-tauri/Cargo.toml L48-55), so any ungated reference would fail macOS at resolve time — none remains. **macOS compilability is restored.**

### 3. Regression-test soundness

**`build_workflow_ci_env_is_clap_bool` (tests/integration/ci_workflow.rs) — sound, pins the invariant.**
- Fail-pre confirmed by reconstruction: pre-fix build.yml L36 was `CI: "1"` → the parsed value `"1"` fails the `true`/`false` assert with the exact line number. Post-fix it passes.
- No false positives on the current file: comment lines start with `#` (not `CI:`), `run: npm ci` lines start with `run:`, and no `SOMETHING_CI:` keys exist (prefix match is on the trimmed line start). The value parser handles quoted and bare forms.
- It scans **every** `CI:` key at any indent, so a future job- or step-level `CI:` assignment is covered too. It would not catch a shell-style `CI=…` inside a `run:` block — none exists today, and the defect class (YAML env assignment) is what's pinned. Acceptable and consistent with the sibling `secrets`-in-`if:` guard's design.
- Registration is inherited: the file is part of the root package's `tests/integration/` binary (`mod ci_workflow;` in main.rs, per the prior round-2 review), already built and run by `cargo test --workspace` on **both** CI legs.

**`watchdog_windows_only_items_stay_cfg_gated` (src-tauri/tests/gating.rs) — sound for its purpose; heuristic with known (documented-by-design) gaps.**
- I traced the algorithm over the entire current watchdog.rs: **zero false positives**. Every top-level item whose body references a listed identifier (`thread_times_ms`, `ThreadGuard`, `impl Drop`, `capture_thread_stack`, `capture_thread_stack_once`, `ensure_symbols_ready`, `module_name_for`, `resolve_frame`, `capture_wait_chain` — the last via the `OpenThread` ⊂ `OpenThreadWaitChainSession` substring, which is correct here since the item is genuinely gated) has its `#[cfg(windows)]` within the 10-line look-back window. Cross-platform items (`detector_loop`, `impl Watchdog`) pass because their Windows-only references are to identifiers deliberately **not** in the list — the list is correctly curated so that listed identifiers appear *only* inside fully-gated items; adding `thread_times_ms`/`capture_thread_stack`/`capture_wait_chain`/`GetCurrentThreadId` would false-positive on `detector_loop`'s inline-gated blocks.
- Fail-pre confirmed by reconstruction: pre-fix, `fn resolve_frame` sat at old L804 with module_name_for's body/`}`/blank in its 10-line window (module_name_for's own gate was ~22 lines up) → flagged, exactly as the BUG record states. Post-fix the gate is at L803, one line above the fn.
- Comment stripping (`//`-prefixed lines dropped) correctly keeps doc-comment mentions of `SymFromAddr` etc. from tripping the guard — verified against resolve_frame's own doc, which names three listed identifiers.
- The guard is textual and platform-independent, so it catches the defect class on **both** CI legs (the Windows leg was green throughout the incident — the guard would have flagged it there anyway). Good design.
- Coverage gaps (false negatives) — see LOW 2: `opens_item` misses `mod`, `use`, `pub const`, `type`, `trait`, `enum`, `pub(crate) fn`, `unsafe fn`; the `mod tests` interior is invisible (an ungated `#[test]` calling a gated helper would break the macOS compile yet pass the guard). No current instance — every such item is gated or identifier-free today — but the guard's protection is partial, and notably it does not enforce the very invariant the file's own L565-567 comment states (every `use windows_sys::…` must be gated — the "review H1" class).

### 4. Multi-platform neutrality — PASS

Covered by the full audit in §2: no ungated Windows-only code remains in watchdog.rs; the change adds no Windows-only APIs, paths, or shell syntax anywhere; the build.yml change is platform-neutral YAML. The fix restores the documented design (watchdog.rs module docs L44-53: "Only the *enrichment* … is `cfg(windows)`-gated … on those platforms the report degrades gracefully").

### 5. Documentation sync — no updates required

- README.md / PLAN.md: searched — zero references to the `CI` env value or to build.yml's env block (all `CI:` hits in `*.md` are `.coding/` historical records or unrelated identifiers like `contains_ascii_ci`). The README's CI claim ("builds and tests on both Windows and macOS") remains accurate — indeed this fix is what makes it true again.
- Module docs: watchdog.rs's module docs already describe the gating design the fix restores; no edit needed. Both new test files carry thorough module/test doc comments (including the defect history and run reference). The workflow's inline comment documents the new CI invariant in place. Nothing stale, nothing missing.

### 6. File-tools-first policy — justification holds; repair verified

- The stated justification (a verified `file_edit` transport freeze — repeated rejections of correctly-shaped calls after a genuine fresh read) is exactly the constitution's sanctioned escape hatch (b). It was stated in-session, and the fallback was to the two files the freeze blocked.
- The first shell edit demonstrated the policy's rationale: PowerShell backtick escaping turned `` `npx `` into a newline, leaving a column-1 fragment that made build.yml invalid YAML. This was **caught**, repaired in 32501d9 (single-quoted re-insertion), PyYAML-validated, and re-guarded — and the repair is verifiably clean in the current tree (L36-39 read back correct; both ci_workflow guards pass over it). The 32501d9 commit message transparently documents the failure mode. Process requirement (justification + repair + validation): met.
- Residual history cost: 4ee6d7d as committed carries the broken YAML (LOW 4) — the branch tip is clean, so nothing user-facing is affected.

### 7. Warning-free build

The watchdog.rs change is a pure move of five lines (doc + attribute) — no code, no identifiers, no imports touched; it cannot introduce a warning. The two test files use every item they declare (`WINDOWS_ONLY`, `opens_item` both referenced). The stated green `cargo test --workspace` under `#![deny(warnings)]` at both crate roots is consistent with the diffs. (Read-only reviewer — not re-run; the parent's closing sequence re-runs the suite anyway.)

### 8. Bug-plan checklist — complete

- **Regression tests exercise the changed paths:** both read the exact changed files and pin the fixed invariants; fail-pre/pass-post proven (task evidence + my independent reconstruction).
- **Root causes documented:** two BUG knowledge files committed (`.coding/knowledge/bug/2027-01-11-tauri-cli-ci-rejects-ci-1-windows-release-build.md`, `…-watchdog-resolve-frame-lost-its-cfg-windows-gate.md`), each carrying symptom → root cause → fix → regression test name → verification; both corresponding BUG memory records are live.
- **No scope creep:** the three commits touch exactly build.yml, watchdog.rs, two test files, two bug records, and the plan file (+ the uncommitted step-4 tick). Nothing else. Both fixes are minimal.
- Plan kind `bug_fixing`, steps 1-4 ticked, both regression tests recorded. The pending re-release (tag delete/re-tag/push) is correctly post-merge work, not review scope.

### Findings

**LOW 1 — tests/integration/ci_workflow.rs committed without a trailing newline.**
4ee6d7d's diff ends `\ No newline at end of file`; the file still ends at L73 `}` with no final newline (gating.rs, the plan file, and both bug records all have one). The repo's own review precedent treats exactly this class as fix-worthy (the v0.1.1 release round-1 review fixed a trailing-newline finding). **Fix:** append a final newline to tests/integration/ci_workflow.rs.

**LOW 2 — gating guard's item scan and identifier list leave documented-but-unenforced gaps.**
`opens_item` matches only `fn`/`pub fn`/`struct`/`pub struct`/`static`/`const`/`impl` at column 0 — it cannot see `mod`, `use`, `pub const`, `type`, `trait`, `enum`, `pub(crate) fn`, or `unsafe fn` items, and it never looks inside `mod tests`. Consequences today: (a) the file's own L565-567 comment states the "every `use windows_sys::…` must be gated" invariant (the prior review-H1 class), but the guard does not enforce it; (b) an ungated `mod` wrapping Windows-only code, or an ungated `#[test]` calling a gated helper, would break the macOS compile while passing the guard. No current instance — all such items are gated or identifier-free (verified item by item). The identifier list is correctly curated *against false positives* (helpers referenced from `detector_loop`'s inline-gated blocks must stay out), so list-side gaps are inherent to the textual approach. **Fix (suggested):** extend `opens_item` with `"mod "`, `"use "`, `"pub const "`, `"type "`, `"trait "`, `"enum "`, `"pub(crate) fn "`, `"unsafe fn "` — verified safe on the current file (`mod wct` gated at L873 within window; `mod tests` contains no listed identifiers; the five `use` lines each carry their gate within the window; `pub const DEFAULT_STALL_THRESHOLD` has no listed identifiers) — **and** extend the block terminator check to `lines[j] == "}" || lines[j] == "};"`, because multi-line `use` blocks close with `};` at column 0 (L575, L584) and the current `!= "}"` scan would run past them, silently skipping the items in between. Alternatively, document the gaps as accepted scope in the gating.rs module doc. Either resolution closes the finding.

**LOW 3 — plan-file bookkeeping: detailed-steps checkboxes stale; step-4 tick uncommitted.**
`.coding/plans/2327546a.md`'s "Detailed steps" leave **Root cause** and **Fix** unticked although both are demonstrably done (BUG records + fixes landed in 4ee6d7d/02ec76b); only the main Steps 1-4 are accurate. "Verify + re-release" correctly stays unticked (the re-release is post-merge work). The step-4 tick itself is the sole uncommitted change and must ride the final commit. **Fix:** tick the two completed detailed steps (or annotate them) and include the plan file in the closing commit.

**LOW 4 — 4ee6d7d is a broken intermediate commit (invalid YAML).**
As committed, 4ee6d7d's build.yml carries the backtick-mangled column-1 fragment (`px tauri build at startup`) — invalid YAML (the PyYAML ScannerError 32501d9 describes); a checkout of that commit alone would not load in Actions. The branch tip is clean, 32501d9 documents and repairs it, and main receives only the merge, so nothing user-facing is affected — this is history/bisect hygiene only. **Resolution (either acceptable):** squash 4ee6d7d+32501d9 into one commit before merge, or accept as-is with this report as the written record. No code change required.

### Bottom line

0 HIGH — both defects are correctly and minimally fixed, both regression tests are sound and proven fail-pre/pass-post, macOS compilability is restored with nothing dangling, docs need no updates, and the bug-plan checklist is complete. Fix LOW 1-3 (all trivial), resolve LOW 4 either way, re-run `cargo test --workspace`, then commit (including the plan-file tick and this report) and proceed to merge + the v0.1.1 re-release.

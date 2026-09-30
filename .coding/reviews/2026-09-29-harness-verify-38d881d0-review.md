## Verdict: FINDINGS (0 high, 1 low)

Review of plan 38d881d0 "Harness-run deterministic verify at plan-step boundaries" (backlog 1f767466), scope `git diff 5189ceb..cef8133` on wt/mnemo — the item's five commits only (346f4aa config → dfdce0e step_verify module → 7921e87 complete_step hook → 77868ae action=verify + volatile tail → cef8133 docs). The working tree is clean except `.coding/backlog.jsonl` bookkeeping; the reviewed code is the committed tree at cef8133.

**Summary:** The item is solidly built and matches the plan's decisions. Every acceptance criterion I could verify statically is genuinely met, the flag-off byte-identity and one-shot-drain invariants hold, the lock discipline at the hook is correct, and no raw-log or secret leak path exists in the note builder. One LOW finding: the timeout test proves prompt cancellation of the harness future but not the child-process kill (the acceptance's "cancels cleanly" kill half rests entirely on the untested `kill_on_drop(true)` line).

## What was read

- `git_read` log + show over all five commits in range (working tree clean except backlog bookkeeping).
- `src/agent/step_verify.rs` — the full 513-line module: `run_checks`, `evidence_note`, `pass_summary`, `fallback_body`, `cap_note` + all 8 unit tests.
- `src/tool/workflow/plan.rs` — `CompleteStepTool` (1355–1682), the hook region and the detailed-sub-step early return, plus the verify test block (4425–4551).
- `src/agent/turn.rs` — `step_lane_target` (1130–1291, the action=verify branch) and the request-seam drain (3370–3411).
- `src/agent/loop_impl.rs` 955–987 (`with_step_verify` / `step_verify` / `set_verify_note` / `take_verify_note`), `src/agent/optimizer.rs` `append_nudge`.
- `src/agent/factory.rs` — `verify` field, `with_verify_config`, `set_verify_config`, `step_verify_handle`, `register_workflow_tools` wiring; `src-tauri/src/main.rs` (verify_config Arc, line 1827 + `.with_verify_config` at 2032) and `src-tauri/src/ipc/rewire.rs` (live mirror, 189–193).
- `src/config/general.rs` — `VerifyConfig` (492–536) + serde conventions; `src/agent/reflex.rs` (Verify variant, parse, module docs), `src/agent/step_lanes.rs` (rung neutrality of Verify), `src/agent/tests.rs` (14559–14717, the three lane/one-shot tests).
- `src/tool/agent/output_compactor.rs` (compress redacts via `bump(redact_secrets(line))`), `README.md:43`, `PLAN.md:1362`, `Cargo.toml` (tokio already `features = ["full"]`).

Reviewed-state: cef813311794c64e01e52fd2ae90f02fa88805af

## Acceptance criteria — each checked against the code

1. **Green, warning-free build** — not independently verified (read-only reviewer, no shell). By inspection: all new pub items (`StepVerifyHandle` + methods, `VerifyOutcome` + variants, `VerifyConfig` + fields, `with_step_verify`/`step_verify`/`set_verify_note`/`take_verify_note`, `set_verify_config`, `with_verify_config`) carry doc comments; nothing found that would warn under `deny(warnings)`. The parent should confirm the final `cargo test` run before finish.

2. **Flag off = byte-identical, zero spawns** — MET. `run_checks` (step_verify.rs:132-139) returns `None` before any spawn when `enabled` is off or `test_command` is blank/whitespace; `CompleteStepTool` without a handle skips the hook entirely (plan.rs:1622-1628). The unit test `inert_handle_does_not_run_even_a_bogus_command` is honest: a regressed guard would run `__mnemo_no_such_command__`, fail, and return `Some` — the `is_none()` assert catches it. `disabled_verify_handle_leaves_the_tick_output_byte_identical` (plan.rs:4476-4489) compares the FULL output strings of no-handle vs wired-disabled real tool executions with `assert_eq` — any byte drift fails it.

3. **Failing suite → compact evidence note, conciseness unit-tested** — MET. `huge_failure_output_stays_compact` (10k panic lines → ≤2000 bytes) and `oversized_line_dumps_are_truncated_inside_the_cap` (3× 3000-char lines → ≤2000 bytes, ends with the truncation marker) both assert `note.len() <= MAX_NOTE_BYTES`; the fallback test asserts repeat-counts and the dropped-lines note.

4. **Timeout cancels cleanly, triage-able failure** — MET in behavior, test-strength gap (finding L1). `VerifyOutcome::TimedOut` renders a failure-shaped note ("timed out after Ns and was killed (verification failed)") and `timed_out_note_is_failure_shaped` pins it; the runtime test asserts a 10s sleep under a 1s bound returns in <8s with the TimedOut outcome.

5. **No new dependencies** — MET: `tokio = { features = ["full"] }` already present; step_verify uses only std + tokio + existing crate modules. **No hardcoded vendor model ids** — none; the only command is the user's own config string. **Docs synced** — MET: README.md:43 (the Laya-classifier row gains the full `[general.verify]` sentence), PLAN.md:1362 ("Harness-run deterministic verify (2027-01, backlog 1f767466)" bullet), step_verify.rs module docs, and reflex.rs's module doc now names the shipped consumer (no stale "later items" phrasing remains).

## Risk focus — verified points

- **Lock discipline (complete_step hook):** the `wf` guard is explicitly `drop(wf)`'d at plan.rs:1612 BEFORE `run_checks().await` (1609-1611 comment states the reason); no other lock is held across the await (the verify-config read lock is released before spawn; the optimizer read lock inside `evidence_note` is synchronous and brief). A failed verification can never flip `success` — `result.success` is only read at 1622, never written after the tick; output is append-only, so no double-tick is possible (`complete_step` ran once; the hook never touches the Workflow). The detailed sub-step tick returns at 1512-1532, structurally above the hook, with a control test (`detailed_sub_step_ticks_never_verify` — skeleton ticks DO verify, the detailed tick does not).
- **Note builder cap logic:** `cap_note` applies unconditionally to every path (Passed/Failed/TimedOut, compressor and fallback alike) — no path can exceed 2000 bytes; truncation is char-boundary-safe (`is_char_boundary` walk-back) with the marker inside the budget. `fallback_body`'s dropped-line counting is correct (usize::MAX sentinel keeps post-cap repeats from double-counting; the count is distinct lines, as the wording says). Redaction is total: `pass_summary` → `redact_secrets`; fallback → `redact_secrets` per line; the lever-2 compressor path redacts inside `bump(redact_secrets(line))` (output_compactor.rs:378-382); the head names only the user's own configured command. No raw-log or secret leak path found.
- **One-shot drain + volatile tail:** `take_verify_note` is destructive (`slot.take()`), drained at the request seam (turn.rs:3403) and passed through `append_nudge`, which is a byte-identical no-op for `None`/empty — same channel and guarantees as lever-7, placed after the tail body and before the byte-stable CONTEXT_FOOTER, so the cached prefix is never touched. Tests: `verify_note_is_one_shot` (drain semantics), `lane_verify_action_without_a_handle_stashes_no_note` (empty slot → no `[verify]` in any captured tail), and the with-handle test rides a REAL `run_turn` and asserts the note text in `RecordingProvider::request_tails()`.
- **Live-config Arcs:** ONE `Arc<RwLock<VerifyConfig>>` — created in src-tauri/main.rs:1827 from the loaded config, installed via `.with_verify_config(verify_config)` (main.rs:2032); `factory.step_verify_handle` clones it into BOTH consumers (the complete_step tool at factory.rs:1303-1305 and the loop at 1081), and `rewire.rs:193` mirrors a Settings save via `factory.set_verify_config` — so a save lands on the next boundary with no rebuild, exactly the optimizer-lever pattern.
- **Shadow-mode correctness:** the action=verify branch (turn.rs:1252-1258) sits inside `ReflexOutcome::Decided` and fires regardless of `routed` (which is `None` in shadow), so the evidence flows while the ladder stays observational — and the lane test runs with `enforce: false`, pinning exactly that. `rung_from_decision` treats Verify as rung-neutral (step_lanes.rs:396-405 test).
- **Multi-platform:** `cfg!(target_os = "windows")` selects `powershell -Command` vs `sh -c` (the shell tool's idiom), and the `CREATE_NO_WINDOW` flag is inside a `#[cfg(windows)]` block — compiles and behaves on both platforms.

## Findings

### L1 (low): the timeout test proves prompt cancellation, not the child-process kill

`timeout_cancels_cleanly_and_reports_the_elapsed_bound` (step_verify.rs:379-400) asserts a 10s sleep under a 1s timeout returns in <8s with `TimedOut`. But `tokio::time::timeout` returns at the deadline regardless of what happens to the child — if `kill_on_drop(true)` (step_verify.rs:151) were ever removed in a refactor, this test would still pass while the orphaned `sleep 10` process lingered. The test comment ("The kill path the design relies on, empirically asserted") and the commit message overclaim: what is empirically asserted is that the harness future doesn't wedge, not that the child dies. The acceptance's "the timeout path cancels cleanly" has an untested half.

Suggested fix (small): make the child's death observable — run a command like `sleep 10 && echo done > marker.txt` (PowerShell: `Start-Sleep 10; Set-Content marker.txt ok`) under a 1s timeout, then after a couple of seconds assert the marker file was never created. That test fails if `kill_on_drop` is dropped. Alternatively, keep the current test and soften its comment/commit claim to "cancels promptly" — but the marker-file form genuinely pins the kill.

## Constitution checks

- **Documentation sync** — README row, PLAN.md bullet, module docs, and reflex.rs consumer doc all updated; no stale "later items"/"nothing acts yet" text remains for this consumer. No finding.
- **Multi-platform neutrality** — platform split is runtime/`cfg`-gated exactly like the shell tool's sanctioned idiom; no ungated Windows-only API. No finding.
- **File-tools-first** — no shell-based file mutation anywhere in the diff. No finding.
- **Warning-free build** — no `#[allow(...)]` added; all pub items documented. (Test run itself not independently verifiable by this read-only reviewer — see acceptance #1.)
- **`.coding/**` accuracy** — plan 38d881d0's goal/steps match what shipped (steps 1-4 ticked, step 5 in progress — this review is part of it); no factually wrong claims found. Line-level review intentionally not performed, per scope.

**Verdict restated: FINDINGS (0 high, 1 low).** Fix L1 (strengthen the test or correct its overclaim), re-run `cargo test` unpiped with `$LASTEXITCODE`, then commit with this report and finish.

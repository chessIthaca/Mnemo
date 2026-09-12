## Verdict: PASS

**Re-review of a completed plan whose closing sequence was never finalized.** The code is already committed (`c7fca6a`) and merged to main via `dd481a4` (Merge branch 'fix/run-all-steer-double-turn'); there are NO uncommitted changes. This re-review verifies, against the **current main source**, that the original review's 3 LOW findings were addressed and that the root fix, regression tests, and constitution compliance all hold.

**Original review:** `.coding/reviews/2026-08-20-run-all-steer-double-turn-review.md` — verdict "root fix correct and complete", 3 LOW findings. Its "Recommended actions before commit" asked to fix findings 1 & 2 (two one-line comment edits) and for finding 3 to either apply the suggested re-check or document the residual window.

---

## Original findings — all 3 addressed

### 1. LOW (stale comment) — `src-tauri/src/ipc/run_all.rs` — ADDRESSED

The stale `// Inject the unattended-mode steer, then dispatch the prompt.` was replaced. Current source at lines 441–442 now reads:

```rust
// Lock the manager, re-check busyness, then dispatch the single combined
// prompt.
```

Accurate and consistent with the corrected comment block immediately below it (lines 489–493: "ONE command, ONE turn … must NOT be sent as a separate Suggestion"). No contradiction remains.

### 2. LOW (stale module comment) — `src-tauri/src/ipc/backlog_cmds.rs` — ADDRESSED

The module header no longer claims Run-All items get "an unattended-mode steer". Current source at lines 32–34 now reads:

```rust
// - **Run-All**: the unattended loop. Each item gets a git checkpoint, an
//   unattended-mode preamble embedded in its dispatched prompt (see
//   `run_all::run_all_prompt`), and a commit/rollback on resolution.
```

Correctly describes the embedded-preamble design and cross-references `run_all::run_all_prompt` as the original review's recommended fix specified.

### 3. LOW (residual race) — `src-tauri/src/ipc/run_all.rs` — ADDRESSED (both options applied)

The original review offered a choice: apply the suggested post-checkpoint re-check OR document the residual window. **Both were applied** — strictly more than required.

- **Re-check implemented** (lines 448–487): after re-acquiring the manager lock post-checkpoint, `busy_after_checkpoint` re-runs the same predicate (`is_running() || has_running_descendants(main_id)`). If busy, the item is reverted to `Pending` (keeping the checkpoint sha in its note for traceability), the run's `current_item` pointer is cleared to `None` so the interfering turn's resolution cannot stamp it, and the function returns `Ok(())` — the item re-waits and is dispatched when that turn resolves. This mirrors the single-dispatch path's one-lock-hold pattern.
- **Residual window documented** (lines 455–460): a comment block explicitly notes the forwarder flips the running flag only when it *processes* the `Started` event, so a just-started turn can briefly read idle; it states this window is narrow/probabilistic vs. the deterministic double-turn removed, and that closing it fully would require moving the running flag into the send path.

---

## Verified clean (review focus)

1. **Root fix is sound — one command, one turn.** `run_all_prompt` (`run_all.rs:119–121`) builds the single `Prompt` text as `format!("{RUN_ALL_STEER}\n\n---\n\n{item_text}")` — preamble + task in one string. The dispatch site (`run_all.rs:494–496`) sends exactly ONE `AgentCommand::Prompt { text: run_all_prompt(&item.text), images: item.images.clone() }`; the old `manager.send(main_id, AgentCommand::Suggestion(RUN_ALL_STEER...))` line is gone. Repo-wide search for `Suggestion(RUN_ALL_STEER` returns **no matches** — no remaining separate-steer senders of the preamble. `emit_prompt_dispatched` still emits the raw `item.text` (line 502) so the UI goal bubble stays clean; the deliberate divergence is documented at the call site (lines 498–501).

2. **Regression tests exist and are meaningful.**
   - `run_all_prompt_embeds_preamble_and_task_in_one_text` (`run_all.rs:188–215`): asserts the preamble leads, rule (4) (the plan-loop contract) is embedded, the `\n\n---\n\n` separator splits preamble from task, and the task text rides verbatim at the end. Directly pins the prompt shape the fix depends on.
   - `idle_suggestion_then_prompt_runs_two_sequential_turns` (`src/runtime/agent.rs:1369`): the runtime contract pin — an idle `Suggestion` runs its own turn before a queued `Prompt`, so the two never merge. Asserts exactly 2 `Started` / 2 `Finished` and that the second `Started` follows the first `Finished` (sequential). Timing-tolerant (handles both the Prompt-queued and absorbed-as-mid-turn-steer paths). Would fail if the idle-Suggestion arm ever changed to buffer instead of running a turn, which is exactly the contract `run_all_prompt`'s rationale rests on.

3. **Constitution compliance.**
   - `pub fn run_all_prompt` (module is `pub mod run_all`) carries a thorough doc comment (`run_all.rs:107–118`) explaining the defect and the one-command/one-turn rationale.
   - No `#[allow(...)]` silencing anywhere in the diff: searches of `src-tauri/src/ipc/run_all.rs` and `src/runtime/agent.rs` for `#[allow` return **no matches**. The build is warning-free under `#![deny(warnings)]` at both crate roots.
   - No dead code: `RUN_ALL_STEER` is still used (by `run_all_prompt`); the new `busy_after_checkpoint` re-check path is reachable.
   - Multi-platform neutrality: no Windows-only APIs/paths/shell syntax in the changed library code (pure Rust + tokio + tauri state).

4. **Tests pass.** Per the main agent's confirmation, both crates are green: root `cargo test` 1483 passed, `src-tauri` `cargo test` 167 passed (the higher counts vs. the original review's 1112/85 reflect tests added in the intervening work; both crates pass under `#![deny(warnings)]`, proving zero warnings). No uncommitted changes exist to re-test.

5. **Merge integrity.** `git show --stat dd481a4` confirms the merge of `fix/run-all-steer-double-turn` touched exactly the expected files (`run_all.rs`, `backlog_cmds.rs`, `agent.rs`, the plan, the review, `stack.json`) — 330 insertions, 15 deletions. The current main source matches the fix commit's intent for all three findings.

---

## Note on the original 3 findings

All three LOW findings from the original review were addressed in the committed code (`c7fca6a`, merged via `dd481a4`): findings 1 and 2 are the two one-line stale-comment corrections the review requested; finding 3 was satisfied by **both** the suggested post-checkpoint re-check (revert item to `Pending` + clear `current_item`) **and** a comment documenting the accepted residual window — exceeding the review's "either/or" ask. No new issues were found. The plan's closing sequence can now be finalized.

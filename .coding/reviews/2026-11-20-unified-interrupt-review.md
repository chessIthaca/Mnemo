# Review: Unified Interrupt/Cancel/Steer Safe-Stop Propagation

**Scope:** All uncommitted changes (`git diff HEAD`) — 10 files, +973/−160.
**Goal:** Make Interrupt, Cancel, and Steer flow through one `StopReason` signal
checked at every safe point, fixing 5 interrupt-handling bugs.

## Verdict

The core design is sound and correctly implemented. `StopReason` is set on every
Interrupt/Cancel path and checked at every safe point (streaming, approval,
`ask_user`, summarization, between tool calls, mid-tool, `complete_with_retry`).
Cancel reaches `AgentTask::run` and emits `Exited`; Interrupt ends the turn but
keeps the agent alive. Mid-batch synthesis produces N tool results for N tool
calls on both paths. The pinned `select!` loops re-poll correctly after buffering
Steers. The `complete_with_retry` future borrow is correctly scoped in a block
before `messages.pop()`. The `dispatch_with_interrupt` `None` arm awaits the tool
(no busy loop). No `#[allow(...)]` suppressions added.

**One low-severity finding** (consistency gap introduced by the new mid-tool-drop
path). Everything else is clean.

---

## Findings

### Low — `git_diff.rs` missing `kill_on_drop(true)` (consistency gap, new drop path)

**File:** `src/tool/agent/git_diff.rs:38-40`

The new `dispatch_with_interrupt` (dispatch.rs:322-355) wraps every tool dispatch
in a pinned `select!` that **drops the tool future** on Interrupt/Cancel. The
dispatch doc comment (dispatch.rs:313-316) claims:

> `kill_on_drop(true)` (shell, git) kills the child process; browser connections
> are dropped; `spawn_blocking` tasks … complete in the background

This diff added `kill_on_drop(true)` to `git.rs::run_git` (git.rs:91, the
mutating git tool) but **not** to `git_diff.rs::run_git` (git_diff.rs:39), which
spawns git via the identical `tokio::process::Command` pattern and whose own
module doc (git_diff.rs:8) states it "Mirrors `GitTool::run_git`."

`git_diff` is `AutoRun` (read-only), so it routes through the `auto_approved`
branch (dispatch.rs:207-211) → `dispatch_with_interrupt`. If an Interrupt/Cancel
arrives while `git diff`/`git status`/`git log` is running, the pinned future is
dropped, but `git_diff`'s `Command` has no `kill_on_drop` — so the child git
process is **orphaned** (continues running until it finishes on its own).

**Why this is new:** Before this diff, `git_diff` ran via a plain
`self.tools.dispatch(&parsed_call).await` (no `select!`, no drop) — the process
always completed. The mid-tool-drop path is introduced by this diff, so the
orphaning scenario is a regression in process hygiene.

**Impact:** Low. The orphaned process is a read-only git subcommand that
completes and exits on its own (no side effects, no data corruption). The only
risk is a briefly-orphaned child process. But it is a real consistency gap: the
sibling `git.rs` was hardened and `git_diff.rs` was missed, and the dispatch
doc comment's "kill_on_drop … (git)" claim is misleading as long as one git tool
lacks it.

**Fix:** Add `cmd.kill_on_drop(true);` to `git_diff.rs::run_git` after
`cmd.args(args).current_dir(&self.project_root);` (mirroring git.rs:87-91).

---

## Focus-point checklist (all verified clean)

1. **StopReason set on every Interrupt/Cancel path** ✓ — streaming
   (turn.rs:667-680), approval (dispatch.rs:262-273), `ask_user`
   (dispatch.rs:438-451), summarization (context.rs:209-219), between tool calls
   (turn.rs:893-910), mid-tool (dispatch.rs:335-342), `complete_with_retry`
   (turn.rs:460-471).
2. **Cancel → Exited; Interrupt → agent alive** ✓ — `run_turn_with_retry` returns
   `true` only on `Cancel` (agent.rs:89), `false` on `Interrupt`/`None`
   (agent.rs:90); `run` breaks on `true` so the post-loop `Exited` fires
   (agent.rs:268-273, 311-314, 351). Tests `cancel_during_streaming_exits_agent`
   and `interrupt_during_streaming_keeps_agent_alive` verify both.
3. **Mid-batch synthesis: N results for N calls** ✓ — between-tool-call path
   synthesizes `tool_calls[i..]` (turn.rs:918, includes the not-yet-started
   current call); hard_stop path synthesizes `tool_calls[i+1..]` (turn.rs:1203,
   excludes the just-completed current call whose real result was pushed at
   turn.rs:1062). Both yield N total across the batch.
4. **`kill_on_drop` on git.rs run_git** ✓ (git.rs:91) — but see the finding
   above for the missed `git_diff.rs`.
5. **Pinned `select!` re-polls after buffering Steers** ✓ —
   `dispatch_with_interrupt` loops after `buffered.push(other)` (dispatch.rs:344);
   `complete_with_retry` loop continues after `buffered_steers.push(s)`
   (turn.rs:473). Both re-poll the pinned future.
6. **No `#[allow(...)]` suppressions added** ✓ — the only `#[allow(...)]` in
   `loop_impl.rs:245` is pre-existing (`clippy::too_many_arguments` on `new`).
7. **Build warning-free** — no dead code / unused imports introduced. New
   `use crate::tool::ToolResult;` (turn.rs:26) and `use super::StopReason;`
   (turn.rs:18) are both used. `buffered_steers` (turn.rs:452) is consumed at
   turn.rs:512. (Could not run `cargo test` as a read-only reviewer, but no
   warning sources are apparent in the diff.)
8. **`dispatch_with_interrupt` `None` arm** ✓ — on channel close it does
   `return (&mut tool_fut).await;` (dispatch.rs:350), awaiting the tool directly
   and exiting the function. No loop re-poll of `cmd_rx` → no busy loop.
9. **`complete_with_retry` borrow scoping** ✓ — `complete_fut` is created and
   `tokio::pin!`'d inside a `{ … }` block (turn.rs:453-486); the block ends
   (dropping the future + its `&messages` borrow) before `messages.pop()`
   (turn.rs:487-488). Compiles correctly.

### Additional observations (not findings — moot by construction)

- The new error strings `"interrupted during execution"`,
  `"cancelled during execution"`, `"ask_user interrupted while awaiting answer"`,
  and `"ask_user cancelled while awaiting answer"` are **not** in
  `is_user_denial_tool_output` (turn.rs:1281-1287), so they would increment
  `tool_error_count`. This is harmless: every one of these strings is produced
  alongside `*stop_signal = Some(Interrupt|Cancel)`, which sets `hard_stop =
  true`, which ends the turn immediately — so `tool_error_count` is never read
  again. No action needed; noted only for completeness.

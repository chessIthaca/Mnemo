+++
title = "Run-All dispatch is one-command-one-turn (preamble embedded in prompt)"
created = "2026-08-24"
+++

SPEC: Run-All backlog dispatch (post-2026-08-20 fix, merged `c7fca6a`/`dd481a4`).

Run-All (unattended backlog loop) dispatches each item as ONE command in ONE turn: the unattended-mode preamble (`RUN_ALL_STEER`) is embedded IN the dispatched `Prompt` text via `run_all_prompt(&item.text)` (`src-tauri/src/ipc/run_all.rs`), NOT sent as a separate `AgentCommand::Suggestion`. Rationale: a `Suggestion` landing on an idle agent runs as its own full turn, which resolved every item on a steer-only turn and dispatched the next item before the real prompt ran (the double-turn defect).

Dispatch guard (`run_all_dispatch_next`): a busy check runs BEFORE the git checkpoint; after re-acquiring the manager lock post-checkpoint, a SECOND `busy_after_checkpoint` re-check (`is_running() || has_running_descendants(main_id)`) reverts the item to `Pending` (keeping the checkpoint sha in its note) and clears `current_item` if the agent became busy during the multi-second checkpoint. Accepted residual window: the forwarder flips `running` only when it processes the `Started` event, so a just-started turn can briefly read idle — narrow/probabilistic vs. the deterministic double-turn removed.

Regression tests: `run_all_prompt_embeds_preamble_and_task_in_one_text` (run_all.rs) pins the prompt shape; `idle_suggestion_then_prompt_runs_two_sequential_turns` (src/runtime/agent.rs) pins the runtime contract that an idle Suggestion runs its own turn before a queued Prompt.

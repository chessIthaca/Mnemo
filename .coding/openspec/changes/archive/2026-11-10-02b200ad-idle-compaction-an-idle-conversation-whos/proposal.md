When an agent conversation of at least 50,000 tokens has sat idle past its model's measured prompt-cache lifetime, Mnemo summarizes it with the compaction model ([models.summarize]), so the next message does not resend the whole context at the uncached rate. A Settings → Savings switch "Idle compaction" turns it on and off; it is on by default.

WHY: Claude Code compacts an idle chat. Its documented triggers are about an hour idle, at least 200k tokens and the 1-hour prompt cache. That way the next message does not resend a cold context at full price. Mnemo compacts only when the context fills (`summarize_at_fill_rate` × window, src/agent/turn.rs `maybe_compact`) or on a manual /compact (src/runtime/agent.rs `compact_context`).

Mnemo's own request_stats show how fast its providers' caches fade. The table gives the mean cache-hit share by idle gap since the previous request of the same session:

| Model | 5–10 min | 10–30 min | 30–60 min | over 1 h |
|---|---|---|---|---|
| deepseek-v4.1-flash | 89% | 34% | 25% | 0–8% |
| glm-5.3 | 71% | 45% | 29% | 0% |
| kimi-k3.1-pro | 76% | 83% | 13% | ~0% |

Once a cache has gone cold, the next request resends everything at the uncached rate. A cheap model summarizing the cold context costs far less. For a 150k-token context that is about $0.06 on deepseek-v4.1-flash, against about $0.30 for kimi-k3.1-pro resending it. The user's config already sets `[models.summarize]` to deepseek-v4.1-flash. The user decided the cheap model's summary is good enough.

DECISIONS (taken with the user):
1. TRIGGER. All four conditions must hold:
   - The agent has sat idle at least the measured cache lifetime of the model its last turn ran on. The lifetime is measured per model from this project's request_stats (`MemoryStoreTrait::cache_lifetime_secs`); when the data cannot decide, it is 3600 s.
   - The conversation holds at least 50,000 tokens (`IDLE_COMPACT_MIN_TOKENS`).
   - A dedicated compaction model resolves: `[models.summarize]` is set and its model id differs from the model the agent's last turn ran on.
   - The switch is on.
   There is no other knob: the app decides the delay and the minimum.
2. NO CACHE RACE. The summary is written by the compaction model after the agent's cache is already cold, so it is applied at once. Claude Code's rule "use the summary only if the cache is cold on the next message" holds by construction: the trigger fires only after the cache lifetime has passed, and no request happens in between.
3. RUNNING WORKERS DON'T BLOCK IT. Running workers or subagents do not block idle compaction: a report arriving later would hit a cold cache anyway. An agent mid-turn is never idle, because the trigger lives only in the between-commands wait of `AgentTask::run`.
4. ONE PER IDLE SPELL. After an attempt, whatever its outcome, the agent needs a new command before it can arm again.
5. INPUT DURING THE SUMMARY IS KEPT. `compact_context` already buffers and re-injects a Prompt or Suggestion that arrives mid-summary. The idle path then runs a turn for it. The manual /compact arm keeps today's behaviour.
6. WHERE THE SWITCH LIVES. The switch is `[general.optimizer] idle_compaction` (default true). It rides the existing live optimizer mirror (`AgentLoop::optimizer_config()`), so a Settings save takes effect without a restart. It shows on Mnemo2's Settings → Savings page as "Idle compaction". Mnemo1 agents share the runtime and get the same default; Mnemo1's settings UI is not changed.
7. SAVINGS ROW. Each idle compaction that shrinks the context records a savings row of kind `idle_compaction` (tokens_before = context before, tokens_after = context after). The Cost tab labels it "Idle compaction". src-tauri/src/mnemo2/insights.rs `lever_value` already values unknown kinds as `LeverValue::Entry`: the first resend at the input rate plus the cached resends. That is exactly what an idle compaction avoids, so insights.rs needs no change.

CONSTRAINTS:
- Don't change the fill-rate auto-compaction in turn.rs or the manual /compact behaviour.
- No schema change: request_stats already has `idx_request_stats_model` and a `purpose` column.
- No settings beyond the one switch.
- Claude Code as external lead is not an AgentTask and is unaffected.
- Never touch main.
- src/memory/schema.rs DDL text stays as it is.

FILE ANCHORS:
- src/config/general.rs: `OptimizerConfig` ~l.696, `is_default` ~l.775, `Default` ~l.792, test `optimizer_levers_default_on_and_round_trip` ~l.2012.
- src/config/settings_dto.rs: `SettingsSaveDto` optimizer fields ~l.335-393; apply block ~l.691-713; test `optimizer_patch_applies_levers` ~l.1050.
- src/memory/mod.rs: `SAVED_TOKEN_LIFETIME_REQUESTS` ~l.204; trait `savings_valuation` ~l.505; impl `savings_valuation` ~l.2785; `MemoryStore.now` field.
- src/memory/types.rs: `RequestStats`.
- src/agent/loop_impl.rs: `model_resolver` field ~l.413, `with_optimizer` ~l.1378, `optimizer_config` ~l.1389, `summarize_provider` ~l.2212, `resolved_model` ~l.2003, `provider()` ~l.2762.
- src/agent/turn.rs: `record_savings_event` ~l.4536 (pub(crate) on AgentLoop).
- src/runtime/agent.rs: `AgentTask` ~l.40, `AfterTurn`/`CompactOutcome` ~l.147/174, `AgentTask::new` ~l.204, `drive_turns` ~l.530, `run` ~l.887 (the `while let Some(cmd) = cmd_rx.recv().await` loop), the `AgentCommand::Compact` arm ~l.1090, `compact_context` ~l.1248, test `compact_success_emits_compacted_event_with_reduction` ~l.6673, `MockProvider` ~l.1705.
- src-tauri/src/mnemo2/model_settings.rs: `OptimizerWire` ~l.135, wire fill ~l.190, `OptimizerPatch` ~l.411, `apply_optimizer` ~l.430.
- src-tauri/src/ipc/settings.rs: optimizer wire asserts ~l.1728.
- mnemo2/src/lib/modelSettings.ts: `OptimizerSettings` ~l.54, `OPTIMIZER_LEVERS` ~l.117, `optimizerDraft` ~l.158, `optimizerWire` ~l.212.
- mnemo2/src/components/SavingsPage.svelte ~l.30.
- mnemo2/src/lib/cost.ts: `LEVERS` ~l.174.

VERIFY:
- `cargo test --lib` (all green)
- `cargo build --tests`
- `cargo check --workspace --tests`
- `npm --prefix mnemo2 run check` (0 errors 0 warnings)
- `npm --prefix mnemo2 test`
- `npm --prefix frontend test`
- `npx --prefix frontend tsc --noEmit -p frontend`

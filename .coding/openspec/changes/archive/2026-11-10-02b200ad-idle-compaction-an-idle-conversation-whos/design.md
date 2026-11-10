## Decisions

- 2026-11-10: added step 5 (Integration check) because steps 2 and 3 ran in parallel
- 2026-11-10: Clarifies decision 4 and step 2: - A Prompt or Suggestion buffered during an idle summary counts as the new command. After its resumed turn, run_idle_compaction re-opens the idle spell (idle_since = now), so a later idle compaction can fire after another full cache lifetime. Without such input, the agent waits for a real command before it can arm again. - dedicated_summarizer_model compares against the provider a summary would actually ride. With [models.summarize] set, it returns the slot's model only when summarize_provider really built that model (a failed build, e.g. a missing key, falls back to the turn provider and yields None) and that model differs from the last turn's model. A pinned override still wins.

## Interfaces

- 2026-11-10: `src/config/general.rs` — owned by step 1: `OptimizerConfig::idle_compaction: bool` (serde key `idle_compaction` in `[general.optimizer]`, default `true`, part of `is_default`). `src/config/settings_dto.rs` — owned by step 1: `SettingsSaveDto::optimizer_idle_compaction: Option<bool>` (absent = keep). Steps 2 and 3 only read these.
- 2026-11-10: `src/memory/mod.rs` — owned by step 1: `MemoryStoreTrait::cache_lifetime_secs(&self, model: &str) -> crate::error::Result<Option<u64>>`, `pub fn cache_lifetime_from_gaps(samples: &[(i64, f64)]) -> Option<u64>`, `pub const CACHE_LIFETIME_PROBES_SECS: [i64; 7]`. Step 2 calls `cache_lifetime_secs` through `AgentLoop::memory_handle()`.
- 2026-11-10: Savings ledger kind `"idle_compaction"` (tokens_before = context tokens before the idle compaction, tokens_after = after) — written by step 2 (src/runtime/agent.rs), labelled "Idle compaction" by step 3 (mnemo2/src/lib/cost.ts). insights.rs values it as `LeverValue::Entry` through its default arm; no change there.

+++
title = "Mnemo2 Models page owns only [models.summarize]"
created = "2026-11-02"
+++

Mnemo2's Models settings page edits exactly one `[models]` slot: the compaction model (`[models.summarize]`), rendered as the "Compaction model" section (section aria-label "Compaction", input label "Compaction model"). Mnemo1's per-task slots (planning, executing, bug_fixing, reviewing) are never read or written by Mnemo2 — they stay in the shared config.toml for Mnemo1, and the page carries one quiet note saying so.

The guarantee is structural, not only UI: `src-tauri/src/mnemo2/model_settings.rs` defines `TaskModelsWire`/`TaskModelsPatch` with only `summarize`, so a save — even a stale client payload carrying the old five-key shape — cannot clear `planning|executing|bug_fixing|reviewing`, and `complete`, `free`, `subagent` plus the per-skill table are untouched. The frontend mirror is `TaskModels = { summarize: string | null }` in `mnemo2/src/lib/modelSettings.ts`; `TASK_SLOTS`/`TaskSlot` no longer exist.

Regression tests: `save_compaction_leaves_every_other_model_slot_alone` (fixtures every `[models.*]` key, saves and clears summarize, asserts the whole slot set, and rejects the stale five-key payload — verified red against the old patch struct) and `save_compaction_preserves_reasoning_effort_and_validates`.

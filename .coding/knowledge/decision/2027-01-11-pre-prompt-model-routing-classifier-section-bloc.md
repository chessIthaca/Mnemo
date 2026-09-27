+++
title = "pre-prompt model routing — Classifier-section block + [general.routing] table, shadow-first"
created = "2027-01-11"
+++

Pre-prompt model routing (backlog 091e694d, item 2 of the Laya chain) — config decided with the user BEFORE implementation:
- Dialog placement: the routing block lives INSIDE Settings → Classifier (user's explicit choice), as a 6th block under the five Laya opt-ins — not a new section, not the Models grid.
- Config derived from the dialog: `[general.laya] routing = true` (flat opt-in flag, off by default, skip-serialized when false, like auto_type_memories/failure_triage) + a `[general.routing]` table { cheap, capable (ModelRef: endpoint/model/reasoning_effort), threshold (default 0.80), enforce (default false) } carried as `RoutingConfig` on `GeneralSection`.
- DTO: `laya_routing: Option<bool>` + `routing: Option<RoutingConfigDto>` in src/config/settings_dto.rs; None = leave unchanged (mirrors the models slot semantics).
- Shadow-first ladder is CONFIG, not a second code change: off → shadow (enforce=false: classify + log every turn, model untouched) → enforce (trivial → cheap, architectural → capable). The UI greys enforce until the classifier status is "ready".
- Invariants: below-threshold / None answer / unknown label / unset-or-dangling target → today's model exactly; skill, subagent and bug-fixing pins win (resolve_routed returns None for them); subagents, reviewers and compaction-summary calls never routed; ONE classification per user turn (not per iteration); every routing-mode turn logged (decision + outcome rows) to ~/.mnemo/laya/training/routing.jsonl.
- Rationale: base Laya checkpoints are near-chance zero-shot on custom tasks, so ship plumbing + logging first and enforce only after fine-tune + threshold calibration; the log corpus is task text → label/confidence → model → outcome.
Files: src/config/general.rs, src/config/settings_dto.rs, src/agent/model_routing.rs (new), src/model_resolver.rs, src/agent/turn.rs, src/agent/loop_impl.rs, src/agent/factory.rs, frontend/src/components/settings/sections/ClassifierSection.tsx.

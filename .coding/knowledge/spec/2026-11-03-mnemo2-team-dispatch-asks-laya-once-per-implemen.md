+++
title = "Mnemo2 team_dispatch asks Laya once per implementation dispatch, per routing mode"
created = "2026-11-03"
+++

src-tauri/src/mnemo2/dispatch_routing.rs plus team.rs: modes off, shadow, suggest and auto live in the global <config_dir>/mnemo2.toml [routing] (default off).
- What is classified: the step text plus the brief, using the shared kernel question. Classification runs BEFORE merge_lock, with a 3 s timeout covering the permit wait and the call. None or a timeout counts as offline.
- How the tier is chosen: an explicit tier or model wins, and the ran tier comes from the model. Only auto uses fallback_tier, falling back to the default model when that tier is empty. Suggest refuses only tier auto. The tier-required refusal applies to the lead tool only, so IPC keeps its default model.
- Logging: route and override rows go to ~/.mnemo/laya/training/dispatch_routing.jsonl, only after successful provisioning and stamped with the real worker id. WorkerRouting appears in team_status (omitted when None). The dispatch result carries one truth-table line.
- Off mode is byte-identical. Audits are never classified. Mnemo2 sessions skip per-turn routing.

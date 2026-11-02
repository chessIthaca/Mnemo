+++
title = "Mnemo2's built-in lead never uses Mnemo1's per-state models"
created = "2026-11-02"
+++

Mnemo2's built-in lead runs the workflow policy in TeamLead mode, whose model route is ["lead"]; ModelsConfig has no lead slot, so Mnemo1's [models.planning|executing|bug_fixing|complete] in the shared ~/.mnemo/config.toml never choose its model in any workflow state — it stays on the picker's choice or the default model. Pinned end to end by runtime test lead_ignores_the_mnemo1_per_state_models_across_a_state_change (src-tauri/src/mnemo2/runtime.rs). [models.summarize] still applies to context compaction.

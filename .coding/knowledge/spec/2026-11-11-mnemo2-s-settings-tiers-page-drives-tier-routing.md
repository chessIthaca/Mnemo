+++
title = "Mnemo2's Settings > Tiers page drives tier routing"
supersedes = "2026-11-04-one-settings-tiers-page-drives-tier-routing-for"
created = "2026-11-11"
evidence = "Commit 8651bd5c (merged dc3dd050); test the_top_control_writes_only_the_dispatch_mode in laya_settings.rs."
+++

Mnemo2's Settings > Tiers page is the only place for models per tier and for who picks a worker's tier.

- Its top control ("Sets how Mnemo2 picks a worker's tier") saves only Mnemo2's dispatch routing mode (off / shadow / suggest / auto) to <config_dir>/mnemo2.toml [routing]. Since 2026-11-11 it never writes [general.laya] routing or [general.routing] in config.toml; those are kernel settings for embedders' per-turn routing, which Mnemo2 sessions skip.
- The Laya options listed there are auto_type_memories, failure_triage, failure_triage_knn and steer_tool_choice; Reflex and Auto fine-tune are gone.
- Code: src-tauri/src/mnemo2/laya_settings.rs (tiers_top_control_writes, save_tiers_top_control_core); mnemo2/src/components/TiersPage.svelte.

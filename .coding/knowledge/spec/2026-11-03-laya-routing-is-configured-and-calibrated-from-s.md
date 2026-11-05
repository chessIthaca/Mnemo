+++
title = "Laya routing is configured and calibrated from Settings > Decisions; the eval writes route_eval.json"
created = "2026-11-03"
+++

Mnemo2 Settings > Decisions (LayaPage) holds the Mnemo2 [routing] block from <config_dir>/mnemo2.toml:
- mode (suggest needs at least 30 shadow decisions and auto at least 30 outcome-labelled decisions in dispatch_routing.jsonl, with a 'force' option);
- min_confidence_3 and min_confidence_2 with the eval date;
- fallback tier, nudges, per-tier effort and a log summary.
Saving reloads the open team immediately (TeamRuntime::reload_routing_settings). 'Run eval' (the run_routing_eval command, using the lead's attached Laya) and the CLI mnemo2_route_eval --laya-url URL share src-tauri/src/mnemo2/route_eval.rs. They write <training_dir>/route_eval.json and never apply thresholds themselves ('Apply' does). Mnemo1's ClassifierSection has a `routine` model picker and a first-prompt gate (shown as the calibrated default when unset). Mission control worker cards show Laya's pick and nudge chips.

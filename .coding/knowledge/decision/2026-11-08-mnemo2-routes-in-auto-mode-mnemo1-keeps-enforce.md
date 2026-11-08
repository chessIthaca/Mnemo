+++
title = "Mnemo2 routes in auto mode; Mnemo1 keeps enforce off"
created = "2026-11-08"
+++

The user chose this on 2026-11-08 after seeing the numbers: on real merged dispatches Laya was right 23% and too high 67% of the time, and the hand set stayed under the 80% bar at 4 labels. `mode = "auto"` in `~/.mnemo/mnemo2.toml`: an omitted or `auto` tier runs Laya's pick if it clears the gate, else `fallback_tier` (routine here); an explicit tier wins. Mnemo1 has no `enforce` line in `config.toml`, so it defaults to false and only shows "Laya would pick". Settings > Tiers no longer writes or reads Mnemo1's `enforce` (`tiers_top_control_writes` in src-tauri/src/mnemo2/laya_settings.rs), so this combination shows no warning and only Mnemo1's own Classifier settings can change it.

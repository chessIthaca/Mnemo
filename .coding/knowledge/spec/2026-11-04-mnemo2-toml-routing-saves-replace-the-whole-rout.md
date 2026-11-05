+++
title = "mnemo2.toml routing saves replace the whole [routing] table, dropping unknown keys"
created = "2026-11-04"
+++

`save_routing_settings` in src-tauri/src/mnemo2/dispatch_routing.rs replaces the ENTIRE `[routing]` table of ~/.mnemo/mnemo2.toml with the serialization of `RoutingSettings` (`table.insert("routing", routing)`). Any key the struct no longer declares is therefore silently deleted on the next routing save. Concretely: `RoutingSettings` lost its `effort` field when per-tier reasoning effort moved onto the tier-ladder rungs, so the `[routing.effort]` table found on older installs is vestigial and disappears when routing is next saved — it is not read by anything, and code must not assume routing saves preserve it.

Practical consequence for anyone writing settings back: to preserve unknown keys, do the read-modify-write on the raw `toml::Value` table rather than round-tripping through `RoutingSettings` (read file → parse → replace only the `routing` key → serialize the whole document → write). `load_routing_settings` does exactly this for its retired-tier-id write-back, via `save_converted_routing_table`.

Also: retired tier ids (simple/average/difficult → trivial/routine/high) are converted by `TierLadder::convert_old_id` (src/config/endpoints.rs); conversion belongs in the loader, and a converted load should write the file back once so stored ids and the app cannot disagree, with no write at all when nothing converted.

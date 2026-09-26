+++
title = "LayaConfig::is_default omitted steer_tool_choice — the flag was silently dropped from config.toml"
created = "2027-01-11"
+++

SYMPTOM: with only `steer_tool_choice = true` set in `[general.laya]`, the flag vanished from config.toml on the next save (and from the running config after a restart). ROOT CAUSE: `LayaConfig::is_default()` (src/config/general.rs) gated the whole `[general.laya]` table's `skip_serializing_if`, and it listed every flag EXCEPT `steer_tool_choice` — so a config whose only non-default was that flag serialized to nothing. Introduced when the tool-choice steering flag landed (commit 7b7691b). FIX (found and fixed during plan 2f74e10a, 2027-02, wt/mnemo): added `&& !self.steer_tool_choice` to `is_default()`. REGRESSION TEST: `config::general::tests::laya_steer_tool_choice_counts_as_touched` (asserts `!is_default()`, that the serialized table carries the key, and that it round-trips) — red before the fix, green after.

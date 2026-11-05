+++
title = "External model swaps must clear only Laya's routed pin and record manual once"
created = "2026-11-03"
+++

Root cause K1: global IPC/console/headless swaps used plain `AgentLoop::set_provider`, which left Laya's explicit routed pin serving and did not record a manual choice; per-surface reroute calls could not cover every path. Kernel contract now: public `set_provider` / `set_explicit_provider` call `note_external_model_change(new_spec)` BEFORE installing the external provider. Active routing + changed spec records `(from,nudged)` once, and every external change clears only `routed_pin` (human pins keep their legacy semantics). Same-spec/effort-only changes are not manual. Laya commits use `pin_routed_provider`; deferred swaps use `set_provider_internal`; 429 fallback, per-iteration resolution and summarize paths never call external hooks. IPC-specific reroute/selector helpers were removed. Regression tests: global set_provider, real Engine::swap_model during gated classification, same-spec, and internal 429. Fixed in commit `83b5ca51fa11388d33c31e70fb0812b398799bd6`.

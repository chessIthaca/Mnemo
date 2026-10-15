+++
title = "compaction trigger is fill-rate-only — proxy-cache ceiling removed (2027-01-25)"
created = "2027-01-11"
+++

User decision (2027-01-25, plan 842c1a70, branch wt/mnemo, backlog 0324749d): the compaction trigger = summarize_at_fill_rate × window ONLY. The ceiling cap min(product, ceiling−32_768) and the proxy_cache_ceiling_tokens dial are removed end-to-end (src/agent/context.rs effective_summarize_threshold drops the ceiling param; general.rs ContextConfig, settings_dto.rs patch/validation, src-tauri settings.rs GetSettingsContext, Settings→Advanced UI control, PLAN.md item 7). A LiteLLM user wanting the old behavior sets a lower fill rate (0.3 on 1M window = 300K, under the cliff). SCOPE BOUND (user chose "Keep consts (retention untouched)" via ask_user): the wire-side DeepSeek reasoning-retention strip is untouched — PROXY_CACHE_CEILING_TOKENS + PROXY_CACHE_PRESSURE_MARGIN_TOKENS stay in src/provider/mod.rs serving src/provider/openai/request.rs:461-462/:511-512 (pressure check for reasoning-content stripping); only their doc is reworded to name the single remaining consumer. CompactionRaise ladder (context.rs:151-202) untouched by the change.

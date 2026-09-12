+++
title = "GLM stop boundaries config-driven via stop_boundary_strings (no model-name prefix matching)"
created = "2027-01-05"
+++

DECISION: GLM stop boundaries are config-driven via `stop_boundary_strings` — the hardcoded `starts_with("glm-5.3")` prefix check is gone (backlog f322277c, shipped 2027-01-05).

- **One mechanism, one place:** `stop_boundary_strings: Vec<String>` on `Endpoint`/`ModelSpec` (src/config/endpoints.rs), resolved per model via `stop_boundary_strings_for(model_id)` — model-level overrides endpoint-level, the exact same pattern as `stop`/`stop_token_ids`. The provider (src/provider/openai.rs) reads the resolved config: request `stop` list = configured boundaries + user `stop` (deduped, boundaries first); `stop_token_ids` = plain config pass-through; the SSE stream guard = user `stop` + configured boundaries. Both duplicated GLM const blocks and the `is_glm_53` branch are deleted.
- **No name-prefix matching:** resolution is by exact model-id match (`model_spec`) — an alias, fine-tune, or proxy rename with `stop_boundary_strings` configured gets identical protection (regression tests: `build_request_json_sends_boundaries_for_aliased_model`, `stream_guard_protects_aliased_model_via_config_only`). The old case-insensitive prefix match is gone entirely.
- **Guard now sees configured cascades (deliberate):** the stream guard previously got only the 4 role tags (not the `\n\n\n\n`/`\n\n\n` cascades); it now reads the full configured list — cutting GLM's runaway-empty-lines defect at the first cascade. User-tunable: remove the cascades from config to restore old guard behavior.
- **Save path:** patch.rs `apply_endpoints` carries `stop_boundary_strings` over from the original endpoint when the UI-sent one is empty (endpoint + model level) — hand-edited values survive Endpoints-tab saves (the 2026-12-24 review lesson).
- **Shipped values:** the GLM-5.3 example (4 role tags + 2 cascades; token ids [151329, 151330, 151336]) lives in the endpoints.rs test fixtures (the "endpoints.toml examples" — no repo-shipped endpoints.toml exists) + README's models paragraph. Users configure it in their own `~/.mnemo/endpoints.toml`.
- **Escapes lesson (unchanged):** tag literals in builder + tests are built from `\u{3c}`/`\u{3e}` escapes, never raw angle-bracket text (transport stripping, review round-1 of the original fix).
- Docs: PLAN.md provider-quirk item 8 rewritten to the config-driven mechanism; README.md models paragraph documents the field.

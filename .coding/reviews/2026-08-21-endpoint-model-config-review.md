# Review — Snapshot fallback, UI gray fixes, forgiving base_url + per-model endpoint config (plan 109af963)

Reviewed ALL uncommitted changes on `feat/endpoint-model-config` (30 files, ~1172 insertions) against the plan's 7 steps. Scope: ModelSpec deserializer/merge, effort clamping, the read-only webview fallback, the two UI fixes, base_url normalization, the wire layer, provider caps, docs, and the constitution (doc comments, no `#[allow]`, no dead code, regression tests, multi-platform neutrality).

## Summary

The implementation is solid and well-tested. Back-compat is handled correctly on both sides of the wire, the read-only fallback is properly isolated from the mutation path (mutations still go through child-only `webview_page()`), the CSS/grip fixes are disciplined (longhand `background-color`, `group` on all four handle hosts, with contract tests), and the model-scoped effort/caps plumbing is threaded through every build site (`main.rs`, `console.rs`, `memory_maintenance.rs`, `settings.rs`, `client_factory::build_provider_for`). No `unwrap()` on `model_spec` in production code (only tests), no new `#[allow(...)]`, no Windows-only APIs outside the sanctioned `game_*`/WebView2 gate.

Findings below, by severity.

---

## Medium

### M1. Effort clamp can send `reasoning_effort: "off"` as a literal request field (should omit the field)

The plan's contract is: `"off"` is always allowed **and always means "omit the field"**. Both clamp sites honor `Some("off") → None` for the *requested/configured* value, but the clamp branch (when the value isn't in the model's allow-list) can *produce* `"off"` as a literal `Some("off")`, which the OpenAI client then puts in the request body verbatim (`body["reasoning_effort"] = "off"`). Providers don't list `"off"` as a valid effort value (cf. the self-healing fallback's sample rejection: "Supported values: none, minimal, low, medium, high, xhigh").

Trigger: a model whose `reasoning_efforts = ["off"]` (a legal config — the UI's `ModelRowConfig` accepts any comma-separated values, and the endpoint-level `reasoning_effort = "max"`/`"high"` is outside that list).

- `src/config/endpoints.rs:238-243` — `effective_reasoning_effort_for`: `Some(list[0].clone())` → returns `Some("off")`.
- `src-tauri/src/ipc/config_io.rs:95-98` — `resolve_reasoning_effort`: `Some(list[0].clone())` → same.

Impact is softened by the client's self-healing retry (a 400 naming `reasoning_effort` is retried without the field — one wasted round-trip per request), but it is still a malformed request on a legitimate config.

Fix: in both clamp branches, map the clamped value: `match list[0].as_str() { "off" => None, v => Some(v.to_string()) }`. Add a regression test (endpoints.rs + config_io.rs) with `reasoning_efforts = ["off"]` + configured `"max"` asserting `None`.

## Low

### 2. Open-but-blank child webview is misclassified as "no child" and reads silently target the app page

`is_app_url` treats `about:blank` as an app URL (`src/browser/mod.rs:873`), so `select_child_target` skips it. During the child webview's initial creation/navigation (and any moment its URL is `about:blank`), `webview_page_for_read` (lines 785-808) finds no child and silently falls back to the app page. Before this PR the same situation would have errored with "is the Browser tab open?" — now the agent gets a screenshot/snapshot of the app UI labeled as the tab. The child is transiently `about:blank` at creation, so this is a small race; the doc comment at lines 857-858 claims "In production the child is never `about:blank`", which is only true after the initial navigation lands.

Suggest: in `webview_page_for_read`, prefer a page whose URL is `about:blank` when it is the only non-app-page target (or extend `is_app_url` handling so `about:blank` matches the app only when no other page exists), and/or note the transient in the doc comment. Low impact — the fallback is still read-only.

### 3. StatusBar effort label can desync from the backend-clamped value on model switch

`selectModel` (`frontend/src/components/layout/StatusBar.tsx:344-360`) sends the current toolbar effort verbatim; when the target model's allow-list excludes it, the backend clamps (config_io `resolve_reasoning_effort`) but `setModel` returns `void`, so the store label is set to the *unclamped* value (line 359 `setReasoningEffortStore(effort)`). E.g. switching from gpt-4o with "max" to a model whose list is `["medium","low"]` leaves the label "max" while requests carry "medium". The dropdown's `effortOptionsFor` (lines 36-45) is correctly model-scoped, so the *options* are right — only the *selected label* can drift until the next re-sync.

Suggest: clamp client-side before the `setModel` call using the target model's `model_configs` list (same rule as the backend), or have `set_model` return the effective effort for the label.

### 4. Whitespace-padded model ids in `model_configs` are dropped instead of trimmed-matched

`validate_endpoint` (`src/config/patch.rs:80-87`) filters configs against `model_ids` (already trimmed) **before** trimming the config id: a config with `id = " gpt-4o"` is dropped as "unknown" even though its trimmed id would match. Harmless in the UI flow (ids originate from the same trimmed rows), but the trim at line 84 is then dead code for the matching purpose. Suggest trimming in the filter (compare `spec.id.trim()`).

### 5. Doc comments slightly misstate the merge/wire semantics

- `src-tauri/src/ipc/settings.rs:62-64` — `EndpointDto.model_configs` "must be 1:1 with `models` by position"; the merge (patch.rs) actually matches **by id** and drops unknown ids (more forgiving than documented; the frontend's `types.ts` comment gets it right).
- `src-tauri/src/ipc/settings.rs:339-341` — `EndpointWire.model_configs` is documented "Empty when no model has per-model config", but `endpoint_wire` maps every `ModelSpec` (line 602), so whenever `models` is non-empty the array is non-empty (bare entries `{id, max_context: null, ...}`). Harmless on the wire (frontend tolerates), but the doc should say "always mirrors `models`".

Both are comment-level nits; no behavioral fix needed (or reword the comments).

---

## Verified clean (spot-checked per the plan's checklist)

- **ModelSpec deserializer**: untagged `Id(String) | ModelSpec` handles both forms; legacy string arrays round-trip (`legacy_string_models_still_parse`, `parses_per_model_table_form`); order preserved + unknown ids dropped (`validate_endpoint_merges_per_model_configs`, `upsertModelConfig` frontend test); empty model lists never panic (`model_spec` returns `Option`, all `unwrap()`s are test-only; `per_model_caps_override_endpoint_level` covers the empty-models case).
- **Read-only fallback security**: all mutations (`webview_eval`, `webview_click`, `webview_type`, `webview_navigate`) still route through child-only `webview_page()`; the fallback test asserts `webview_page()` refuses the app page with the "is the Browser tab open?" error. No mutation path can reach the app page.
- **Scrollbar corner**: `background-color: var(--bg-secondary)` longhand only (globals.css:119-121), pinned by scrollbar.test.ts (incl. the "no shorthand" contract).
- **Grip pills**: `bg-slate-500 group-hover:bg-slate-400` on all four hosts (App.tsx:690/699, InflightBar.tsx:139/146, FileViewer.tsx:371/373, GraphView.tsx:776/778), each host carries `group`; InflightBar.test.ts pins both the colors and the `group` host.
- **base_url**: trim + trailing-`/` append in patch.rs:48-59; tests in patch.rs (two) + settings.rs (`normalizes_missing_trailing_slash`).
- **Wire back-compat**: `EndpointDto.model_configs` is `#[serde(default, skip_serializing_if = "Vec::is_empty")]`; legacy payload without the key still saves (`supports_reasoning_effort_defaults_true_when_key_absent`); legacy `endpoints.toml` string arrays parse.
- **Provider caps**: `openai_client_config`/`anthropic_client_config` use `max_context_for`/`max_output_tokens_for`; all effort resolution sites are model-scoped (`effective_reasoning_effort_for(Some(&model))` in main.rs:907, console.rs:503, memory_maintenance.rs:231, settings.rs:241, client_factory.rs:196).
- **Docs**: README.md + PLAN.md updated (per-model tables + forgiving base_url, incl. the `[[endpoint.models]]` syntax); module doc comments updated (endpoints.rs `ModelSpec` doc block, patch.rs `validate_endpoint` doc).
- **Constitution**: public items have doc comments; no `#[allow(...)]` added; no dead code (warning-free build contract); regression tests for every defect (scrollbar corner, grip visibility, base_url normalization, per-model caps/efforts).
- **Multi-platform**: all new Rust is in lib code using std-only APIs; the webview fallback lives in the sanctioned Windows-only `game_*`/WebView2 gate; frontend CSS/TS is platform-neutral.
- **Security**: no secrets on the wire (`get_settings`/`EndpointWire` carry no keys; keys stay in the separate `api_keys` map); fallback is read-only.

**Conclusion**: 1 Medium + 4 Low findings, all with concrete fixes. Everything else verified clean.

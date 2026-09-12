## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoder` for plan d5d43154 "Per-model vision (multimodal) option" (backlog a634835c). The per-model resolution is correct at every conversion site, back-compat holds in all three legacy dimensions (string-form models, old Settings payloads, existing endpoints.toml), and the "vision-capable active model never falls back" claim is structurally true. One low finding: the per-model vision checkbox tooltip is imprecise when an explicit per-model `false` was loaded from `endpoints.toml`. No high findings.

## Finding L1 (low) — vision checkbox tooltip slightly dishonest for explicit `false`

`frontend/src/components/settings/sections/EndpointCard.tsx:880` — the per-model vision checkbox title reads "Unchecked = inherit the endpoint's Multimodal flag." But an explicit `multimodal = false` loaded from `endpoints.toml` also renders unchecked (`checked={config.multimodal === true}`), and in that state unchecked means "explicitly off", not "inherit". Related asymmetry: toggling the box off sends `null` (inherit), never `false` — so at a multimodal=true endpoint, a user who checks then unchecks the box expecting "off" silently gets "on" (inherit). The code comment (":874-877") correctly documents the deliberate scope choice ("An explicit false is settable in endpoints.toml only") and the data itself is safe: a pure load→save round trip preserves `false` (`ProvidersSection.tsx:66` `mc.multimodal ?? null` keeps it; `upsertModelConfig`'s spread keeps it unless the vision checkbox itself is toggled). Suggested fix (wording only, one string): e.g. "Send image blocks to this model even when the endpoint default is text-only. Unchecked = inherit the endpoint's Multimodal flag (an explicit off, set in endpoints.toml, also shows unchecked)." Behavior need not change.

## Verification detail (what was checked and confirmed correct)

### 1. Per-model resolution — correct
`Endpoint::multimodal_for` (src/config/endpoints.rs:221-226) resolves `model_spec(id).and_then(|m| m.multimodal).unwrap_or(self.multimodal)` — exact mirror of the existing `max_context_for`/`max_output_tokens_for` pattern. `ModelSpec.multimodal: Option<bool>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (endpoints.rs:131) — absent key parses as None, None does not serialize → no churn on existing endpoints.toml re-saves.

### 2. Every client-construction site resolves per model — exhaustive enumeration
All callers of `build_client` / `build_provider_for` / `resolve_model_provider`:
- lib: `build_provider_for` (client_factory.rs:196 → build_client, resolves via multimodal_for); `context_manager_for_model` (:220 → build_provider_for); `model_resolver.rs:271` (turn-override fallback chain → build_provider_for).
- src-tauri: main.rs:1083 (main provider); config_io.rs:159 (`resolve_model_provider` — which serves console.rs:979 AND ipc/agent.rs:565 `set_model`, the runtime swap the plan flagged as "verify"); memory_maintenance.rs:276; settings.rs:251 (save re-sync).
- `build_vision_client` (client_factory.rs:245-247) passes literal `true` — intentionally unchanged, correct for a vision-specific client.
- Remaining `.multimodal` field reads are all intentional per the brief: provider-capabilities plumbing (loop_impl.rs:1377 `is_multimodal` reads caps; runtime/agent.rs:509 fallback gate; provider/mod.rs:141; openai.rs:148 / anthropic.rs:161 client-config structs), and endpoint-level DTO/wire mapping in settings.rs (:136 into validate_endpoint, :630 wire emission).
- No unconverted `ep.multimodal` read feeding a specific-model client build exists — verified by a full-tree `\.multimodal\b` sweep plus the call-site enumeration above.

### 3. No-fallback-when-vision-capable — falls out as claimed
`is_multimodal()` (loop_impl.rs:1372-1377) reads the built provider's capabilities; the runtime image-parsing fallback gate (runtime/agent.rs:509) keys on it. Since capabilities now derive from `multimodal_for(model)`, a vision-capable active model never falls back to general.vision_model — no loop_impl change needed, as the plan assumed.

### 4. Back-compat — all three legacy dimensions covered
- String-form models: untagged `deserialize_models` (endpoints.rs:158-176) wraps strings via `From<&str>` → `multimodal: None` (:143).
- Old Settings payloads: `ModelSpecDto.multimodal` has `#[serde(default)]`; test `per_model_multimodal_absent_key_defaults_none` (settings.rs:1504-1522) parses a raw payload without the key.
- Existing endpoints.toml: serde default + skip_serializing_if; TOML round-trip test `per_model_multimodal_round_trips_through_toml`.
- FE: `multimodal?: boolean | null` optional everywhere; `?? null` at both DTO→editable and import mappings.

### 5. Conversion/serde fidelity
`into_endpoint` (settings.rs:117-127) maps `multimodal: d.multimodal` and routes through `validate_endpoint` — the DTO round-trip test (settings.rs:1466-1502) therefore also proves validate_endpoint preserves the field (asserts on the resulting `Endpoint`'s `model_spec`, both save and GET directions, plus `multimodal_for` resolution and endpoint-default separation). FE save direction: `EndpointEditable.model_configs` (tauri.ts:1097) flows wholesale to save_endpoints — no reconstruct-without-multimodal site exists; `upsertModelConfig` spread-merge carries it; `importEndpointEditable` (types.ts:336) maps it.

### 6. Docs sync (project check 1) — pass
- README.md:57 feature bullet updated ("per endpoint, or per model — one endpoint can mix text-only and vision-capable models"); :67 image-parsing bullet still accurate.
- PLAN.md:793-794 rewritten with per-model resolution wording.
- The stale `src/tool/agent/describe_image.rs` path fixed to `src/tool/agent/image_tools/` — verified that directory exists (mod.rs/tools.rs/zoom.rs); the old path was indeed stale.
- general.rs VisionModel docs, endpoints.rs ModelSpec docs, client_factory docs all updated. No endpoints.toml example in README mentions multimodal (only :57/:67 mention vision — both current), so nothing stale left.

### 7. Multi-platform neutrality (project check 2) — pass
Pure config/DTO/UI change; zero platform-specific code, paths, or shell syntax.

### 8. Tests and style
New tests at every layer: 3 in endpoints.rs (override wins / Some(false) forces off at a multimodal endpoint / TOML round-trip), 1 in client_factory.rs asserting via `LlmClient::capabilities()` for both the override and inherit models, 2 DTO tests in settings.rs, 2 FE tests in types.test.ts. Style mirrors house patterns; all new public items have doc comments. Reported suites green (cargo 1722/0 under `#![deny(warnings)]`, src-tauri build clean, vitest 703/703, tsc clean); as a read-only reviewer I could not re-run the suites myself — assessed via direct code reading, and the test code is sound and regression-capable.

### 9. Bookkeeping rider
backlog.jsonl changes are edits/additions only (a634835c → in_flight + reworded text, 8b8f40d2 failed→pending, 69cf7e9c → done) — safe under the union merge driver (only deletions have the resurrection issue). Untracked plan file + web_fetch knowledge file are appropriate to include in the commit.

## Verdict: FINDINGS (0 high, 1 low)

The per-model reasoning-effort feature is correctly implemented end-to-end: the resolver precedence (model → endpoint → "max"), the DTO/wire round-trip, the frontend load/save/import paths, and the UI gating all check out, and no path silently erases the new field. One LOW documentation-sync finding in `src/provider/client_factory.rs` (stale references to the model-less `effective_reasoning_effort()` that this change makes actively misleading).

### Scope
- Full `git diff HEAD` + untracked files on `wt/agenticcoding` (HEAD = 58849c7): `src/config/endpoints.rs`, `src-tauri/src/ipc/settings.rs`, `src/config/patch.rs`, `src-tauri/src/ipc/config_io.rs`, `src/provider/openai/tests.rs`, `frontend/src/lib/tauri.ts`, `frontend/src/components/settings/types.ts` + `types.test.ts`, `ProvidersSection.tsx`, `EndpointCard.tsx`, `README.md`, `PLAN.md`, plus `.coding/` bookkeeping.
- Cross-checked every consumer of `effective_reasoning_effort_for`, the save pipeline (FE → DTO → `validate_endpoint` → `apply_endpoints`), and the toolbar dropdown's initial selection.

### Findings

**F1 (LOW, documentation sync) — stale `effective_reasoning_effort()` references in `client_factory.rs` doc comments.**
- `src/provider/client_factory.rs:72-73` (`openai_client_config` doc): "`reasoning_effort` is passed through (use `endpoint.effective_reasoning_effort()` for the default)" — following that advice would now skip the per-model override; every actual caller (client_factory.rs:204, main.rs:1074, config_io.rs:117, settings.rs:259, memory_maintenance.rs:274, console.rs:536) uses `effective_reasoning_effort_for(Some(model))`.
- `src/provider/client_factory.rs:186` (`build_provider_for` doc): "+ `effective_reasoning_effort()` are used" — but line 204 calls `effective_reasoning_effort_for(Some(&model.model))`.

Pre-existing text (this diff doesn't touch the file), but before this change the two variants differed only in allow-list clamping; they now differ semantically (per-model override resolution), so the references understate what the code does and would mislead a reader into thinking the per-model default does not flow through `build_provider_for`. Fix: update both to name `effective_reasoning_effort_for(Some(model))`. Doc-only, no behavior impact.

### Verified correct (detail)

**1. Resolver (`src/config/endpoints.rs`)**
- `ModelSpec.reasoning_effort: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` + doc comment; `Default` impl entry — old TOML files load as `None`, saves omit the key (no disk noise).
- `effective_reasoning_effort_for`: `spec` computed once and reused for both the allow-list and the value (`Option<&ModelSpec>` is `Copy` — compiles clean); model-level wins → endpoint-level → "max"; the off-encoding (DeepSeek "off"→"none", others omitted) and allow-list clamping arms are unchanged. The doc-comment change ("clamped to the list's first entry") corrects previously inaccurate text — the code has always clamped to `list[0]`, not "max".
- `effective_reasoning_effort()` (model-less) delegates to `_for(None)` → endpoint-level only — its semantics are unchanged, correct.
- 3 new tests match the code: model beats endpoint; per-model "off" encodes per provider; model-level value clamps into the model's own list. Ghost-model fallback (config for an unknown id) covered.

**2. Consumers — the per-model default actually flows.** All six call sites pass `Some(model_id)`: client_factory.rs:204 (per-context model overrides), main.rs:1074 (startup brain build), config_io.rs:117 (`set_model`/model swap), settings.rs:259 (post-save live re-sync), memory_maintenance.rs:274, console.rs:536 (toolbar initial selection). Design decision 3 verified: the status-bar dropdown is untouched and its initial selection now reflects the per-model default automatically (and the clamped value is always a member of the dropdown's option list, so no invalid selection).

**3. DTO/wire round-trip (`src-tauri/src/ipc/settings.rs`).** `ModelSpecDto.reasoning_effort` with `#[serde(default)]` (absent key → `None` — back-compat with older FE payloads; covered by the extended absent-key test); `into_endpoint` maps it; `ModelSpecWire` is null-emitting (no `skip_serializing_if` — consistent with its other Options; the asymmetry vs the TOML-facing `ModelSpec` is intentional and correct); `from_spec` maps it. Round-trip test covers both directions.

**4. Save path — no silent erase (the 2026-12-24 lesson).** FE save sends the full `EndpointEditable[]` → `EndpointDto` → `into_endpoint` → `validate_endpoint` (merges `model_configs` onto `models` by id, preserving spec fields — asserted by the extended `validate_endpoint_merges_per_model_configs`) → `apply_endpoints` (carry-over only for non-DTO fields: temperature/top_p/stop/stop_token_ids/stop_boundary_strings/extra_body). `reasoning_effort` rides the DTO, exactly like endpoint-level `reasoning_effort` — design decision 2 verified, and the round-trip + unset-clears tests prove it. The frontend populates the field on every path: load mapping (`mc.reasoning_effort ?? null`), `modelConfigFor` bare fallback, `upsertModelConfig` spread-merge, `importEndpointEditable` (2026-08-22 M1 lesson — import doesn't drop per-model config; covered by the new import test). Unset = null → `None` → inherit endpoint-level. Correct sentinel semantics.

**5. UI (`EndpointCard.tsx` ModelRowConfig).** The effort select uses the existing `effortToSelectValue`/`effortFromSelectValue` helpers (null/undefined/"" → the `REASONING_EFFORT_DEFAULT` sentinel → "endpoint default" option; sentinel → null on change). `disabled` gates on `!supports_reasoning_effort` (same prop as the efforts input, with matching title). Hidden for `kind === "anthropic"` — mirrors the endpoint-level Effort control (line 434) exactly as planned; `anthropic_client_config` doesn't consume reasoning_effort at all, so hiding is semantically right too.

**6. Test plumbing.** `ep()` in config_io tests builds `ModelSpec` models, so `e.models[0].reasoning_effort = Some(...)` + `FIRST_MODEL` line up. The openai test's `openai_client_config(&config, &ep, "glm", false, …)` matches the real signature (config, endpoint, model, multimodal, reasoning_effort) and mirrors the existing `build_request_json_includes_reasoning_effort_when_set` pattern — it exercises the full chain Endpoint → resolver → client config → request body.

**7. Docs.** README.md (~122) and PLAN.md (~403 config tree, ~936 feature) updated and accurate. No shipped `.toml` example in the repo contains `reasoning_efforts`/`reasoning_effort` (searched), so no example file is stale.

### Observations (non-blocking)
- For anthropic endpoints the per-model `efforts` allow-list input remains visible/enabled (pre-existing) while the new effort select is hidden — dead UI for anthropic since `anthropic_client_config` ignores effort entirely. Pre-existing asymmetry, outside this diff's scope; the new control's gating is correct per plan.
- The uncommitted `.coding/plans/152d8007.md` edit + untracked `.coding/knowledge/bug/152d8007.md` are bookkeeping leftovers from the prior web-fetch SSRF plan's closing sequence — not code, no action for this review.

### Constitution checks
- Doc comments present on all new public fields/functions (Rust + TS). ✓ (F1 concerns pre-existing comments made stale by this change.)
- Warning-free: pre-review `cargo test` green under `deny(warnings)` (lib 2003+16, tauri 187+4, FE tsc clean + 869 vitest tests). ✓
- Regression tests: comprehensive at every layer (resolver, DTO, patch round-trip, config_io, wire→request-body, FE helpers). ✓
- Multi-platform neutrality: pure config + React UI, no platform-specific code. ✓
- Security: no new attack surface — config-only field, UI input constrained to the fixed option list, same trust level as every other endpoints.toml field. ✓
- Existing code style followed throughout (serde attribute placement, wire-struct null-emitting convention, test naming). ✓

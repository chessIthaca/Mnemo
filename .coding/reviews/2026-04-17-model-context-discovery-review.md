# Review — Discover model context/output caps from /models (auto-fill + conflict warning)

Reviewed all uncommitted changes in the working tree (git status / git diff HEAD):
- `src/provider/openai.rs` — ModelWithVision enriched with `context_length`/`max_output_tokens`; lenient `model_reported_caps` extractor; dedupe merge ORs vision + max()s caps; ids-only `fetch_models` deleted; 6 new parse tests + all `ModelWithVision` literals updated.
- `src-tauri/src/ipc/models.rs` — `list_models` now returns `Result<Vec<ModelWithVision>, IpcError>` via `fetch_models_with_vision`; doc comment updated.
- `frontend/src/lib/tauri.ts` — `listModels` returns `VisionModelInfo[]` with optional `context_length`/`max_output_tokens` (null when unreported); `VisionModelInfo` doc updated.
- `frontend/src/components/settings/sections/EndpointCard.tsx` — `capsById`/`lastPicked`/`filledForRef` state, auto-fill effect, conflict warnings + Apply buttons, `applyServerModels` split helper.
- `.coding/plans/3fc1c367-…md` — step checkbox bookkeeping only.

## Verdict

The diff is correct and matches the plan's design. One low-severity frontend inconsistency (P3) and two nits; nothing blocking.

## Findings

### P3 — `testConnection` error path leaves stale discovered caps on screen (EndpointCard.tsx ~174-176)

`fetchServerModels`'s catch clears `capsById` (`setCapsById({})`, line 156) so a failed fetch can't leave stale hints, but `testConnection`'s catch (lines 174-176) does not. Sequence: (1) user successfully fetches models for URL A → caps populated; (2) user edits base_url to B; (3) clicks "Test connection" → it fails → `capsById` still holds A's caps, so the "Endpoint reports … context for …" hints and the "Caps detected from endpoint" note keep rendering under the now-B URL, and the conflict "Apply discovered" button would apply A's stale value (the effect does not re-fire, so nothing is auto-written — the stale data only affects display + a deliberate Apply click, and the field value itself remains visible above the hint).

Fix suggestion: `setCapsById({})` in `testConnection`'s catch (mirroring `fetchServerModels`). Optionally also `setModelsCacheKey("")` there so a subsequent picker fetch re-probes instead of trusting the stale ready cache — note `testConnection`'s success path already sets `modelsCacheKey`, so the error path leaving it stale is asymmetric with itself.

### Nit — hint copy (EndpointCard.tsx ~546)

"empty fields auto-fill on pick" is slightly imprecise: auto-fill fires as soon as a fetch succeeds and the reference model has caps (the fetch is triggered by opening the picker, but the fill itself is not tied to the pick). Cosmetic only.

### Nit — `entry.1 .context_length` spacing (openai.rs ~249-256)

`entry.1 .context_length` / `entry.1 .max_output_tokens` (space before the field access) is valid but un-rustfmt'd style; a single `entry.1.context_length` would read cleaner. Purely cosmetic — no `cargo fmt` gate exists in this repo.

## Verified-clean focus points

1. **Lenient parsing (openai.rs:315-347)** — `first_positive_u64` does `get → as_u64 → filter(>0)`: strings, floats, negatives, nulls, and zero all read as `None`, never an error (test `model_caps_malformed_values_are_ignored` covers all five). Key precedence is first-hit-wins per spec: top-level `context_length` → `max_context_length` → `max_model_len`, then nested `top_provider` fallback with the same key order; output = `max_completion_tokens` → `top_provider.max_completion_tokens`. `top_provider` being a non-object is safe (`get` on a non-object returns None). Tests cover Ollama/LM Studio/vLLM/OpenRouter-nested/precedence/vanilla-OpenAI-all-None.
2. **Dedupe merge (openai.rs:240-266)** — vision OR'd; caps `Option::max`'d per field (None.max(Some(x)) = Some(x), so a duplicate entry lacking caps can't zero out a reported cap). BTreeMap keeps sort order; blank ids still dropped. Correct.
3. **No dead code** — ids-only `fetch_models` fully removed; repo-wide search shows zero live callers (only historical mentions in `.coding/plans/` and `.coding/reviews/`). `ModelCaps`/`model_reported_caps` are used. No `#[allow(...)]` added anywhere in the diff (the only `#[allow]`s in src/ are pre-existing clippy allows in `src/agent/loop_impl.rs`, untouched by this plan). All public items (`ModelWithVision` + its two new fields, `list_models`, `fetch_models_with_vision`) have doc comments.
4. **All `ModelWithVision {` literals updated** — construction sites (openai.rs:368, :396), dedupe output (:260), and all test literals (:2005, :2046-2047, :2084-2085, :2101-2102, :2120-2121) carry the new fields.
5. **IPC (models.rs:40-74)** — `list_models` delegates to `fetch_models_with_vision` with `.map_err(IpcError::from)`; `From<myharness::Error>` maps variant → kind, Display → message. Error messages contain the URL and (for non-401/403) a truncated body; the 401/403 path suppresses the body entirely, and none of the changed code introduces key material into any error string. `list_vision_models` unchanged and still compiles against the enriched struct. Frontend `VisionModelInfo` (tauri.ts:251-258) matches the serialized shape (`Option<u64>` → `number | null`); its only other consumer (VisionSection.tsx:88-90) stores the full list and filters on `vision_capable`, so the superset shape breaks nothing.
6. **Contract fixtures** — no `list_models`/`list_vision_models` fixture exists in `src-tauri/src/ipc/contract_fixtures.rs` or `frontend/src/lib/ipc-fixtures/` (fixtures cover DTOs: dto-agent-info, event-user-question, dto-workflow-state-info, dto-backlog-changed-payload, dto-get-config, dto-get-settings, dto-save-endpoints, dto-save-settings), so the plan's conditional "update if an exact-shape fixture exists" was correctly a no-op.
7. **Frontend behavior (EndpointCard.tsx)** — auto-fill guards `endpoint.max_context == null` / `max_output_tokens == null` so an explicit value is never silently overwritten; `filledForRef` (model + caps guard key, set only when a patch is emitted) prevents refill loops after a user clears a field, and also survives the StrictMode double-effect and a fetch-error → refetch round-trip with identical caps. Conflict Apply buttons change exactly one field each. Reference model = `lastPicked ?? endpoint.models[0] ?? null`; hints render only when a reference model with caps exists; caps cleared on the main fetch error path. Hooks: all hooks unconditional, deps array complete for the effect's reads (`capsRefModel`, `refCaps`, both field values), `eslint-disable-next-line react-hooks/exhaustive-deps` present with a justification (filledForRef guard + stable `onEndpointChange` prop) — acceptable. No new dependencies; `fmtTokens` reused from `frontend/src/lib/format.ts:56`.
8. **TypeScript** — `npx tsc --noEmit`/`npm run build` were run by the main agent; by inspection the types line up (`listModels → Promise<VisionModelInfo[]>`, both EndpointCard call sites go through `applyServerModels`, `VisionModelInfo` fields optional/nullable, `DiscoveredCaps` fully used, no unused imports).
9. **Security/constitution** — no unsafe Rust, no secrets in new error strings or logs, no changes outside the plan's scope (5 files: 4 code + the plan file's own bookkeeping). The `"list_models"` string at `src-tauri/src/ipc/spawn.rs:191` is the reviewer role's agent-tool name, unrelated to the IPC command.

# Review: config bugs — forgiving base URLs on load + per-model caps auto-fill

Branch: `fix/config-forgiving-per-model-caps`
Reviewed files: `src/config/endpoints.rs`, `frontend/src/components/settings/types.ts`, `frontend/src/components/settings/sections/EndpointCard.tsx`, `frontend/src/components/settings/types.test.ts`, `README.md` (`.coding/*` out of scope).

## Critical

No findings.

## High

No findings.

## Medium

### M1 — Settings import drops `model_configs`; this change makes that gap bite more users (pre-existing, amplified)

`frontend/src/components/settings/sections/AdvancedSection.tsx:256-272` — the import mapping builds `EndpointEditable` **without `model_configs`** (and casts `e.models` to `string[]`). Export (`buildExportBundle`, types.ts:407) passes `settings.endpoints` through raw, so exported bundles **do** contain `model_configs`, but importing such a bundle silently discards every per-model cap/effort list.

This predates the diff (AdvancedSection untouched), but it is directly relevant here: before this change, discovery auto-fill wrote the *endpoint-level* fields, which survive import; now auto-fill writes *per-model* entries, which import silently loses. A user who exports settings, wipes, and re-imports loses all auto-filled caps with no warning. Recommend mapping `model_configs` in the import path (mirroring the `ProvidersSection.tsx:61-66` load mapping), ideally in this PR or as a fast follow-up.

## Low

### L1 — Empty/whitespace `base_url` loads as `"/"` instead of staying empty

`src/config/endpoints.rs:314-321` — for `base_url = ""` (or whitespace-only) in a hand-edited file, `trimmed == ""`, which does not end with `/`, so the normalization fabricates `ep.base_url = "/"`. The save path (`validate_endpoint`, src/config/patch.rs:48-51) *rejects* an empty base_url; the load path now converts it into a slash-only relative URL that will fail confusingly at request time and no longer reads as "empty" in the UI. Previously it loaded as `""` (broken but visibly empty). Suggest skipping normalization when `trimmed.is_empty()`:

```rust
if trimmed.is_empty() { continue; } // or leave as trimmed empty string
```

Severity is low: an empty base_url is broken either way; this only changes the failure mode and masks the emptiness.

### L2 — "Apply discovered" is a silent no-op for a ghost reference model

`frontend/src/components/settings/sections/EndpointCard.tsx:541-547, 563-569` — `capsRefModel` is `lastPicked ?? endpoint.models[0]`. If the user picks a model from the server picker and then deletes that model row, `lastPicked` is stale, `capsById[capsRefModel]` still holds caps, so the conflict warnings render — but the Apply buttons write via `upsertModelConfig`, which prunes configs for ids not in `models` (types.ts:252-260), so the click changes nothing and the warning stays. The old code wrote the endpoint-level field, so Apply always worked. (Auto-fill correctly skips ghosts via the membership guard at types.ts:278; only the manual Apply path is affected.) Narrow UX edge; consider hiding the hints when `capsRefModel` is not in `endpoint.models`, or clearing `lastPicked` on row delete.

### L3 — PLAN.md not updated for load-time normalization (doc-sync)

`PLAN.md:657` says "base_url saves are forgiving (missing trailing `/` auto-appended)" — save-path only. This change adds the same normalization to the load path (`load_or_default`), and the README bullet was updated to say "on save and load", but PLAN.md was not. Per the project's documentation-sync expectation, update that line to mention loads (PLAN.md:219-220/650 already cover the per-model tables, so only the base_url sentence is stale).

## Areas verified clean (no findings)

**Load normalization correctness (beyond L1):** trim + append-slash logic is right for the normal cases; the `else if trimmed.len() != ep.base_url.len()` branch correctly trims whitespace on already-slashed URLs and leaves clean URLs untouched. Query-string/fragment URLs (e.g. `…/v1?x=y` → `…/v1?x=y/`) were never supported by the URL builders (they concatenate path segments), so appending `/` regresses nothing. Embedded credentials (`https://user:pass@host/v1`) survive intact — normalization only trims outer whitespace and appends `/`, so it cannot strip or corrupt userinfo. Existing tests are unaffected: all parse tests use `toml::from_str` directly (bypassing `load_or_default`), and both round-trip tests (`save_round_trips_endpoints_and_pricing` endpoints.rs:847, `save_empty_writes_loadable_file` :902) use slashed URLs, so normalization is a no-op there. Load/save consistency with `validate_endpoint` confirmed.

**Regression tests genuinely fail on old code:** `load_normalizes_missing_trailing_slash` fails without the fix (URL loads un-slashed). The 4 `capsAutofillPatch` tests fail on old code (function didn't exist). `load_preserves_slashed_and_path_urls` is a guard test that passes both before and after — acceptable as a companion pinning "never rewrite a good value", since the first test carries the regression.

**`capsAutofillPatch` logic (types.ts:273-289):** `== null` checks correctly cover `null`/`undefined` (the `EndpointEditable` fields are optional). The membership guard's `m.trim() === modelId` asymmetry vs. the exact `===` matching in `modelConfigFor`/`upsertModelConfig` is harmless (a whitespace-padded model id never has `refCaps`, so the effect early-returns first). Merge safety confirmed: the patch object only ever contains keys assigned a verified-non-null `number`, so `{ ...current, ...patch }` in `upsertModelConfig` cannot clear an existing value — and the "never overwrites an explicit per-model value" test proves it. Returning the full `model_configs` array (which materializes all-null bare entries for the other models) is semantically inert (null = inherit; backend matches by id).

**EndpointCard effect (137-147):** deps widening to `[capsRefModel, refCaps, endpoint]` adds re-runs on every endpoint keystroke, but cannot loop or double-write: the `filledForRef` guard is set *before* `onEndpointChange`, and after the write lands `capsAutofillPatch` returns null anyway. Guard-per-(model, caps) semantics (manual clear stays cleared) are preserved from the old code.

**Conflict warnings (525-584):** no stale `endpoint.max_context`/`max_output_tokens` references remain in the hints — all comparisons use `effCapsCtx`/`effCapsOut` (per-model ?? endpoint-level), which matches the backend merge order (pinned by `per_model_caps_override_endpoint_level`, endpoints.rs:582). After Apply, the per-model value equals the discovered value, so the warning clears. The "Caps detected" note's new wording ("empty per-model fields auto-fill") accurately describes the new behavior.

**Single-model UX coherence:** endpoint-level inputs keep placeholder `"default"` and stay empty; the autofilled value appears in the per-model `ModelRowConfig` strip. Functionally equivalent post-save (backend merges per-model over endpoint-level); intended by the plan. Coherent.

**Constitution compliance:** new/changed pub fns have doc comments (`load_or_default` doc extended, `capsAutofillPatch` documented). No new imports or `#[allow]`; `tempfile` is already a dev-dependency (Cargo.toml:69) and already used by sibling tests — `#![deny(warnings)]` compatible. Multi-platform neutrality: pure string ops in Rust, pure TS; no `cfg(windows)`, no platform paths. README updated accurately (mentions save *and* load). Security: no secrets in new UI text (model id + token counts only).

## Suggested disposition

- M1: fix in this PR (map `model_configs` in the AdvancedSection import) or explicit follow-up — pre-existing, but this change increases its blast radius.
- L1: one-line guard (`trimmed.is_empty()`), worth folding in.
- L2: optional; hide hints when `capsRefModel` isn't in `endpoint.models`.
- L3: one-line PLAN.md edit.

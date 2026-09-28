## Verdict: FINDINGS (0 high, 3 low)

Review of plan b3188918's uncommitted delta on `wt/mnemo` (working tree dirty; scope = the 13 modified files + 2 new knowledge records). Read in full: `src/provider/anthropic.rs` (config field, `build_request_json` effort table, both new tests), `src/provider/client_factory.rs` (`anthropic_client_config`, `build_client`, `resolve_effort`/`build_provider_for`), `src/provider/models.rs` (`ModelWithVision`, dedup merge, `model_effort_levels`, `model_reported_caps`, `model_supports_vision`, new tests), `src/config/endpoints.rs` (`effective_reasoning_effort_for`, `normalize_reasoning_effort_for`, display twins, `reasoning_efforts_for`), `frontend/src/components/settings/types.ts` (`capsAutofillPatch`, `effortSurvivesAllowList`, `DiscoveredCaps`, `discoveredCapsById`), `EndpointCard.tsx` (autofill effect, hints, un-guarded controls), `frontend/src/lib/tauri.ts`, doc/comment changes in `src-tauri/src/ipc/models.rs`, and the plan + knowledge records.

Summary: the design is correctly implemented against the verified API facts. All six risk probes pass (detail below). Three low findings — one misleading UI note, one UI-ladder gap for `xhigh`, one doc-precision nit.

Reviewed-state: ed8fb1d0dce4cde7b7708406dfccce1f8b955873

## Risk probes (all pass)

**(a) No `thinking` field; `off` byte-identical.** `build_request_json` (anthropic.rs:435–457) only inserts `output_config` when the trimmed effort matches a table member (`eq_ignore_ascii_case`); `off`/unset/unknown/blank all miss the table and the body is untouched — structurally identical to pre-change since `output_config` is the only addition. No request-building path emits `thinking` (every `thinking` occurrence is response-side block echo/history hygiene or tests). Both new tests assert exactly this. Serving path: `build_provider_for` → `resolve_effort` → `normalize_reasoning_effort_for` (anthropic `off` → no off-wire → `None` → field omitted) → `anthropic_client_config` → the config field. Even a misconfigured `reasoning_effort_off_wire` on an anthropic endpoint resolves `off` to an off-ladder value, which the table drops — fails safe.

**(b) Allow-list order invariant.** Rust discovery emits highest-first (`EFFORT_LADDER` reversed, merge re-sorts `Reverse(effort_ladder_index)`, models.rs:216), matching the backend clamp to `list[0]` (endpoints.rs:457–468) and the `max,high,…` settings convention. `off` is resolved to `None` at endpoints.rs:456 (`resolved?`) — before the clamp — so `off` can never be clamped into a literal level; the display twin early-outs the same way. Frontend `effortSurvivesAllowList` mirrors the backend chain exactly (`model ?? endpoint ?? "max"`, `off` always survives) and `capsAutofillPatch` refuses to write a list the effective effort wouldn't survive. Consistent end to end.

**(c) No silent downgrade / empty list.** `capsAutofillPatch` writes `reasoning_efforts` only when the discovered list is non-null AND non-empty AND the current list is empty AND the effective effort survives — an empty allow-list is un-writable, and a downgrade case defers to the explicit Apply hint. The `guardKey` includes `efforts`, so a caps change re-arms autofill; the hint's Apply writes only on explicit click. No double-write path found (autofill and hint are mutually exclusive on the empty-vs-nonempty list condition). See LOW 1 for the one cosmetic mismatch.

**(d) Key collisions.** `max_input_tokens`/`max_tokens` are appended AFTER the existing context keys (`context_length`/`max_context_length`/`max_model_len`) and output key (`max_completion_tokens`), so no provider whose existing key the code already reads changes behavior. Among known OpenAI-compat surfaces (OpenAI, Ollama, LM Studio, vLLM, OpenRouter, z.ai) none carries a top-level `max_tokens` meaning the context window; Anthropic's meaning is the intended one. Zero/negative/non-numeric values are filtered (`first_positive_u64`) — no poison values. Acceptable residual risk, documented in the doc comment's precedence list.

**(e) Guard removals.** The three un-guarded controls are now correctly gated on `supports_reasoning_effort` alone (EndpointCard 453, 754, per-row `disabled`), which is the real semantic gate: unchecked = omit `output_config` — the "field omitted" affordance is now accurate for anthropic. No DeepSeek-style off-wire assumption leaks: the name-based `none` policy can only surface for DeepSeek-named models, and even then the anthropic table drops the value (see (a)). The workspace-id field stays anthropic-only.

**(f) Doc claims.** PLAN.md "hidden for anthropic hosts" corrected; endpoints.rs/general.rs/ipc/models.rs comments match the shipped code; the spec knowledge record (54926eab) matches the implementation (output_config only, never thinking). Bookkeeping accuracy: plan b3188918.md and the two new knowledge records match what shipped.

## Findings

### LOW 1 — "Values detected … auto-fill with these values" note can be false for the effort list (EndpointCard.tsx:621–629)

The muted note renders whenever `refEfforts` is non-empty and `refModelEfforts` is empty, but there are two reachable states where the effort allow-list will never auto-fill: (i) the `effortSurvivesAllowList` guard declined (e.g. effective effort `max`, discovered `[high, medium, low]`) — autofill is deterministically blocked yet the note still promises it; (ii) a partial autofill (ctx/out only) set `filledForRef`, so a later user change that would make efforts eligible still never fills (guard key unchanged). Failure is in the safe direction (no silent downgrade), but the text misleads. Fix: condition the note's effort clause on `capsAutofillPatch` actually being willing to fill the effort field (e.g. reuse the survives check for the current effective effort), or split the sentence so the effort claim only appears when it applies.

### LOW 2 — `xhigh` is discoverable but not selectable in the UI ladder

`EFFORT_LADDER` probes `xhigh`, the merge/dedup can emit it, the wire table maps it (`xhigh`→`xhigh`), and the allow-list can legitimately contain it — but `REASONING_EFFORTS` (types.ts:188–195, used by both the endpoint Effort select and the per-model effort select) offers only `max..off`. A model supporting only `xhigh`/`high` can have `xhigh` written into its allow-list yet the user can never choose it from the dialog (only via endpoints.toml). Harmless today (Anthropic models also report `max`), but if `xhigh` is real API surface it should be an option; if not, stop probing it.

### LOW 3 — doc precision nits (no behavior impact)

- client_factory.rs:120–122 states "`off` resolves to `None` for this kind" without the `reasoning_effort_off_wire` escape-hatch caveat (with one configured, `off` resolves to the wire value — harmlessly dropped by the client's table, per (a)).
- endpoints.rs:480 / client_factory.rs:241 say the display vocabulary is `off|low|medium|high|max`, but the code deliberately passes `minimal` and `xhigh` through display normalization too (and the frontend ladder includes `minimal`). One-line wording fix.

## Constitution checks

- **Documentation sync** — pass: PLAN.md, module docs, UI tooltips, and the knowledge records were all updated with the change (LOW 3 is a precision nit, not a stale doc).
- **Multi-platform neutrality** — pass: no platform-specific API, path, or shell syntax anywhere in the delta.
- **File-tools-first** — pass: no shell-based file mutation in the diff.
- **Warning-free build** — pass by construction: `#![deny(warnings)]` at both crate roots + green `cargo test` (2880 lib / 19 integration / 1 doc) per the dispatch verification state; no `#[allow(...)]` added.
- **Bookkeeping accuracy** — pass: plan + knowledge records match the shipped code.

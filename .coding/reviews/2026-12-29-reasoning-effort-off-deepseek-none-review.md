## Verdict: PASS

Review of all uncommitted changes on `wt/agenticcoding` for bug plan a73d4855 (backlog 3f3044b9: reasoning_effort "off" → "none" for DeepSeek-family). Scope: 8 modified files (231+/25−) + 4 untracked `.coding/` side-car files. The fix is correct, complete at all three layers, safe against the bug-1042 tail contract, platform-neutral, and fully documented. No changes required; two non-blocking notes at the end.

## Scope

`git diff HEAD --stat`: `.coding/backlog.jsonl`, `PLAN.md`, `src-tauri/src/ipc/config_io.rs`, `src-tauri/src/ipc/contract_fixtures.rs`, `src/config/endpoints.rs`, `src/provider/client_factory.rs`, `src/provider/openai.rs`, `src/provider/policy.rs`. Untracked: the bug + decision knowledge records, `.coding/plans/a73d4855.md`, and the stale round-2 auto-delegate review report (leftover from failed plan ead0d61c — should ride along in the commit).

## (a) Correctness — mapping, clamp arms, builder guard, non-DeepSeek unchanged

- **Policy fn** (`policy.rs:261–277`): `reasoning_effort_off_wire_value` returns `Some("none")` iff `ProviderPolicy::for_kind_and_model(kind, model).vendor == Vendor::DeepSeek`. Verified the detection it leans on (`policy.rs:225–252`): model id is lowercased and `contains("deepseek")` → `deepseek()` for **both** `ProviderKind::OpenAI` and `Local`; `ProviderKind::Anthropic` short-circuits to `claude()` (vendor Anthropic) before any model-id matching — the Anthropic gate holds by construction, and the test asserts it explicitly with a deepseek-named model id. Case-insensitivity is inherited (`to_ascii_lowercase`).
- **Both resolver arms patched, both resolvers**: `config_io.rs resolve_reasoning_effort` (:93 `off_wire`, used at the `Some("off")` arm and the clamp arm) and `endpoints.rs effective_reasoning_effort_for` (:349–356, same two arms). In each, `off_wire` is computed once from `(provider_kind(ep.kind), model_id)` and both `"off"` arms map through it — no arm can emit a literal `"off"`. The `Some(v)` verbatim arm is unreachable for `"off"` (matched earlier). Clamp semantics otherwise unchanged (clamp to `list[0]`, encode only when the clamped entry is `"off"`).
- **Builder guard** (`openai.rs:1771–1781`): `effort == "off"` → policy value or omit; anything else verbatim. This is the only `reasoning_effort` body-emission site in the file (verified by full-file search: the only other touch is the self-healing fallback at :877–881, which *removes* the field on an effort-rejection 400 — a removal, not an emission). No Responses-API or other path can leak a literal `"off"`.
- **Non-DeepSeek behavior unchanged**: `off_wire` is `None` for every non-DeepSeek `(kind, model)`, so all three layers reproduce the historical omit. The four pre-existing tests (`resolve_reasoning_effort_off_maps_to_none`, `resolve_reasoning_effort_off_only_list_never_sends_literal_off`, `effective_reasoning_effort_off_omits_field`, `effective_reasoning_effort_off_only_list_never_sends_literal_off`) are untouched and green; `build_request_json_omits_reasoning_effort_when_none` likewise.
- **Regression tests fail at HEAD**: verified by inspection — the four resolver tests assert `Some("none")` where HEAD returned `None`; the two builder tests assert `"none"`/absent where HEAD sent the literal `"off"`. All six are genuine defect reproducers.
- **`model_id.unwrap_or_default()`** in `effective_reasoning_effort_for`: the only `None`-model caller is the no-arg wrapper `effective_reasoning_effort()` (its sole caller is a test); every production caller (console.rs:536, config_io.rs:110/117, memory_maintenance.rs:274, settings.rs:254, main.rs:1074, client_factory.rs:203) passes a concrete model id. Even in the `""` case the result is omit — the pre-change, safe behavior.

## (b) Tail-validation safety (bug 1042) — verified

The age-0 `reasoning_content` injection (`openai.rs:1577–1585`) fires on `assistant_age == 0 && policy.reasoning_required && policy.reasoning_field == Some("reasoning_content") && echoed lacks the key` — **purely policy-driven by model id, not effort-gated**. The decision record's reading is correct. Emitting `"reasoning_effort":"none"` therefore cannot reintroduce the "reasoning_content must be passed back" 400: the tail guarantee already holds unconditionally for DeepSeek-family models, and is in fact *stronger* than the live-verified contract (which is conditional on effort presence). Additionally, the pre-existing self-healing fallback (`openai.rs:877–881`, `EFFORT_REJECTION` :5564+) retries once without the field if any server rejects the effort value — which also covers the Local-kind DeepSeek case (a local server that didn't accept `"none"` self-heals to an omitted field).

## (c) Bugs & security

No new attack surface: one pure function, visibility widening of a pure kind→kind mapping (`provider_kind`, doc comment intact), and two resolver arms + one builder branch of string mapping. No secrets, no I/O, no new input handling. `contract_fixtures.rs` (`deleted_at: None` ×2) is the stated pre-existing build-breakage repair for the BacklogItem soft-delete field — minimal and correct, unrelated to the bug but required for the build.

## (d) Constitution

Doc comments on all public surface: `reasoning_effort_off_wire_value` (full rationale), `provider_kind` (pre-existing), plus updated docs on `resolve_reasoning_effort`, `Endpoint.reasoning_effort`, `effective_reasoning_effort{,_for}`, `OpenAiClientConfig.reasoning_effort`. Warning-free build evidenced by the green suite under `#![deny(warnings)]` (mnemo lib 1917 passed, src-tauri 182+4 passed, 0 failed). Six regression tests added at all three layers.

## (e) Documentation sync

PLAN.md item 7 gained the encoding sentence (accurate, names the policy fn and the builder test). Code-level docs updated everywhere the behavior changed. README.md needs nothing: its reasoning-effort mentions (:57, :59, :122) describe support/allow-lists generically and remain true; the off-encoding detail lives at the right altitude (PLAN.md + rustdoc on the config field users actually set).

## (f) Multi-platform neutrality

Pure JSON/string logic end to end — no `cfg(windows)`, no paths, no shell, no platform APIs anywhere in the diff. Builds and behaves identically on macOS and Windows.

## Call-site propagation (behavioral completeness)

The configured-off path now genuinely reaches DeepSeek as `"none"`: all four `build_client` effort arguments flow from `effective_reasoning_effort_for(Some(&model))` (settings.rs:254, main.rs:1074, memory_maintenance.rs:274, client_factory.rs:203 `build_provider_for`). The dropdown path flows through `resolve_reasoning_effort` (agent.rs:556 → config_io.rs:156). `model_swap_effort` (console.rs:555) uses the raw `requested_effort`, so swap semantics are untouched; the console status surface renders `Some("none")` accurately (the field *is* sent). The decision record's gap (2) — DeepSeek never receiving its explicit thinking-off value — is closed by exactly these paths.

## Notes (non-blocking, no action required)

1. `.coding/backlog.jsonl` flips item 3f3044b9 to `"status":"failed"` (note: "plan loop did not close") — transient bookkeeping from the prior failed attempt; the finish sequence will update it. Union-merge side-car; harmless in the commit snapshot.
2. The untracked `.coding/reviews/2026-12-28-auto-delegate-search-review-round2.md` is a leftover from the failed auto-delegate plan (ead0d61c) — it should be swept into this plan's commit rather than left dangling.

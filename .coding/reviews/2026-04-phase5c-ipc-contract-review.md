# Code Review — Phase 5c: IPC contract hardening (Maint H4)

**Reviewer:** read-only reviewer (spawn_agent)
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`), reviewed against the plan's goal (hardening the Rust↔TS IPC contract against silent drift; no FE behavior change; no ts-rs/specta).
**Branch:** `feat/bookkeeping-tools-autorun` (not main — no main-commit violation).

## Files reviewed

- NEW `tests/contract_fixtures.rs` (brain crate event fixtures)
- NEW `src-tauri/src/ipc/contract_fixtures.rs` (app crate DTO fixtures)
- MODIFIED `src-tauri/src/ipc/mod.rs` (declares `#[cfg(test)] mod contract_fixtures;`)
- MODIFIED `src-tauri/src/ipc/settings.rs` (typed response structs + 4 command changes + `endpoint_wire` helper + 5 shape tests)
- NEW `frontend/src/lib/ipc-fixtures/*.json` (29 fixture files — 21 events + 8 DTOs)
- NEW `frontend/src/lib/ipc-contract.test.ts` (TS-side contract suite, 29 tests)
- MODIFIED `frontend/vitest.config.ts` (includes the new test)
- `.coding/plans/*` bookkeeping (ignored per instructions)

## Verification performed

- Cross-checked the new `settings.rs` typed-struct bodies field-by-field against `git show HEAD:src-tauri/src/ipc/settings.rs` (the old `json!` bodies) for all four responses.
- Read every fixture JSON and matched it against the source enum/struct serde forms (`SafetyMode` `#[serde(rename_all="kebab-case")]`, `WorkflowState` `rename_all="lowercase"`, `FinishReason` `rename_all="snake_case"` + `Other(String)`→`{"other":..}`, `ApprovalPreview` `tag="kind"`, `BacklogStatus` `rename_all="snake_case"`, `WorkflowStateInfo.skill` `skip_serializing_if`, `PlanFile` fields).
- Verified path resolution for both crates (`CARGO_MANIFEST_DIR` = repo root for brain crate; `../frontend/...` for app crate in `src-tauri`).
- Byte-level line-ending scan of every changed/new file (CRLF vs lone-LF vs lone-CR, per-file).
- Confirmed `Endpoint` has no `api_key` field (secrets stay out of wire output); scanned all fixtures for key/secret/token patterns.
- Ran tests: `cargo test --workspace` → 508 + 1 + 9 + 5 + 47 (all 0 failed); `npm test` → 5 files, 78 tests (incl. `ipc-contract.test.ts` 29 tests) all pass; `npx tsc --noEmit` → exit 0.
- Confirmed the 2 `unused_mut` warnings in `src/runtime/agent.rs:448,511` are pre-existing and unrelated to this diff.
- Confirmed `reduceChildFinished` records `agentNames[child_id]=name` and `reduceError` sets `running=false` when `retrying=false` (TS test assertions are sound).

## Findings by severity

### Correctness / behavior preservation — no findings

The typed-struct conversion preserves the wire output exactly. Verified field-by-field against the old `json!`:

- **get_config** (`GetConfigResponse`): `general.{default_provider, default_model, safety}` + endpoints (name/kind/base_url/models/max_context/max_output_tokens/multimodal/supports_reasoning_effort/reasoning_effort) + pricing (model/input_per_1m/output_per_1m/cached_per_1m) — all keys present, same names, same order-independent equality. `safety` is the on-disk `config.general.general.safety` enum (serialized via `SafetyMode`'s `#[serde(rename_all="kebab-case")]`) — identical to the old `config.general.general.safety` (which serde rendered the same way).
- **get_settings** (`GetSettingsResponse`): `config_dir`, `general.{default_provider, default_model, safety, vision_model}`, `context.summarize_at_fill_rate`, `ui.{theme, show_token_usage}`, endpoints, pricing, `projects.{name, path}` — all present. **Critical invariant upheld:** `default_provider`/`default_model`/`vision_model` are `Option` with NO `skip_serializing_if`, so `None`→`null` (not key-absent), matching the old `json!` which emitted `null`. Confirmed by `dto-get-settings.json` (shows `"vision_model": null`, `"default_provider": null`).
- **Key distinction preserved:** old `get_config` used the *on-disk* `config.general.general.safety`; old `get_settings` used the *live* `safety_mode_wire(runtime_safety)`. New code preserves this exactly: `GetConfigGeneral.safety = config.general.general.safety` (on-disk enum), `GetSettingsGeneral.safety = runtime_safety` (live enum). Both render kebab-case via `SafetyMode`'s own serde attr — verified all 4 variants (`approve-each-action`, `auto-read-approve-writes`, `auto-approve-project`, `autonomous`) equal `safety_mode_wire`'s output.
- **get_settings safety equivalence:** `SafetyMode` serializes to the kebab-case form itself (its `#[serde(rename_all="kebab-case")]`), so `safety: runtime_safety` renders the identical string the old `safety_mode_wire(runtime_safety)` produced — for all 4 variants. ✓
- **endpoint_wire helper:** produces the same JSON the inline `json!` did — `kind` is `endpoint_kind_wire(e.kind)` (the wire form `"openai"`/`"local"`, NOT Debug), all 9 fields in the same set. Used by both `get_config` (`.map(|e| endpoint_wire(e))`) and `get_settings` (`.map(endpoint_wire)`). ✓
- **save_endpoints** (`SaveEndpointsResponse`): `{default_provider, default_model, provider_swapped}` — old moved the params into `json!`; new `.clone()`s them at the end (no use-after-move; `.clone()` is strictly safer). ✓
- **save_settings** (`SaveSettingsResponse`): `{ok, safety, vision_configured}` where `safety: Option<&'static str> = parsed_safety.map(safety_mode_wire)` — identical to old. ✓

The fixtures + shape tests are NOT merely tautological: the 5 `settings_dto_tests` shape tests assert the typed structs render byte-identically to the *expected* `json!` (hand-written expected JSON, e.g. `"default_model": null`, `"safety": "auto-approve-project"`), so a future edit (adding `skip_serializing_if`, renaming a field) is caught independently of the fixtures. The cross-check against `git show HEAD:settings.rs` confirms the expected JSON in those tests matches the *actual old* `json!` bodies. The generate-on-missing pattern cannot mask drift: a present-but-stale fixture triggers `actual != expected` → panic with a drift message (not a silent pass); generate-on-missing only fires when the file is *absent*, which then panics "newly generated — review & re-run". Correct design.

### Bugs — no findings

- No typo'd fixture values; every fixture matches its source serde form (spot-checked all 29).
- Path resolution is correct for both crates (verified `CARGO_MANIFEST_DIR` resolves: brain crate root → `frontend/src/lib/ipc-fixtures/`; app crate `src-tauri` → `../frontend/src/lib/ipc-fixtures/`; both reach the same single source of truth).
- No unhandled event kind: the brain test covers all 21 `SerializableAgentEvent` variants (incl. 3 extras: `Finished::Other`, `ApprovalPreview::NewFile`, `preview: None`); the TS reducer's `applyAgentEvent` switch handles all 20 kinds + `default` fallback.
- No unused imports/dead code: `tests/contract_fixtures.rs` uses `json!`, `Value`, `ApprovalPreview`, `FinishReason`, `SerializableAgentEvent`, `ToolResult`, `WorkflowState`; `contract_fixtures.rs` uses all 11 imported symbols; `settings.rs` new structs are all referenced (the 4 responses + `endpoint_wire` + `EndpointWire`/`PricingWire`/`ProjectWire`/`VisionModelWire` used by both responses and fixtures).
- The `as unknown as AgentEventPayload` cast is intentionally loose (documented in the test's doc comment): JSON imports infer `kind: string`, not the literal union, so a compile-time check isn't viable. Runtime protection is the dispatch + per-variant state-slice assertions — a malformed fixture (wrong `kind`) hits the reducer's `default` branch (no state change) and fails the per-variant assertion (e.g. expecting `running=true` but agent missing). Acceptable.
- `dtoWorkflowStateInfo.skill` access via `Record<string,unknown>` cast is sound: `skill` has `skip_serializing_if = "Option::is_none"`, so the key is absent when `None`; the JSON import infers the shape *without* the absent key, and the cast + `expect(...).toBeUndefined()` correctly asserts runtime absence. The skill-state fixture (`dto-workflow-state-info-skill.json`) has `skill` present and `plan: null` (plan has no skip → `null` emitted), matching the test's `expect(plan).toBeNull()` / `expect(skill).toBeDefined()`.
- The `child_finished` test dispatches to agent `ID` with `child_id: 2` (SUB); `reduceChildFinished` records `agentNames[2]="reviewer"` and sets agent `ID` `running=false` — both assertions hold. ✓
- The `event-tool-result` test correctly chains `toolCallStart` → `approvalRequest` → `toolResult` (matching `tool_call_id: "call-1"`) so `pendingApproval` clears and the call result lands. ✓

### Security — no findings

- No secrets in any fixture: scanned all 29 JSONs for `api_key`/`apikey`/`secret`/`token`/`password`/`sk-` — only false-positive field names (`*_tokens`, `max_output_tokens`, `max_tokens` FinishReason value). No real credentials.
- The four typed response structs contain NO key fields. `get_config`/`get_settings` continue to exclude API keys (the `KeyStore` is never serialized); `save_endpoints`/`save_settings` responses carry only provider/model names + flags. The `get_api_keys` command (separate, gated) was not touched. No-secrets guarantee preserved.
- No path traversal / injection surface introduced (fixtures are committed JSON read with `std::fs::read_to_string` + `serde_json::from_str`; no user input flows into fixture paths).

### Constitution compliance — 1 finding (LOW / informational)

1. **Line-ending style of the two new `.rs` files (LOW, informational).**
   - **Files:** `tests/contract_fixtures.rs`, `src-tauri/src/ipc/contract_fixtures.rs`.
   - **Observation:** Byte scan shows both new `.rs` files are pure-LF (`loneLF` counts, 0 CRLF, 0 lone-CR — so NOT mixed, which would violate the "no mixed \r\n/\n within a file" rule). However, every *existing* `.rs` file in the repo is CRLF (`src/lib.rs`, `src/runtime/channels.rs`, `src-tauri/src/ipc/settings.rs`, `src-tauri/src/ipc/agent.rs`, `src-tauri/src/main.rs`, and the existing `tests/*.rs` files `ipc_bridge.rs`/`provider_integration.rs`/`workflow_integration.rs` are all CRLF). The new `.rs` files therefore don't match the repo's `.rs` line-ending style. The new `.test.ts` file and the JSON fixtures are also LF, but LF is already the norm there (`useAgentStore.preview.test.ts`, `DiffView.test.ts` are LF; JSON fixtures are conventionally LF).
   - **Impact:** Low. The repo has `core.autocrlf=true` and no `.gitattributes`, so git normalizes on commit (stores LF) and checks out CRLF for `.rs` — after a fresh checkout the working tree would show CRLF, and no *mixed* endings are introduced within any file. The constitution's strict prohibition is on *mixed* endings within a single file, which does NOT occur here. The "preserve the line-ending style of existing files" guidance is a soft preference that the file tools normally enforce automatically; these files were authored with LF.
   - **Suggested fix (optional):** Re-save the two new `.rs` files with CRLF to match the existing `.rs` convention (e.g. open + save via an editor that emits CRLF, or `((Get-Content -Raw tests/contract_fixtures.rs) -replace "(?<!`r)`n", "`r`n") | Set-Content -NoNewline`). Not required for correctness — `cargo test` and `npm test` are green regardless. No action needed if the team is fine with LF new `.rs` test files under autocrlf.

## Other constitution checks — all pass

- **Doc comments:** Every new public struct in `settings.rs` (`EndpointWire`, `PricingWire`, `VisionModelWire`, `ProjectWire`, `GetConfigResponse`, `GetConfigGeneral`, `GetSettingsResponse`, `GetSettingsGeneral`, `GetSettingsContext`, `GetSettingsUi`, `SaveEndpointsResponse`, `SaveSettingsResponse`) has a `///` doc comment, and every public field has a `///` doc. The private `endpoint_wire` fn is documented. The four `#[tauri::command]` functions retain their existing doc comments. The `#[cfg(test)] mod contract_fixtures` (private, test-only) has a module doc comment. ✓
- **No direct commits to main:** changes are on `feat/bookkeeping-tools-autorun`. ✓
- **cargo test before complete:** `cargo test --workspace` → all green (508 + 1 + 9 + 5 + 47, 0 failed). ✓
- **npm test + tsc:** `npm test` → 78 tests pass (incl. 29 in `ipc-contract.test.ts`); `npx tsc --noEmit` → exit 0. ✓
- **Line-ending style (mixed-endings prohibition):** No file has mixed `\r\n`/`\n` within it — all changed/new files are uniformly one style (existing modified files stayed CRLF; new files are LF). The only soft-style observation is item 1 above. ✓

## Conclusion

The diff is correct and behavior-preserving. The typed-struct conversion renders byte-identically to the old `json!` bodies (nulls emitted, no skipped keys, safety/vision/provider/model shapes all preserved across all 4 responses), the golden fixtures + shape tests genuinely guard against drift (cross-checked against the old `json!`, not merely tautological), the generate-on-missing pattern cannot mask drift, and the TS contract suite catches the same shapes end-to-end. No correctness, bug, or security findings. One low/informational constitution-compliance observation: the two new `.rs` files use LF where the repo's `.rs` convention is CRLF (not mixed, autocrlf normalizes — optional fix only). `cargo test`, `npm test`, and `tsc --noEmit` are all green.
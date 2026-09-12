## Verdict: FINDINGS (0 high, 1 low)

Plan 5cf7419e's dedup is a faithful, semantics-preserving code motion: lock scoping preserved (guard drops before the HTTP fetch in both commands), credential/delegation blocks verbatim modulo the four declared adjustments, kind_wire/endpoint_kind_wire mappings confirmed identical, and the five new tests pin the five resolution paths with correct env serialization. One LOW (optional hardening, mirrors an established in-repo precedent): two non-env tests read env vars without the ENV_TEST_LOCK while the env tests mutate them — a theoretical Unix-only race.


### Scope reviewed

`git diff HEAD` on wt/agenticcoding plus untracked files: `src-tauri/src/ipc/models.rs` (helpers extracted, commands thinned, kind_wire deleted, new test module), `src-tauri/src/ipc/settings.rs` (1 line: `fn` → `pub(crate) fn` on `endpoint_kind_wire`), `.coding/backlog.jsonl` (status flip pending → in_flight), untracked `.coding/plans/5cf7419e.md`.

### Constraint 1 — Lock scoping preserved: VERIFIED ✓

`list_models` (models.rs:136-141) and `list_vision_models` (models.rs:172-177): the `state.project.config.lock().await` guard is created inside the block and dropped at the closing `}` — `fetch_by_kind(...).await` (lines 141, 177) runs after the drop. Identical to the original scoping; the lock is held only for credential resolution, never across the HTTP fetch, so concurrent model listing is not serialized.

### Constraint 2 — Pure code motion: VERIFIED ✓

Line-by-line against the diff's removed blocks:

- **Credential block → `resolve_endpoint_credentials` (models.rs:30-67):** byte-identical apart from the four declared adjustments. Filter conditions (`.filter(|u| !u.trim().is_empty())` / `.filter(|k| !k.trim().is_empty())`), env-var names and their order (anthropic: `ANTHROPIC_API_KEY` → `ANTHROPIC_AUTH_TOKEN`; else `OPENAI_API_KEY` → `ANTHROPIC_AUTH_TOKEN`), the error message text ("endpoint '{endpoint_name}' has no base_url and does not match a saved endpoint"), and the `"openai"`/`"dummy"` defaults all match verbatim. No logic drift.
- **Delegation → `fetch_by_kind` (models.rs:77-91):** identical apart from `&base_url, &api_key` → `base_url, api_key` — the helper's params are `&str`, so the same values are passed (deref coercion instead of explicit borrow).
- **`config.endpoint(&endpoint_name)` → `config.endpoint(endpoint_name)`:** same call — `Config::endpoint(&self, name: &str) -> Option<&Endpoint>` (src/config/mod.rs:87). `saved` is `Option<&Endpoint>` (references are Copy), so the three closure captures compile exactly as the original inline code did.
- **`kind_wire` → `endpoint_kind_wire`:** the two mappings were identical — OpenAI→"openai", Local→"local", Anthropic→"anthropic" (old `kind_wire` body visible in the diff's removed lines; survivor at settings.rs:638-644, pinned by the pre-existing `endpoint_kind_wire_matches_serde` test at settings.rs:1208-1211). The `fn` → `pub(crate) fn` change is visibility-only. `kind_wire` is fully deleted — zero remaining references repo-wide (remaining hits are the survivor, its test, and prose).
- Command signatures are unchanged → no frontend or invoke_handler impact (main.rs correctly untouched).

### Constraint 3 — Test quality: VERIFIED ✓ (one LOW below)

The five tests pin the five review paths:

1. `override_wins_over_saved` (models.rs:200) — override base_url/api_key win over the saved endpoint + stored key; an explicit kind override wins too; no kind override → the saved kind's wire form ("openai").
2. `saved_endpoint_fallback` (models.rs:228) — saved base_url + keys.toml key + saved kind wire ("anthropic").
3. `kind_aware_env_fallback_anthropic_vs_openai` (models.rs:244) — no stored key; anthropic kind prefers ANTHROPIC_API_KEY, openai kind prefers OPENAI_API_KEY.
4. `empty_base_url_errors` (models.rs:263) — no override + no saved endpoint → the "has no base_url" error; also the whitespace-only-override sub-case (empty treated as absent).
5. `dummy_terminal_fallback` (models.rs:289) — saved endpoint, no stored key, env removed → "dummy" (plus kind "local", transitively covering `endpoint_kind_wire`'s Local arm from models.rs's perspective).

Env serialization: `ENV_TEST_LOCK` (models.rs:241) serializes the two mutating tests, and each sets up its own env state under the lock (no order dependence between them). The non-env tests' outcomes are env-independent (override/stored key win before the env fallback is consulted for the result; `empty_base_url_errors` returns before any env read). No other test in the src-tauri binary reads these vars — directly (searched: only console.rs/main.rs with unrelated `MNEMO_*`/`SHELL`/`PATH`/`WEBVIEW2_*` vars) or indirectly (the only `build_client` call sites — config_io.rs:161, memory_maintenance.rs:269, settings.rs:254, main.rs:1069 — are production code; the test modules in those files are DTO/wire-shape/parse tests that never build clients).

Fixture APIs all verified against the library: `Config` derives Default with pub `endpoints: Vec<Endpoint>` (src/config/mod.rs:46) and pub `keys: KeyStore` (mod.rs:50); `KeyStore::insert(String, String)` (src/config/keys.rs:76); `Endpoint::test_default()` is `#[cfg(any(test, feature = "test-support"))]` (src/config/endpoints.rs:522) and src-tauri's dev-dependency enables `test-support` (src-tauri/Cargo.toml:40, pre-existing — no Cargo.toml change was needed); `IpcError.message` is pub (error.rs:34) and `From<String>` exists (error.rs:47). Edition 2021 (Cargo.toml:4) — `set_var`/`remove_var` are safe calls, no `unsafe` needed.

### Constraint 4 — Constitution: VERIFIED ✓

- **Doc comments:** both helpers documented (models.rs:21-29, 69-76) — the resolution chain with rationale (picker sync with unsaved edits), the delegation rationale (app crate stays off reqwest; no key leaked in errors), and the shared-by-both-commands drift-prevention purpose. Command doc comments retained; `list_vision_models`'s "Resolution is identical to [`list_models`]" (models.rs:149) is now literally true via the shared helper.
- **Multi-platform neutrality:** `std::env::var`/`set_var`/`remove_var` are platform-neutral std APIs; no Windows-only code, no `cfg(windows)` additions.
- **Documentation sync:** README.md and PLAN.md contain no mention of list_models/list_vision_models/kind_wire (all .md hits are `.coding/` historical records) — internal refactor, nothing goes stale.
- **Warning-free plausibility:** no `#[allow]` added, no dead code (`kind_wire` fully deleted; `endpoint_kind_wire` still used at settings.rs:675 + models.rs:46), no unused imports, both helpers called.

### Constraint 5 — Test status

Could not re-run tests (read-only reviewer). Source-level verification supports the claimed green run: every API the new code and tests use exists with matching signatures, the env calls are edition-2021-safe, and nothing in the diff would trip `deny(warnings)`.

### Findings

**LOW 1 — Unlocked env reads in the two saved-endpoint tests race the env tests' mutations (Unix-only, theoretical).**
`override_wins_over_saved` (models.rs:200) and `saved_endpoint_fallback` (models.rs:228) call `resolve_endpoint_credentials`, which always computes `env_key` (models.rs:52-60) — reading `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_AUTH_TOKEN` — without holding `ENV_TEST_LOCK`, while `kind_aware_env_fallback_anthropic_vs_openai` / `dummy_terminal_fallback` may concurrently set/remove those same vars under the lock. On Windows these are process-safe OS calls; on Unix (macOS is a supported platform per the multi-platform rule) `std::env::var` does not take the internal lock that `set_var`/`remove_var` take, so the read is technically unsynchronized. Practical impact is nil — the read values are discarded in those tests (override/stored key win), so no assertion can flip — and this exactly mirrors the established client_factory.rs precedent (its non-env stored-key tests run `resolve_api_key`'s env reads unlocked alongside its env test). Optional one-line hardening each: acquire `ENV_TEST_LOCK` in the two tests as well. Fix or consciously accept the precedent.

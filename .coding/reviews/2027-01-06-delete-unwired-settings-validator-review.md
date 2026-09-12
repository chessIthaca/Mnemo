## Verdict: FINDINGS (0 high, 1 low)

The deletion is complete and surgical: the dead family is gone from every `.rs` source, everything live survives byte-unchanged, and the two ported tests genuinely pin the live save path (one is strictly stronger than its dead twin). One LOW documentation finding: a superseded knowledge spec still names the deleted `SettingsPatch`/`apply_settings_patch` as required plumbing.

### 1. Deletion completeness — VERIFIED

Repo-wide search for `validate_settings_patch|apply_settings_patch|apply_models_patch|SettingsPatch|ModelsPatch` (2315 files walked, 95 hits in 24 files): the only `.rs` hits are the LIVE `validate_and_apply_settings_patch` — definition (src/config/settings_dto.rs:258), callers (src-tauri/src/ipc/settings.rs:15 import + :822 call; settings_dto.rs test calls :644/:664/:670/:675/:700/:716/:723), and the module-doc mention (src/config/patch.rs:13). Those match via the `apply_settings_patch` substring inside the live name — not the dead family. `src/config/mod.rs:21` holds only `pub mod patch;` (no re-exports to go stale). All remaining hits are `.coding/` history (backlog items, completed plan files, past review reports) — correct to keep as history.

### 2. Nothing live removed — VERIFIED

patch.rs retains exactly the four live items plus their full test suite: `validate_endpoint` (:30), `validate_endpoint_set` (:147), `apply_endpoints` (:213), `parse_safety_mode` (:288 — still called live from settings_dto.rs:338). 24 tests remain, all targeting those four fns (read the full 915-line file; no test references any deleted symbol). The settings_dto.rs diff is a pure append — `@@ -649,4 +649,84 @@` adds 80 lines inside `mod tests` only; the live validation+apply body (:258-624) is unchanged.

### 3. Ported tests genuinely exercise the live path — VERIFIED

- `proxy_cache_ceiling_validation_rejects_tiny_and_accepts_disable_or_sane` (settings_dto.rs:654-677): `Some(1024)` → Err containing "proxy_cache_ceiling_tokens" matches live validation :270-276 (`ceiling > 0 && ceiling < 65_536`); `Some(340_000)` → Ok; `Some(0)` → Ok **and** `next.general.context.proxy_cache_ceiling_tokens == None` matches live apply :481-483 (`if ceiling == 0 { None } else { Some(ceiling) }`). Strictly stronger than the dead twin — it also pins the apply-side clear-to-None.
- `models_patch_double_option_semantics` (:680-731): preset `planning = Some(ModelRef{ep, old})`; `Some(Some(ModelRefDto{ep2, m2}))` → replaced (live apply :564-566, `p.as_ref().map(to_ref)`); `Some(None)` → None; outer None (`ModelsConfigDto::default()`) → unchanged. The endpoint-ref validation (:403-435) is correctly skipped — `Config::default()` has no endpoints so `has_endpoints` is false, and "ep2"/"m2" pass the non-empty checks (:406-411). Assertions match live semantics exactly, and the test runs validation→apply end-to-end — stronger than the dead twin's direct `apply_models_patch` call.
- Types in scope: `use super::*` (:628) + module import `use crate::config::{Config, ModelRef};` (:20); the DTOs are defined in-file. Compiles by inspection; the reported green `cargo test` confirms.

### 4. Import hygiene — VERIFIED

- Module import (patch.rs:17) `{Config, Endpoint, EndpointKind, KeyStore, SafetyMode}`: all five used in non-test code (Config/Endpoint/KeyStore in `apply_endpoints` :213-285, EndpointKind :60-62, SafetyMode :288-293).
- `PricingEntry` moved to the tests-module import (:303): correct — after the deletion its only user is the `apply_endpoints_preserves_pricing_and_projects` test (:687). A module-level import would be unused in the non-cfg(test) compilation → `deny(warnings)` failure. The stated reasoning holds.
- `ModelRef` dropped from the tests import: correct — its only tests-module user was the deleted `apply_models_patch_double_option_semantics` test; no `ModelRef` use remains anywhere in patch.rs.
- `EmbeddingModel`/`VisionModel`/`EMBEDDING_MODEL_SENTINEL_HASH` dropped from the module import: correct — only the deleted family used them.
- Module doc (:5-13): accurate — states the remaining scope (endpoint-save path + safety-mode parsing) and points the non-endpoint save path at settings_dto.rs (`validate_and_apply_settings_patch`). No stale F2/wire-struct claims remain.

### 5. Documentation sync — one LOW finding

README.md / PLAN.md: zero references (neither appears in the search hit list). The decision-file pointer update (.coding/knowledge/decision/2026-12-21-vendor-reasoning-retention-policy-driven-340k-pr.md) is correct — it now cites `settings_dto.rs (proxy_cache_ceiling_validation_rejects_tiny_and_accepts_disable_or_sane)`, matching the ported test's real name and file. See LOW 1 for the one gap.

### 6. Multi-platform neutrality — VERIFIED

Pure deletion + two portable tests; no platform-specific code introduced or touched.

### Findings

**LOW 1 — stale plumbing map in a knowledge spec names the deleted family as live code.**
`.coding/knowledge/spec/2027-01-04-chat-readability-settings-thread-line-prose-cap.md:18` — the plumbing sentence reads: "save path needs BOTH SettingsSaveDto + validate_and_apply_settings_patch (settings_dto.rs) AND SettingsPatch + apply_settings_patch (patch.rs) — miss either and saves silently drop the fields". After this change `SettingsPatch`/`apply_settings_patch` no longer exist and the "miss either" warning is moot (there is exactly one save path now). Mitigations: the file's frontmatter is `status = "superseded"` and memory search excludes superseded records, so it is near-history; and a contributor following it would hit an immediate compile error, not a silent trap. Still, it is present-tense contributor guidance in the mergeable knowledge sidecar and the one doc a future "add a [ui] setting" task might grep up. Fix (one line): amend the sentence to note the patch.rs twin was deleted (plan 8684dc0e, quality review HIGH 3) and the save path is settings_dto.rs only. Not blocking.

### Test status

Read-only reviewer — cannot re-run tests. Source-level verification is fully consistent with the reported green runs (root: 2004 passed / 0 failed / 4 ignored + 16 doc-tests, exit=0; src-tauri: 196 + 4 passed, 0 failed, exit=0; zero warnings under `#![deny(warnings)]`): −2 dead tests, +2 ported, net count unchanged, and both ported tests reference only live symbols.

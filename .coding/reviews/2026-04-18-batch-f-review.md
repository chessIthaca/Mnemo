# Batch F (partial) — F2 settings-policy→lib extraction

**Branch:** `feat/deep-review-f-extractions`
**Scope:** F2 PARTIAL — extract the cleanest, most testable pieces of the
settings-policy→lib move to a new `src/config/patch.rs` lib module. F1
(EventCoordinator) and the F2 apply-block (DTO→SettingsPatch conversion) are
DEFERRED with written justification.

**Files reviewed (uncommitted diff vs HEAD):**
- `src/config/patch.rs` (NEW — 598 lines, 6 pub fns + 2 pub structs + 14 tests)
- `src/config/mod.rs` (+1: `pub mod patch;`)
- `src-tauri/src/ipc/settings.rs` (−146: `into_endpoint`, `save_endpoints`
  cross-endpoint validation + config-build, `parse_safety_mode` now delegate)
- `.coding/plans/e342c141-…md` (bookkeeping: Batch E marked done)

---

## Verdict

**The WIRED extractions are behavior-identical and correct.** The four lib fns
the adapter actually calls (`validate_endpoint`, `validate_endpoint_set`,
`apply_endpoints`, `parse_safety_mode`) faithfully reproduce the old inline
logic, the call sites are type-correct, and the 14 lib tests exercise the
extracted code directly. No security regression — the acceptance sets and
validation ordering are unchanged.

**The UNWIRED scaffolding** (`validate_settings_patch`, `apply_settings_patch`,
`SettingsPatch`, `ModelsPatch`) is *not* a `deny(warnings)` violation (it is
`pub` in a lib crate, so the compiler treats it as public API, not dead code),
**but it is NOT a faithful extraction of the adapter's `save_settings`** — it
has several latent discrepancies that will cause regressions when F2-full wires
it in. These must be fixed before wiring (medium severity now, since they
don't affect runtime until wired).

Findings below are grouped by severity.

---

## CORRECTNESS — wired extractions (✅ behavior-identical)

### `validate_endpoint` vs old `EndpointDto::into_endpoint` — ✅ identical
`patch.rs:26-76` reproduces the old `into_endpoint` (`settings.rs` old lines
78-93 now delegate). Name trim + empty check, base_url empty + trailing-slash
check, exact-match `kind` ("openai"/"local", case-sensitive — Debug form
rejected), empty-model dropping, and field-by-field `Endpoint` construction all
match. The adapter passes `&self.name`/`&self.kind`/`&self.base_url`
(`&String`→`&str` coercion), `self.models` (moved `Vec<String>`), and the
scalar fields by value — all match the signature. ✓

### `validate_endpoint_set` vs old inline cross-endpoint validation — ✅ identical
`patch.rs:81-140` reproduces the old `save_endpoints` block (old settings.rs
~150-196). Ordering is preserved exactly:
1. `default_provider` ref check,
2. `default_model` host lookup (`default_provider`'s endpoint, else first) +
   in-models check + `old_default_model` leniency + no-endpoints error,
3. resolvable-model check (`default_model` non-empty → resolvable, else
   endpoint must have ≥1 model).

The `Option<&str>` (lib) vs `&Option<String>` (old) difference is invisible:
`e.name == dp` and `old_default_model == Some(dm)` compare the same string
contents. The adapter call (`settings.rs:152-157`) passes
`default_provider.as_deref()` / `default_model.as_deref()` /
`old_default_model.as_deref()` — types match. The `?` converts `String`→
`IpcError` via the existing `From<String> for IpcError` (same path the old
`.into()` used). ✓

### `apply_endpoints` vs old inline config-build — ✅ identical
`patch.rs:146-171` reproduces the old block: `general` clone +
`default_provider`/`default_model` set, `KeyStore` built by iterating `built`
(keeping only keys for surviving endpoints, dropping empties), `pricing` +
`projects` carried over. The adapter (`settings.rs:160-169`) clones
`default_provider`/`default_model` to pass by value (matching the old
`.clone()`), and `built` is moved after the `validate_endpoint_set` borrow
ends. ✓

### `parse_safety_mode` — ✅ identical acceptance
`patch.rs:393-403` accepts the same 4 kebab-case variants as the old
`settings.rs:519` fn. See the error-message note below for the one
(cosmetic) difference.

### `apply_models_patch` double-Option semantics — ✅ correct
`patch.rs:371-390`: `if let Some(p) = &patch.planning { models.planning = p.clone() }`
where `patch.planning: Option<Option<ModelRef>>`. Outer `None`→skip (keep),
`Some(None)`→assign `None` (clear), `Some(Some(m))`→assign `Some(m)` (set).
`skill` replaces the map wholesale. The test
`apply_models_patch_double_option_semantics` (`patch.rs:578-597`) exercises
set/clear/keep for `planning`. ✓

---

## BUGS — medium (latent, in UNWIRED scaffolding)

These fns are `pub` in `myharness::config::patch` and therefore **not** dead
code under `#![deny(warnings)]` (pub items in a lib crate are public API; the
compiler does not flag them). So there is **no constitution violation** and the
build stays warning-free. **However**, they are not faithful extractions of the
adapter's `save_settings` — when F2-full wires them in, these will regress
behavior. Fix them now (or gate the wiring on fixing them).

### B1. `validate_settings_patch` does not lowercase `theme` before matching — MEDIUM
- **Lib:** `patch.rs:192-198` — `if !matches!(theme.as_str(), "dark"|"light"|"system")` (case-sensitive).
- **Adapter:** `settings.rs:798-806` — `let t = theme.to_ascii_lowercase(); if t != "dark" && …`.
- **Impact:** the adapter currently accepts `"Dark"`/`"SYSTEM"` (lowercased
  first); the lib fn would *reject* them. On wiring, valid user input regresses.

### B2. `validate_settings_patch` skips `default_provider` ref validation — MEDIUM
- **Lib:** `patch.rs:179-270` — no `default_provider` check at all.
- **Adapter:** `settings.rs:840-850` — rejects `default_provider` not matching
  any endpoint (when not clearing + endpoints exist).
- **Impact:** on wiring, a typo'd `default_provider` slips through validation
  and only surfaces at runtime resolution. This is a validation-behavior
  regression (a bad config that is currently rejected would be accepted).

### B3. `validate_settings_patch` vision/embedding/[models] checks skip empty + trim — MEDIUM
- **Lib:** `patch.rs:206-254` — only checks `current_endpoints.iter().any(|e| e.name == vm.endpoint)`; no empty-endpoint/empty-model check, no trim.
- **Adapter:** `settings.rs:811-820` (vision empty), `851-861` (vision trim+ref), `862-878` (embedding empty+trim+ref), `880-912` ([models] empty+trim+ref).
- **Impact:** on wiring, an empty `vision_model.endpoint` (currently rejected
  with a specific message) passes the lib validate step; untrimmed refs that
  don't exactly match an endpoint name also behave differently.

### B4. `apply_settings_patch` does not trim vision/embedding/pricing — MEDIUM
- **Lib:** `patch.rs:300-302` (`Some(vm.clone())`), `307-309` (`Some(em.clone())`), `345-349` (`pricing = rows.clone()`).
- **Adapter:** `settings.rs:935-940` (vision trim), `943-948` (embedding trim), `1004-1015` (pricing `model: p.model.trim()`).
- **Impact:** on wiring, leading/trailing whitespace in vision/embedding model
  fields and pricing model names would be persisted untrimmed (today it is
  trimmed). The trim responsibility needs to live somewhere in the future
  DTO→SettingsPatch conversion layer; as written, the lib apply fn drops it.

### B5. `apply_settings_patch` theme uses `to_lowercase()` not `to_ascii_lowercase()` — LOW
- **Lib:** `patch.rs:328` — `theme.to_lowercase()`.
- **Adapter:** `settings.rs:965` — `theme.to_ascii_lowercase()`.
- **Impact:** for valid themes (pure ASCII) identical; differs only for
  non-ASCII input that already failed validation. Cosmetic, but not
  byte-identical to the adapter.

---

## LOW / INFORMATIONAL

### L1. `parse_safety_mode` error message text changed — LOW (not a regression)
- **Lib:** `patch.rs:399-401` — `"unknown safety mode '{other}' (expected approve-each-action, …)"`.
- **Old adapter:** `"unknown safety mode '{other}'"`.
- **Impact:** the accepted variants are identical (no security/behavior
  change); only the user-facing error string is more informative. No test pins
  the old text (the lib test checks `contains("unknown safety mode")`, the
  adapter test checks `is_err()`). Benign improvement, but strictly speaking
  not a "pure refactor" for the error path. Acceptable as-is.

### L2. `validate_endpoint_set` doc comment is inaccurate — LOW
- **Lib:** `patch.rs:78-80` — doc says "Cross-endpoint validation: unique
  names, default_provider references an endpoint, …". The fn does **not**
  check unique names — the adapter does that separately in its `seen` HashSet
  loop (`settings.rs:142-149`), and the adapter's inline comment correctly
  notes "unique names already checked above". The lib doc should drop "unique
  names" (or the fn should check it). Doc/comment mismatch only.

### L3. Test `apply_endpoints_preserves_pricing_and_projects` doesn't assert projects — LOW
- **Lib:** `patch.rs:536-551` — the test name claims "preserves pricing AND
  projects" but never seeds `current.projects` and never asserts on
  `new.projects`. The `apply_endpoints` fn does preserve projects
  (`projects: current.projects.clone()`), so the logic is correct — only the
  test's claim is unverified. Add a seeded project + `assert_eq!` on
  `new.projects`.

### L4. No test for deleted-endpoint key dropping — LOW
- `apply_endpoints` drops keys for endpoints no longer in `built` (it iterates
  `built`, not `api_keys`). `apply_endpoints_drops_empty_keys` (`patch.rs:554-561`)
  only covers the empty-key case. The deleted-endpoint case is correct but
  untested. Minor coverage gap.

### L5. No lib test for empty `base_url` — LOW
- `validate_endpoint` rejects empty `base_url` (`patch.rs:41-43`) but the lib
  test suite only covers the missing-trailing-slash case
  (`validate_endpoint_rejects_base_url_without_slash`). The empty case is
  covered by the adapter's `endpoint_dto_tests::rejects_empty_base_url`, so
  it's exercised, just not in the lib.

---

## CONSTITUTION COMPLIANCE

- **`#![deny(warnings)]` / dead code:** ✅ No violation. All four unwired items
  (`validate_settings_patch`, `apply_settings_patch`, `SettingsPatch`,
  `ModelsPatch`) are `pub` in `pub mod patch` (reachable as
  `myharness::config::patch::*`), so the compiler treats them as public API
  and does not emit `dead_code`. `apply_models_patch` is additionally called
  by the `apply_models_patch_double_option_semantics` test. All wired fns are
  called by the adapter. No `#[allow(...)]` anywhere. (Caveat: the unwired fns
  have the latent bugs B1-B5 above — not a build issue, but fix before wiring.)
- **Doc comments on all pub items:** ✅ Every `pub fn` and `pub struct` in
  `patch.rs` has a `///` doc comment; the module has `//!`. (L2 is a doc
  *accuracy* issue, not a missing-doc issue.)
- **Unused imports:** ✅ None. All 10 names in the `use crate::config::{…}`
  block (`Config`, `EmbeddingModel`, `Endpoint`, `EndpointKind`, `KeyStore`,
  `ModelRef`, `PricingEntry`, `SafetyMode`, `VisionModel`,
  `EMBEDDING_MODEL_SENTINEL_HASH`) and `std::collections::HashMap` are used.
  The adapter's `use myharness::config::{Endpoint, EndpointKind, SafetyMode}`
  is still used (`endpoint_kind_wire`, `endpoint_wire`, etc.).
- **Type mismatches at call sites:** ✅ None. All four adapter call sites
  (`into_endpoint`, `validate_endpoint_set`, `apply_endpoints`,
  `parse_safety_mode`) match the lib signatures, including the
  `&MutexGuard<Config>` → `&Config` deref coercion at the `apply_endpoints`
  call (same pattern the old code used for `current.general.clone()`).
- **Line endings:** ✅ No CRLF issues on the Rust files. The git CRLF warning
  is only on the `.coding/plans/*.md` bookkeeping file (not code).
- **Tests that don't exercise the fix:** ✅ None. All 14 lib tests call the
  extracted fns directly and assert on their behavior (not no-ops). (L3/L4/L5
  are coverage gaps, not non-exercising tests.)

---

## SECURITY

- **No validation weakening.** The wired extractions check the same things in
  the same order with the same acceptance sets. `validate_endpoint` still
  rejects empty names, non-`/`-suffixed base_urls, and unknown kinds.
  `validate_endpoint_set` still rejects dangling `default_provider`/`default_model`
  refs and model-less default endpoints. `parse_safety_mode` still accepts only
  the 4 known variants. No bad config can slip through that couldn't before.
- **Key handling unchanged.** `apply_endpoints` still drops empty keys and keys
  for deleted endpoints (iterates `built`, not `api_keys`); keys never appear in
  any error message or `Debug` output.
- **Latent (B2):** the *unwired* `validate_settings_patch` would let a bad
  `default_provider` through — but it is not called, so no current exposure.

---

## RECOMMENDED ACTIONS (before merging F2-partial)

1. **(Medium, latent — fix before F2-full wiring)** Reconcile
   `validate_settings_patch` + `apply_settings_patch` with the adapter's
   `save_settings` (B1-B5): lowercase theme in validate, add `default_provider`
   ref check, add empty+trim checks for vision/embedding/[models], trim
   vision/embedding/pricing in apply, use `to_ascii_lowercase` for theme. Either
   fix now or add a `// TODO(F2-full): reconcile with save_settings — see review
   B1-B5` marker so the next batch doesn't wire them in blind.
2. **(Low)** Fix the `validate_endpoint_set` doc to drop "unique names" (L2).
3. **(Low)** Strengthen `apply_endpoints_preserves_pricing_and_projects` to
   actually seed + assert projects (L3); optionally add a deleted-endpoint key
   test (L4) and a lib test for empty `base_url` (L5).

The core F2-partial extraction is sound and safe to merge; the medium findings
are all in intentionally-deferred, unwired scaffolding.

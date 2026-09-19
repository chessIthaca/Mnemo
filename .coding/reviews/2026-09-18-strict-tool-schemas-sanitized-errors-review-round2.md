## Verdict: PASS

Round-2 verification of commit `b7a7ba8` (tip of `wt/mnemo`; working tree clean — `git diff HEAD` empty, so the files read are exactly the commit) against the three round-1 findings in `.coding/reviews/2026-09-18-strict-tool-schemas-sanitized-errors-review.md`. **All three findings are verified fixed in the committed code; no new findings.**

## HIGH 1 — strict-mode null vs non-Option `#[serde(default)]` fields: FIXED

**The helper.** `null_to_default` at src/tool/mod.rs:164-184: `Option::<T>::deserialize(de)` then `unwrap_or_default()`. Semantics verified: a present value deserializes normally; `null` → `None` → `Default::default()`; an absent key never reaches `deserialize_with` (serde invokes it only for present keys), so the paired `#[serde(default)]` still fires on absence — all three cases covered. The doc comment (mod.rs:164-176) states the contract and cites review HIGH 1.

**Every round-1 inventory row covered:**

| Round-1 row | Fix site (committed code) |
|---|---|
| file_edit `old_string` | file_edit.rs:48-49 |
| file_edit `new_string` | file_edit.rs:53-54 |
| file_edit `replace_all` | file_edit.rs:55-56 |
| file_edit `use_regex` | file_edit.rs:60-61 |
| file_edit `fuzzy_whitespace` | file_edit.rs:85-86 |
| file_edit `append` | file_edit.rs:107-108 |
| create_plan `context` | plan.rs:78-79 |
| create_plan `kind` | plan.rs:94-95 |
| update_plan `append` | plan.rs:453-454 |
| StepInput::Map `body` | plan.rs:139-140 (`String` → `Option<String>`) |

**Regression tests exercise the real strict-mode shape:**
- `strict_mode_shape_all_keys_null_optionals_deserializes` (file_edit.rs:1452-1484): all 11 keys present, every schema-optional set to `null`, line-range mode (`start_line: 1, end_line: 2`); asserts each null landed on its default (`old_string == ""`, `!replace_all`, `edits.is_none()`, …). Fails on the pre-fix struct (`invalid type: null, expected a string`).
- Same-named test in plan.rs:2283-2324: create_plan with all 8 keys present (`context`/`kind`/`bug`/`branch`/`base` null, plus a map step `{"header": "h", "body": null}`) asserting `context == ""`, `kind == PlanKind::Implementation`, `body.is_none()`; then update_plan with all 7 keys null asserting `!append` and `steps.is_none()`. Both halves fail pre-fix.

**No other STRICT_TOOL has a non-Option optional field** — all nine members of `STRICT_TOOLS` (strict.rs:250-260) checked, not just the round-1 spot-check list:
- `file_write` (file_write.rs:27-30): `path`/`content` both required; `mode` is read from raw JSON via `args.get("mode").and_then(as_str)` (file_write.rs:57-61) — `null` falls to the overwrite default, safe.
- `file_append` (file_append.rs:24-27): `path`/`content` both required.
- `convert_line_endings` (convert_line_endings.rs:27-30): `path`/`to` both required.
- `complete_step` (plan.rs:415-434): `step_index`/`detailed_step_index`/`plan_id` all `Option`.
- `abandon_plan` (plan.rs:1521-1524): empty `properties`; `execute` ignores `_args` (plan.rs:1535).
- `finish` (plan.rs:1610-1619): `review_report` required, `regression_test` `Option`.
- `create_plan` `steps: Vec<StepInput>` is non-Option but sits in the schema's `required: ["title", "goal", "steps"]` (plan.rs:564), so normalization never widens it — no `null` can reach it.

**`to_text` None-body parity:** plan.rs:158-165 — `body.unwrap_or_default()`; an empty body renders `**{header}**` alone, identical to the old absent/empty-body behavior. `Text` steps still pass through verbatim (plan.rs:157).

**Module doc amended:** strict.rs:32-38 now states the real contract — the RECEIVING side must accept `null`; `Option<T>` natively, non-Option optionals via `null_to_default`; a bare `#[serde(default)]` "would reject the very `null` strict mode forces". The round-1 wrong assumption ("execute deserialization is untouched") is gone.

## LOW 1 — sanitizer fallback leaking serde vocabulary for untagged-enum mismatches: FIXED

Dedicated branch at error_message.rs:139-145: `data did not match any variant of untagged enum` → "an item in a list parameter does not match its allowed forms. Please check each list item against the tool's schema and try again." — no serde vocabulary, actionable, names the tool. Placement is correct: after the more-specific prefixes (an untagged failure never starts with `invalid type:`/`unknown variant`/`invalid value:`) and before the fallback. Test `untagged_enum_mismatch_points_at_list_items` (error_message.rs:395-418) generates a REAL untagged failure via `serde_json::from_value::<Vec<Item>>(json!([42]))` (plus a sanity positive case that also keeps the dead-code lint honest) and asserts the exact message — pinned against serde's actual vocabulary, per the module's own test convention. The round-1 suggestion's optional extra (mapping the enum name back to the parameter) was explicitly optional and is reasonably omitted.

## LOW 2 — anchor substrings inside values mis-splitting the parsers: FIXED

- `split_actual_expected` (error_message.rs:171-188) now splits on the LAST `", expected "` (`rfind`, :176) and trims serde's position suffix from the EXPECTED side only (:183-186 — a type name never contains " at ", and an " at " inside the actual value's text is untouched).
- The unknown-variant allowed-set extraction uses `rfind("expected ")` (error_message.rs:106-113), so a variant VALUE containing the anchor no longer mis-anchors it.
- Tests, both generating real serde errors: `anchor_substrings_inside_values_do_not_mis_split` (error_message.rs:421-435 — `count: "a, expected b"` asserts "must be an integer, not a string", no raw tail leak) and `anchor_substring_inside_variant_name_does_not_mis_extract` (:438-450 — `mode: "expected A"` asserts "(allowed: A or B)").
- Residual (theoretical, not a finding): an allowed-set MEMBER itself containing "expected " would still mis-anchor `rfind` — no enum in the codebase has such a variant (`PlanKind`, file_write `mode`, convert_line_endings `to`), and the round-1 finding asked only for the variant-value case, which is fixed.

## Also verified

1. **Commit contents:** 44 files per `git show --stat b7a7ba8`, including the round-1 review report (`.coding/reviews/2026-09-18-strict-tool-schemas-sanitized-errors-review.md`, 87 lines — the exact document this round verified against) and the plan file. Working tree clean at the commit, so file reads == committed state.
2. **Warning-free claim:** no `#[allow]` attributes anywhere in `src/` (the single search hit is prose inside a doc comment at src/agent/loop_impl.rs:641, not an attribute). Under `#![deny(warnings)]` the parent's green `cargo test` (2445 passed, 0 failed, per the commit message) proves zero warnings; I could not re-run it (read-only reviewer).
3. **No regressions from the fixes:** existing map-step tests intact and meaningful (plan.rs:3879-3920 — deserialization/bold-unwrap cases with `Some` bodies; plan.rs:3938-4024 — create_plan/update_plan map-step acceptance); file_edit's test helpers build `FileEditArgs` via struct literals (file_edit.rs:1487-1541), unaffected by the serde attributes; the sanitizer's pre-existing tests (missing/unknown field, invalid type, unknown variant, syntax, never-carries-vocabulary) are unchanged alongside the new ones.

## Notes (non-findings)

- Round 1's fix direction suggested a strict-shape regression test for *every* STRICT_TOOL; the implemented tests cover the three tools that had the defect (and fail without the fix — the actual regression-test bar). The other six tools' safety is their all-Option/all-required shape, verified field-by-field above; a future non-Option optional field added to any of them would need its own null-tolerance, but that is future-proofing, not a defect in this change.

## Verdict rationale

All three round-1 findings are fixed exactly as claimed, each pinned by a regression test that generates a real serde error and fails without the fix. The HIGH 1 inventory is fully covered with no residual non-Option optional field anywhere in the nine-tool STRICT_TOOLS surface (including the `steps: Vec<StepInput>` near-miss, which is schema-required and therefore never widened). The two LOW fixes close the reported cases with correct last-anchor semantics. No new defects found in the fix delta.

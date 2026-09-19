+++
title = "null-stringify transport defense — optional-property drop at the dispatch seam"
created = "2027-01-11"
+++

The transport stringifies JSON null for string/enum params (and auto-fills omitted optional properties with it) into the literal string "null" — the model's "no value" arrived as a value. Since plan 50f36b1e (commit 1ea2940 on wt/mnemo, 2027-01-24, backlog 9118714a) the harness defends centrally:

- `drop_stringified_nulls(schema, args)` (src/tool/mod.rs, next to null_to_default): recursive walk mirroring normalize_in_place (anyOf/oneOf/allOf branches, items single + draft-04 tuple, nested properties) — drops the exact string "null" from properties absent from that schema level's `required` list. Required fields keep it (genuine error, circuit-breaker territory); JSON null untouched (Option/null_to_default handle it); unknown properties untouched. Keyed on the RAW advertised schema (tool.schema().parameters), not the strict-normalized one.
- Wired into execute_tool_call (src/agent/dispatch.rs): the tool lookup was moved before ParsedToolCall construction; args are sanitized before approval, safety rules, previews, steering, and the tool read them; raw tc.arguments stays untouched (UI fidelity).
- Victims' advertised schemas are nullable (["string","null"], enums keep their list — the normalize_for_strict widened form, byte-stable under is_nullable): backlog_status.status/note, update_plan.title/goal/context/regression_test, create_plan.bug/branch/base/context/kind, file_edit old_string/new_string. spawn_agent.model (backlog 3e6f7887) was the one-field precedent this generalized.
- backlog_status accepts note-only updates (status OR deferred OR note required; deferred+note now lands the note instead of silently dropping it) — no more done→pending→done requeue dance.
- Accepted exception (documented in the defense's doc): file_edit anchors/replacements of exactly "null" are dropped — the old_string side self-corrects via the empty-old_string error hint (batch mode / use_regex); the new_string side is a visible-in-diff wrong write (the schema description carries the workaround clause).
- Budget ceilings (dated raises in src/agent/factory.rs): Executing 32_300, PlanFrozen 33_600, Reviewing 27_500.
- Scope decision: the full ~50-field advertised-nullability sweep across all tools deliberately deferred — the central defense covers every tool's optional fields for the stringified form; a blind sweep risks advertising null for plain #[serde(default)] String fields whose deserializers reject JSON null.

Regression tests: dispatch_drops_stringified_nulls_from_optional_params (the seam, end-to-end), 5 unit tests in src/tool/mod.rs, per-victim composition tests (status_tool_stringified_null_status_updates_note_only, update_plan_stringified_null_fields_leave_them_unchanged, create_plan_stringified_null_branch_skips_branch_prep, batch_mode_with_stringified_null_old_new_applies_the_batch), per-tool nullable-schema assertions. Reviews: round1 FINDINGS (2 low, fixed) → round2 FINDINGS (1 low, fixed) → round3 PASS (.coding/reviews/2026-09-19-plan-50f36b1e-null-stringify-defense-review-round3.md).

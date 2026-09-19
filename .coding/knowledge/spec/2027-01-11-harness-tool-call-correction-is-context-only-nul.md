+++
title = "harness tool-call correction is context-only; \"null\" is the unset model sentinel (plan 995436c2)"
created = "2027-01-11"
+++

SPEC: harness tool-call correction is context-only; "null" is the unset model sentinel (plan 995436c2, commit b94ce37 on wt/mnemo). (1) The repeat-failure circuit breaker's corrective schema reminder rides the provider-facing messages ONLY — no AgentEvent::Error is emitted (no red transcript box, no activity-log row, no doom-streak extension); the LLM still sees the correction. (2) spawn_agent's `model` param is nullable (["string","null"]); the literal string "null" (trimmed) counts as absent/unset at the dispatch reviewer gate AND in the tool's forced-model resolution — a strict-schema transport stringifies JSON null for string properties, and "null" is never a real model id; a dropped "null" is named in the tool's success output (never silent), while JSON null is an explicit unset with no note. Regression tests: repeat_failure_injects_schema_correction_on_second_identical_error, dispatch_treats_null_model_as_absent_for_reviewer_spawns, null_model_string_counts_as_unset.

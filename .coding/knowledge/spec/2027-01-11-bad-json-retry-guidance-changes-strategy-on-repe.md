+++
title = "bad-JSON retry guidance changes strategy on repeat (per-tool remedy)"
created = "2027-01-11"
+++

How the bad-JSON retry guidance works (plan 481be538, backlog 38040f12, commit 4be19df on wt/macos-fix):

FIRST failure per (tool, error) signature: handle_bad_json (src/agent/turn.rs) pushes the constant schema-oriented text from bad_json_retry_message() — "re-read the tool's schema, rewrite the COMPLETE call" — correct for the dropped-required-field class (plan 4fa222cc).

REPEAT (same tool name + byte-identical error text, detected via repeated_tool_failure scanning BEFORE the current result is pushed): the message is instead built by repeated_bad_json_correction(tool_name, failed_content) — harness-attributed (HARNESS_CORRECTION_PREFIX), Tool-role, context-only (no Error event), naming a per-tool different formulation: search/search_read → (1) literal:true with the plain text, (2) metacharacter-free character class e.g. [(][?][<] for the literal `(?<`, (3) substring/glob narrowing; file_write/file_edit → chunk the body; default → rebuild the argument object from scratch. From the second repeat on the guidance is IDEMPOTENT by design — same stuck state, same remedy; what must never happen is reversion to the first-failure schema advice.

Invariants: the scan-before-push ordering (a match is always against a PRIOR failure); the first-failure text stays byte-identical to the pre-2027-02-05 string; bad_json_count / MAX_BAD_JSON_RETRIES=8 backstop untouched; the correction embeds only harness-controlled strings.

The search and search_read tool descriptions advertise literal:true as the escape hatch for metacharacter/backslash-dense text (ESCAPE-HATCH note + RECOMMENDED param wording + inline example). Budget ceilings: Planning/Complete 18_700, Executing 33_900, PlanFrozen 35_200, ExecutingResearch 28_000, Reviewing 29_200 (measured+headroom raises, factory.rs dated comments).

Regression test: src/agent/tests.rs::repeated_bad_json_for_same_tool_changes_strategy (asserts [0]≠[1], [0]≠[2], [1]==[2], harness attribution, `literal`, and [(][?][<]).

+++
title = "prompt-block placement — messages[0] is the cache prefix; volatile tail + footer ride last (user role on DeepSeek/Local)"
created = "2027-01-11"
+++

WHERE EVERY PROMPT BLOCK LIVES (final architecture, plan 506b85e2):
- messages[0] = the STABLE HEAD (`prompt::build_stable_head`: preamble + workflow lifecycle + APP_RULES + tool discipline + memory-record block + tool strategy + constitution + hidden tool-group index + session primer). Byte-stable across progress bumps; changes only on an agent.md edit (mtime-guarded), a tool-group reveal, or a new session. This is the prefix every prompt cache keys on — nothing per-step/per-turn may enter it.
- messages[1..n-2] = the conversation history.
- the LAST TWO messages = the transient pair: the VOLATILE TAIL (`prompt::build_volatile_tail`: workflow section with CURRENT PLAN/GOAL/PROGRESS/CURRENT STEP, skill goal, subagent notes, CAPABILITIES, RECALLED MEMORIES) + the byte-stable `prompt::CONTEXT_FOOTER`. Pushed by `AgentLoop::install_system_messages` just before the request and popped (twice) right after it returns, before any `?` or early return, so they never persist.

ROLE RULE (the DeepSeek/Ollama carve-out): the pair is SYSTEM-role by default, but USER-role for vendors that cannot take trailing system blocks — `ProviderPolicy::tail_as_user_messages` (DeepSeek-vendor: its models echo trailing system blocks instead of answering — sentinel-mirror 5cf5469c + the 2027-01-11 exit-note loop) or `ProviderKind::Local` (Ollama renders a system message only in first position). Single decision point: `AgentLoop::tail_is_user_role` (src/agent/turn.rs), a pure `(kind, model)` lookup, also used by `AgentLoop::push_suggestion_message` for the three text-only suggestion-steer sites (turn.rs swap path, turn.rs summarization re-injection, runtime/agent.rs compact_context).

WHY (measured): folding the tail into messages[0] for those vendors made every `complete_step` a fresh prefix — DeepSeek hit 5.1-9.0% and re-billed 115-120K tokens per check-off (traces 2026-09-14 04:48:52-04:51:06 UTC; extractor `.coding/analysis/cache-hit-6-extract.py`, section B = HEAD k=0).

INVARIANTS TO PRESERVE: (1) no trailing SYSTEM message on `tail_as_user_messages`/Local vendors (context.rs summary messages sit at index 1 — never trailing; `sanitize_local_messages` is a Local-only backstop); (2) the FINAL message is always exactly `CONTEXT_FOOTER` (empirical cache law: the provider reuses the prefix only when the last message is byte-identical to the previous request's); (3) push/pop symmetry even on the error path; (4) the harness-side PrefixCache excludes the trailing pair by the FOOTER SENTINEL, not by role (`src/provider/openai/request.rs`, `trailing_transient`/`cacheable_len`) — a role-keyed count would pull the user-role pair into the cached prefix and rebuild every iteration.

REGRESSION TESTS: `agent::tests::deepseek_head_stays_byte_stable_across_plan_progress_bumps` (the cache property: two turns around a `complete_step`, messages[0] byte-identical + footer last), `deepseek_vendor_tail_rides_as_user_messages_no_trailing_system_block`, `local_provider_tail_rides_as_user_messages_no_trailing_system_block` (roles via `CapturingProvider::trailing_handle`).

PENDING LIVE ACCEPTANCE: a real DeepSeek session must show >=95% cached on check-off rows with no echo/exit-note loop (needs a rebuilt app). Rollback = revert the commit; flipping the flag alone only picks user vs system role, it does not restore the fold. Known gap: no test pins the user-tail variant of the reasoning re-add contract (`deepseek_tail_owner_keyed_when_system_follows_tool_results` covers the system-tail shape; the mechanism is role-blind as it counts ASSISTANT turns only).

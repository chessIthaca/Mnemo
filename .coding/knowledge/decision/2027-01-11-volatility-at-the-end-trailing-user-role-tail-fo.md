+++
title = "volatility at the end — trailing USER-role tail for DeepSeek/Local; messages[0] is the cache prefix"
created = "2027-01-11"
+++

Volatility (workflow PROGRESS / CURRENT STEP / skill goal, recalled memories) must never be written into messages[0]: that message is the prefix every provider's prompt cache keys on, so any change there makes the whole conversation a cache miss.

DeepSeek-vendor and Local/Ollama cannot receive trailing SYSTEM messages — DeepSeek mirrors them instead of answering (sentinel-mirror 5cf5469c + the 2027-01-11 exit-note loop), Ollama accepts a system message only in first position — so both get the volatile tail + the byte-stable CONTEXT_FOOTER as trailing USER messages. The request still ends the way a normal client turn does (a tool turn ending in a user message), the constant footer stays the final message (empirical cache law, .coding/analysis/cache-hit-2-report.md), and messages[0] stays byte-stable.

Single decision point: AgentLoop::tail_is_user_role (src/agent/turn.rs) — used by install_system_messages (the tail/footer role) and by AgentLoop::push_suggestion_message (the three text-only suggestion-steer sites: turn.rs handle_pending_swap, turn.rs summarization re-injection, runtime/agent.rs compact_context). Policy switch: ProviderPolicy::tail_as_user_messages (renamed from fold_volatile_tail, which became a lie once the fold was removed).

Measured cost of the old fold-into-head strategy: 5.1-9.0% cache hit and 115-120K tokens re-billed per complete_step (traces 2026-09-14 04:48:52-04:51:06 UTC; extractor .coding/analysis/cache-hit-6-extract.py, section B classified the breaks as HEAD k=0). Regression tests: agent::tests::deepseek_head_stays_byte_stable_across_plan_progress_bumps, deepseek_vendor_tail_rides_as_user_messages_no_trailing_system_block, local_provider_tail_rides_as_user_messages_no_trailing_system_block.

+++
title = "Mnemo2's LLM trace design and the lead's review changes"
created = "2026-11-02"
+++

Mnemo2's full LLM trace follows .coding/design/mnemo2-llm-trace.md (designed on Kimi, reviewed by the lead): a top-level Trace route between Cost and Graph plus the compact side-panel Trace tab sharing one data layer; request-scoped kernel attribution (TraceAttribution: actor_kind, agent_id, session_id, agent_name, worker_id, plan_id, plan_step, turn_id, purpose; plus run_id) passed explicitly via LlmRequestContext, never a mutable label on a shared provider; detail tabs Overview/Context/Response/Tools/Raw with a plain-language glossary; one Insights panel (Timing, Tokens, Cost over time) using the Cost tab's pricing; a metadata journal .coding/logs/trace-index.jsonl on by default, full bodies opt-in. Lead changes: failed-request bodies stay mirrored (redacted) as in Mnemo1; Mnemo2 ring capacity 128 (Mnemo1 32); export writes to .coding/logs/trace-exports/ with Copy/Reveal (no save dialog); 'Model calls' links on workers and lead turns open the filtered trace; the kernel step is high-risk with an independent audit.

Amended 2026-11-09: The hosting part of this decision no longer holds: there is no top-level Trace route and no compact tab. The full Trace view is a tab of the side panel on every main tab. The attribution, detail, Insights and journal decisions stand.

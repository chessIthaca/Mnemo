+++
title = "Mnemo1 routes a session's first prompt once, then only nudges; a manual model choice always wins"
created = "2026-11-03"
+++

With [general.laya] routing on, Mnemo1 classifies a main-agent session's first prompt (at least 3 words) once, using the shared question (src/agent/route_question.rs; targets cheap, routine and capable in [general.routing]).
- Enforce on: it pins the model immediately with pin_routed_provider. It never uses the deferred compact-first swap; if the target needs compaction it skips with 'needs_compaction'.
- Afterwards: only one RoutingNotice nudge (drift, or strikes from failed or repeated tool calls), published atomically with a reserved channel permit.
- Manual choice: every external model change (public set_provider or set_explicit_provider: IPC, console /model, headless engine and session) goes through note_external_model_change, which ends routing (Off{manual}), clears Laya's pin and logs a reroute. Internal switches (429 fallback, pending swaps, routed pin) use internal variants.
- Excluded: subagents and skip_turn_routing (Mnemo2) sessions. Resumed sessions are never routed. Clear resets routing; Clear and end log nudge_ignored once.
- Logs: ~/.mnemo/laya/training/session_routing.jsonl.

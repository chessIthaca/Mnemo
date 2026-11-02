+++
title = "team_status compact worker entries and the lead object"
created = "2027-01-11"
+++

Mnemo2's `team_status` tool (`src-tauri/src/mnemo2/team.rs`) answers `{"action":"team_status","result":[...],"lead":{...}}`.

- Each worker entry is compact by default: id, name, status, kind, tier, model (observed model_id, else the selected spec), plan_id, plan_step, branch, worktree, error, pending, findings (open only: id/title/severity/file/line), checks (name/status/eight-char tip), review {summary, tip} when reviewed, merge {commit} when merged, cost_micros, last_activity_ms. No brief, opening_prompt, inbox, transcript, or review evidence.
- `detail: true` returns the full WorkerRecord (transcripts stripped from the text via without_transcripts). `worker_id` / `plan_id` filtering works in both forms; an unknown worker_id is an error result.
- Top-level `lead`: {"kind":"external"|"in_app","attached":bool,"since_ms","idle_since_ms"}, read under one external-state lock. kind is "external" whenever an external session is attached OR idled out (mirrors ipc.rs `named = since_ms || idle_since_ms`); attached=false with idle_since_ms set means Claude Code was released for silence; kind in_app means no external session has taken the team.
- `result` stays an array and `ToolResult.data` keeps the same array shape, so existing readers (runtime.rs tests count it) keep working.
- Because a tools/call itself attaches the external lead before the tool runs, an MCP caller always sees lead.attached=true for that call; only the in-app lead path or an idle sweep can observe the other two states.
Implementing helpers: `TeamLeadStatus`, `worker_status_view`, `team_status_result`, `TeamRuntime::lead_status`.

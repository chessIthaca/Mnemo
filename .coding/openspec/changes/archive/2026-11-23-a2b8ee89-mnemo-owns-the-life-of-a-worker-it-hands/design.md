## Decisions

- 2026-11-23: no spec impact: Only external-lead behaviour changes (its delta is attached). dispatch-routing, tier-ladder and worker-brief govern the same files, but routing, tier choice and the worker prompt are unchanged.
- 2026-11-23: added step 3 (Integration check) because steps 1 and 2 ran in parallel

## Interfaces

- 2026-11-23: Worker JSON to the window and team_status (step 1 produces, step 2 renders): an optional string field `agent_phase` with exactly one of "awaiting_launch", "running", "stalled", "finished"; absent or null for workers that are not Claude Code agent workers. Derived at snapshot time, never persisted in team.json.
- 2026-11-23: File ownership: step 1 owns src-tauri/ and mnemo2/plugin/skills/mnemo-lead/SKILL.md; step 2 owns mnemo2/src/. They run in parallel.

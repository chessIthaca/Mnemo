# Plan: Post-review bugfixes + gap-filling

## Goal
Fix the four bugs and fill the six gaps identified in the codebase review
against the PRD (PLAN.md). The bugs range from a silently-broken memory
consolidation pipeline to a non-functional file browser. The gaps cover
placeholder views, unwired slash commands, a missing theme toggle, a missing
spawn-agent command, and dead code cleanup.

## Context
A full adversarial code review found the brain (Phases 1–7) complete and
correct (199 tests passing), and the Tauri UI swap (Phases A–E) ~85% done.
The four bugs are real defects; the six gaps are unimplemented PRD items.
This plan addresses all ten in priority order: brain correctness first, then
UI bugs, then placeholder views, then missing features, then cleanup.

---

## Steps
- [x] 1. Thread session_id into run_turn so consolidation isn't a no-op
- [x] 2. Enforce MAX_RETRIES for tool errors within a turn
- [x] 3. Re-inject buffered suggestions during approval
- [x] 4. Fix FileBrowser subdirectory navigation
- [x] 5. Implement real DiffViewer in the right panel
- [x] 6. Implement ToolOutput live view
- [x] 7. Wire /save and /load slash commands
- [x] 8. Add light theme toggle
- [x] 9. Add spawn_agent command
- [x] 10. Remove dead code in src/app/mod.rs

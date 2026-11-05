The lead's team_dispatch refuses tier `escalate` for fresh work, as the tier's own definition already says ("escalation only, never a first pick"); team_resume and team_route_fix still accept it. Also the first live end-to-end run of OpenSpec spec changes on a plan.

Why: the unified tier ladder (plan 87932c46) made `escalate` an escalation-only rung. Its definition and the team_dispatch tool text say "escalation only, never a first pick", and Laya/kNN never offer it, but `TeamManager::route_implementation_dispatch` in src-tauri/src/mnemo2/team.rs still accepts `tier: "escalate"` from the lead and runs it (folding to high when escalate has no model). The code should match the stated rule. This plan is also the first live use of OpenSpec spec deltas: it carries a MODIFIED delta on the `tier-ladder` capability (.coding/openspec/specs/tier-ladder/spec.md), which the board merges into that spec on close.

Scope and constraints:
- Only lead-tool dispatches (`request.source == DispatchSource::LeadTool`) are refused. IPC dispatches, `team_resume` with a tier and `team_route_fix` with a tier keep accepting `escalate`: those are escalation paths.
- The check goes in `route_implementation_dispatch` (src-tauri/src/mnemo2/team.rs, around line 1836, next to the existing `tier auto needs routing mode ...` refusal), BEFORE any routing or classification, in every routing mode.
- Identify the rung through the ladder, not a string literal alone: `self.tier_ladder().escalation()` returns the escalation-only rung (`src/config/endpoints.rs` `TierLadder::escalation`); refuse when `requested_tier == Some(rung.id.as_str())`.
- Refusal text (exact): `tier escalate is escalation only, never a first pick; dispatch on high, or move an existing worker up with team_resume or team_route_fix and tier escalate`.
- Also fix the stale hint in the same function: `tier auto needs routing mode suggest or auto (Settings > Decisions)` must say `(Settings > Tiers)`; the Decisions tab no longer exists.
- Do not touch src/config/endpoints.rs, the routing classifier, or any UI file.

Verification: `cargo test -p mnemo-app --lib mnemo2::team` and `cargo clippy -p mnemo-app --all-targets -- -D warnings` (run from the worktree root).

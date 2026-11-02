+++
title = "team tools answer with a compact worker entry"
created = "2027-01-11"
+++

Every Mnemo2 team tool answer puts a compact worker entry in its text (worker_status_view in src-tauri/src/mnemo2/team.rs). The tools: team_status, team_dispatch, team_check, team_review, team_merge, team_resume, team_message, team_stop, team_route_fix, review_fix, team_resolve_finding and team_audit_dispatch. The entry holds id, name, status, kind, tier, model, plan_id, plan_step, branch, worktree, error, pending, open findings (id, title, severity, file, line), checks (name, status, 8-char tip), review summary and tip, merge commit, cost_micros and last_activity_ms. Briefs, opening prompts, inboxes, check output, finding detail and review evidence are left out of the text. ToolResult.data keeps the full record for internal callers. team_status {detail: true} returns the full record in the text, and its answer carries a top-level `lead` object: {kind external|in_app, attached, since_ms, idle_since_ms}. Why: full records were about 6 KB per call and filled the lead's context.

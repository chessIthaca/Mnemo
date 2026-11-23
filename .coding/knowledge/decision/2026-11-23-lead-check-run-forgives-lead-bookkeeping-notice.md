+++
title = "lead check run forgives lead bookkeeping; notice drain sends no summary; worker screenshots go in the shared git exclude"
created = "2026-11-23"
evidence = "tests lead_run_forgives_a_bookkeeping_only_tree_change, notices_drain_without_session_start_sends_no_summary, worker_screenshots_leave_a_pool_checkout_clean"
+++

A check run in the LEAD checkout (team_run_checks without worker_id) counts an exit-0 command as passed when its before/after content trees differ only in lead bookkeeping paths (phase3::is_lead_bookkeeping: .coding/plans, reviews, knowledge, openspec, laya), checked with git diff-tree in src-tauri/src/mnemo2/team/check_run.rs forgive_lead_bookkeeping; any error fails closed and worker runs stay strict. The /lead/notices endpoint builds team.external_summary() and the knowledge map only when the body has session_start true; PostToolUse drains and Stop peeks get summary "" (hooks read it only at SessionStart). Mnemo appends /.coding/browser/screenshots/ to <git common dir>/info/exclude (shared by every worktree; never the project's .gitignore) whenever it creates or reuses a worker checkout (src/app/worktrees.rs ensure_screenshot_exclude), so untracked screenshots never dirty a checkout or change a measured content tree.

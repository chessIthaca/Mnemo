+++
title = "strict reviewer surface + failed-reviewer protocol + watchdog stack capture"
created = "2026-08-23"
+++

SPEC (2026-09-04, fd49bdb): reviewer subagent = strict read-only surface — ToolFilter::Reviewer name allow-list (current_plan + REVIEWER_BASE_TOOLS: reads, git_diff/log/show, web_fetch, graph_*, memory+backlog queries, write_review_report); NO ask_user/finish/mutations. finish = main-agent-only. Reviewer end w/o report (failed OR finished cleanly — broadened 2027-01-07, backlog 5b46674d) latches reviewer_failure_pending on parent; respawn denied until ask_user. Watchdog stall report carries stack walk + WCT wait-chain (Windows-only).

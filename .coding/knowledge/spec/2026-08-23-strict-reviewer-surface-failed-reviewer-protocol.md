+++
title = "strict reviewer surface + failed-reviewer protocol + watchdog stack capture"
created = "2026-08-23"
+++

SPEC (2026-09-04, fd49bdb): reviewer subagent = strict read-only surface — ToolFilter::Reviewer name allow-list (current_plan + REVIEWER_BASE_TOOLS: reads, git_diff/log/show, web_fetch, graph_*, memory+backlog queries, write_review_report); NO ask_user/finish/mutations. finish = main-agent-only. Reviewer end w/o report (failed OR finished cleanly — broadened 2027-01-07, backlog 5b46674d) latches reviewer_failure_pending on parent; respawn denied until ask_user. Watchdog stall report carries stack walk + WCT wait-chain (Windows-only).

Amended 2027-01-11: Tool-name correction (plan f4636852, backlog 85313a7e, review round-5 LOW 1 — the last surviving live availability claim naming unregistered tools): the reviewer's read-only git surface is the SINGLE `git_read` tool, ops diff/log/show/status — the "git_diff/log/show" in the SPEC line above means `git_read` with those ops. `git_diff`, `git_log` and `git_show` are NOT registered tools; they survive only as internal delegates constructed inside `GitReadTool::new` (src/tool/agent/git_read_tool.rs), and the factory registers only `GitReadTool` (src/agent/factory.rs). Ground truth for the reviewer's surface: `REVIEWER_BASE_TOOLS` / `ALWAYS_SAFE_ROLE_TOOLS` in src-tauri/src/ipc/spawn.rs.

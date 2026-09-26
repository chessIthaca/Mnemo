+++
title = "git_read op surface — diff/log/show/status (status = read-only tree cleanliness)"
created = "2027-01-11"
+++

git_read op surface (plan 65c6b70f, backlog 1aa7e456, commit 37540e0 on wt/macos-fix): FOUR read-only ops — diff (all uncommitted changes, never truncated), log (recent commits, optional path filter), show (one commit, stat by default), and status (added 2027-02-05).

op="status": the read-only home for "is the tree clean?" — GitStatusTool delegate (src/tool/agent/git_read.rs) runs the fixed argv ["status", "--short"] through the shared run_git_read (no user-controlled argv, no option-injection surface) and appends a one-line summary computed from the UNCAPPED data["stdout"]: empty → "(working tree clean — no changes, nothing untracked)"; else "(N changed, U untracked)" with "?? "-prefixed lines counted as untracked (count stays accurate even when the listing is capped). Clean tree → empty short listing + the summary line. Non-repo → git's own error passes through with success=false.

Unknown-op error (git_read_tool.rs): "unknown op '{other}' — valid: diff, log, show, status; for write operations (commit/merge/push/…) use the `git` tool" — names the write-ops alternative so a read-loop cannot persist (live incident plan 263a9e31 motivated the change).

Invariants: git_read stays AutoRun and separate from the NeedsApproval `git` tool (the split is load-bearing — reads never prompt and stay visible in Planning/Complete); the status op never stages/fetches/mutates (same read-only classification is_git_read_only already applies to the `git` tool's status subcommand). Frontend: op="status" renders a bare "status" chip via the argLabel fall-through (pinned in messageArgLabel.test.ts). Budget: Planning/Complete ceilings 18_900 (measured 18_727, dated comment in factory.rs).

Tests: status_op_lists_dirty_tree_and_summarizes + status_op_reports_clean_tree + the tightened unknown_or_missing_op_errors_with_the_valid_set (src/tool/agent/git_read_tool.rs). Known stale (pre-unification, future doc pass) — NARROWED 2027-01-11 (plan f4636852, review round-5 LOW 2): agent.md's retired names were fixed (that plan's round-3 delta); only the SPEC record 2026-08-23-strict-reviewer-surface-failed-reviewer-protocol.md still carried "git_diff/log/show", and that record was corrected by its own 2027-01-11 amendment — so no live availability claim names an unregistered tool any more.

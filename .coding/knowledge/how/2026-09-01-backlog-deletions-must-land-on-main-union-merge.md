+++
title = "backlog deletions must land on main (union merge resurrects wt deletions)"
created = "2026-09-01"
+++

HOW: deliberate backlog.jsonl deletions only stick on main — union merge resurrects wt-branch deletions. .coding/backlog.jsonl uses the git union merge driver, so deleting a line on a wt/* branch does NOT propagate: on merge_to_main the line returns (union keeps every input line). When an item is deleted deliberately (e.g. cleared in the Backlog UI — like 323036fd, the stale failed credit-balance item removed 2026-09-19 after its request shipped in ff7a633e), expect it to reappear after the next wt→main merge and re-delete it on MAIN, where the deletion becomes canonical for subsequently forked branches. Never "fix" a missing backlog line by restoring it on a wt branch without checking who deleted it — the wt agent's backlog_status calls validate-before-write and cannot be the deleter.

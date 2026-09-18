+++
title = "live shell output streaming — sink at dispatch, display-only ToolOutputDelta, rolling card tail (plan 0d2c1221) — MERGED into main"
supersedes = "2027-01-11-live-shell-output-streaming-sink-at-dispatch-dis"
created = "2027-01-11"
+++

MERGED into main — commit 9ce763d (plan 0d2c1221, backlog 7e6385b3) has been in main's history since the earlier wt/mnemo landing, pre-dating dcc7a93 (2027-01-16); this record's "landed on branch wt/mnemo" hint was stale. WHAT: a running shell call streams stdout/stderr into the chat card as produced — sink at dispatch, display-only ToolOutputDelta wire event, rolling 16 K-char card tail cleared on result; the final ToolResult stays byte-identical (what the model consumes). Full detail: .coding/knowledge/spec/2027-01-11-live-shell-output-streaming-sink-at-dispatch-dis.md.

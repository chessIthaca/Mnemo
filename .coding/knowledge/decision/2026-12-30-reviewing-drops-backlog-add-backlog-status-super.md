+++
title = "Reviewing drops backlog_add + backlog_status — SUPERSEDED by the 2026-12-30 reversal"
supersedes = "2026-08-23-reviewing-drops-backlog-add-backlog-status-user"
created = "2026-12-30"
+++

SUPERSEDED 2026-12-30 by "DECISION: backlog_add/backlog_status available in ALL workflow states — 2026-09-04 Reviewing exclusion reversed (2026-12-30)" (plan 8a16bc91): the Reviewing state now exposes both tools. History: the 2026-09-04 user decision removed them from ToolFilter::Reviewing (initially not implemented — plan 9275932b abandoned, the arm still allowed both — but later implemented with the 2026-09-04 citation in the arm comment and three test assertions). The 2026-12-30 user request reversed it after two blocked mid-review queue requests and one silently-lost backlog_add. The reviewer ROLE filter stays read-only (unchanged).

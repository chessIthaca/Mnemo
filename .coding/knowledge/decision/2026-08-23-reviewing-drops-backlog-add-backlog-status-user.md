+++
title = "Reviewing drops backlog_add + backlog_status (user 2026-09-04)"
created = "2026-08-23"
status = "superseded"
+++

User decision 2026-09-04: REMOVE backlog_add + backlog_status from ToolFilter::Reviewing (tool/mod.rs:277-287); keep in Planning/Executing/Complete. Review agents never see plan tools. NOT implemented — plan 9275932b abandoned; Reviewing arm still allows both. Detail: FINDINGS memory same date.

+++
title = "Resolve fixed findings before re-reviewing, or a step-down round is logged as failed"
created = "2026-11-05"
+++

team_review replaces the worker's finding list, so every finding it names counts as OPEN, even one whose detail says "Fixed in <commit>". With fix-round step-down, the next review settles the stepped-down round: an open routed finding records `fix_downshift` with `finding_closed: false`, logs a `fix_downshift_restore` row, and moves the worker back to its previous tier. That gives the kNN a wrong label and switches the model for nothing (seen 2026-11-04 on specArchive198). After a fix round: first call team_resolve_finding (status fixed, with evidence) for each finding the fix closed, then call team_review with `findings: []`, or with only the findings that are genuinely still open.

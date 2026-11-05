+++
title = "batched file_edit ops — read-receipt cites go stale after a line-shifting op"
created = "2026-11-02"
+++

In file_edit/multi_edit `ops` arrays the read-receipt cite form ({from_read, lines, new_string}) is validated against the snapshot its read minted, NOT against the file as left by the preceding ops in the same array. If an earlier op shifts lines (insert/delete), later cites land at stale positions and silently corrupt the file. Symptom: svelte-check reports "':' expected" or "'(' expected" in the edited file, then "'../lib/store' has no exported member 'actions'" cascades across every importer.

Rule: in a multi-op batch use verbatim content anchors ({old_string, new_string}) for every op, or do one read-receipt cite per call after a fresh read. Repair: read `git diff` for the file, restore each clobbered region from HEAD plus the intended edit in ONE cite, and re-diff until the change set is pure additions; when several regions are hit, rebuild the whole file with file_write from `git show HEAD:<path>` plus the intended edits.

Amended 2026-11-05: The compact line-op form (r/i/b/d verbs) is equally fragile in a large ops array: a 15-op array of r/i verbs, authored bottom-up so no op should have shifted a later target, still landed several ops at positions consistent with the original file numbering rather than the running content (plan fdc334b4, 2026-11-05) — the vi.hoisted mocks object lost its closing `}));` and a function signature landed inside another function's body, caught by vitest as a PARSE_ERROR before any test ran. Recovery that worked: one ops-array call of VERBATIM text anchors ({old_string, new_string} only — order-insensitive across disjoint regions, no line arithmetic), then re-read the regions and re-run the battery. Prefer verbatim anchors for multi-region edits; reserve line ops for single small edits whose result you re-read immediately.

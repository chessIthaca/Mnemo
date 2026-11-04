+++
title = "batched file_edit ops — read-receipt cites go stale after a line-shifting op"
created = "2026-11-02"
+++

In file_edit/multi_edit `ops` arrays the read-receipt cite form ({from_read, lines, new_string}) is validated against the snapshot its read minted, NOT against the file as left by the preceding ops in the same array. If an earlier op shifts lines (insert/delete), later cites land at stale positions and silently corrupt the file. Symptom: svelte-check reports "':' expected" or "'(' expected" in the edited file, then "'../lib/store' has no exported member 'actions'" cascades across every importer.

Rule: in a multi-op batch use verbatim content anchors ({old_string, new_string}) for every op, or do one read-receipt cite per call after a fresh read. Repair: read `git diff` for the file, restore each clobbered region from HEAD plus the intended edit in ONE cite, and re-diff until the change set is pure additions; when several regions are hit, rebuild the whole file with file_write from `git show HEAD:<path>` plus the intended edits.

+++
title = "edit files reliably when tool payload emissions decay (balanced-range edits, cites over verbatim anchors, sequential over batched)"
created = "2026-11-02"
+++

Working with the code-bearing tool payload emission decay (see .coding/knowledge/bug/2026-12-20-code-bearing-tool-payload-emissions-degrade-unde.md) — patterns that kept edits landing during a long degraded session (observed 2027-01-14, step B of the trace plan, ~30 edits):

1. The artifact detector rejects any single edit whose delimiter balance CHANGES (it counts the edit's own opens minus closes, per call). A partial change (e.g. adding `Some(` on one line) is inherently unbalanced and gets rejected even when the payload is not truncated. Fix: edit the SMALLEST balanced RANGE — include the line with the compensating close in the SAME edit (e.g. replace lines [411,413] `usage.map(|u| { ... }),` with `Some(usage.map_or(0, |u| { ... })),` — opens and closes both shift by the same amount).

2. multi_edit mangles its `files` array first (a top-level `path` key appears, or the array arrives as a string). When multi_edit calls start failing with shape errors, drop to SEQUENTIAL single file_edit calls — they kept working at payload sizes where multi_edit did not.

3. Single-line edits with read-receipt cites (`from_read` + `lines: [n, n]`) are the most reliable form at any degradation level; verbatim `old_string` anchors longer than ~3 lines are the least reliable (they get truncated mid-string, producing not-found or artifact rejections). For ranges, prefer cites over verbatim anchors.

4. After any edit that adds or removes lines, all subsequent cites' line numbers shift — re-read before the next range edit (a rejected edit with "line N changed since read" is a drift error, NOT emission decay: re-reading fixes it; blind-retrying does not).

5. Large insertions: file_write with mode overwrite (first section) + mode append (chunks under ~10 KB each) never failed; file_edit replacements of ~2.5-10 KB landed with the fragility warning; payloads over ~10 KB reliably truncated.

6. Distinguish the two failure modes before retrying: "emission artifact detected ... truncated" = emission decay (re-emit smaller/differently, never resend the same payload); "not found: line N changed since read" = drift (fresh read required); "invalid input: from_read and old_string are BOTH set" = schema slip (the cite form takes ONLY from_read+lines+new_string).

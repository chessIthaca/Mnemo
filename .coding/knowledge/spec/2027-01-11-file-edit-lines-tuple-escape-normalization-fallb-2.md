+++
title = "file_edit — lines tuple, escape-normalization fallback, no-op + miss diagnostics — MERGED into main (5a86e9d)"
supersedes = "2027-01-11-file-edit-lines-tuple-escape-normalization-fallb"
created = "2027-01-11"
+++

MERGED into main at 5a86e9d (5a86e9d1157a497a8f6fdc567c29d584dda774eb) on 2027-01-24 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 33ad6ec) — supersedes this record's earlier unmerged marker. WHAT (plan 04a195de, backlog 838b6f4e, commit a96041b): file_edit's line-range mode is `lines: [start, end]` (exactly two ints); the apostrophe/escape trap auto-falls-back to fuzzy matching with a visible note; no-op edits are rejected; a miss carries the first-difference pointer.

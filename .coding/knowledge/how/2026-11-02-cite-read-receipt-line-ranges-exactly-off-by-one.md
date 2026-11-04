+++
title = "cite read-receipt line ranges exactly — off-by-one cites clobber neighbors"
created = "2026-11-02"
+++

When using file_edit/multi_edit read-receipt cites ({from_read, lines, new_string}), the cited line numbers must be taken from the RECEIPT'S OWN numbering, not from a different read of the same file or from memory of an earlier numbering — a single-line off-by-one validates fine against the receipt's bytes but replaces the WRONG lines: an off-by-N cite silently clobbers neighboring lines (a doc-comment line vanished and a struct field was duplicated when a [39,41] cite landed one line off in src/headless/session.rs; the same mis-cite ate a guard's body in team.rs, leaving a dangling `));`). Rules that follow:
- Cite the exact range you SAW in the specific read that minted the receipt, and re-verify the region with a fresh read BEFORE writing when the file has been edited since that read.
- After a receipt-cited multi-op edit, immediately re-read the edited regions and `cargo check` before anything else — the drift gate validates bytes, not intent.
- For anchors without a fresh receipt, prefer the verbatim old_string form (content match is immune to line-number shifts).

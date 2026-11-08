+++
title = "Delete lines with file_edit's compact d-ops, not a cite with an empty new_string"
created = "2026-11-08"
+++

In file_edit and multi_edit, a read-receipt cite {from_read, lines, new_string} splits new_string on newlines and keeps the trailing empty string: ending in one newline adds one blank line, ending in two adds two, and new_string "" collapses the range to one blank line instead of deleting it. To delete lines, use the compact op `dN` or `dN-M` based on a fresh read, or include a neighbouring blank line in the cite range. For insertions, end the payload with one newline and let the next original line provide the separator.

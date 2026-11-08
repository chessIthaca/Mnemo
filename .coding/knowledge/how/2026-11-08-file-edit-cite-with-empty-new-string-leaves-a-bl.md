+++
title = "file_edit cite with empty new_string leaves a blank line — use compact d-ops for true deletions"
created = "2026-11-08"
evidence = "Observed repeatedly in one session: seven cite-with-\"\" field removals each left a stray blank line inside an interface/object literal, and a test-insert cite ending \"\\n\\n\" doubled the separator blank; compact d-ops cleaned them; verified by git diff + green battery afterwards."
+++

file_edit/multi_edit ops (read-receipt cite {from_read, lines, new_string}) split new_string on \n and KEEP the trailing empty string: a payload ending in "\n" produces one extra blank line, "\n\n" produces two. Replacing a cite range with new_string "" does NOT delete the range — it collapses it to exactly one blank line. To truly delete lines, use the compact line op dN / dN-M (exact but unvalidated — base it on a fresh read), or include a wanted blank inside the cite range so the leftover blank lands where a blank belongs. For insertions, end the payload with a single "\n" and let the original following line provide the next separator; check the result diff for stray/doubled blanks before running batteries.

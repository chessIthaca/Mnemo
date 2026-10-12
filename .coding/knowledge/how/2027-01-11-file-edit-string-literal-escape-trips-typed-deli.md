+++
title = "file_edit/string-literal escape trips — typed=delivered; probe byte counts"
created = "2027-01-11"
+++

Symptom: string literals edited via file_edit/file_write in this session carried doubled backslash-n text ("\n" as literal two chars instead of a newline escape) — the six literals in prompt.rs's re-signed build_volatile_tail (plan 339dfe72 round-1 HIGH). The green suite missed it: the order-pin golden is find()-based on bare headers and every other tail assert is newline-free contains.

Root cause (this session's transport): the file tools deliver NEW string content byte-for-byte as TYPED in the call payload, and the tool call serialization adds one more escape layer than expected — typed 4 backslashes + n landed 2 file backslashes + n (Rust: escaped backslash + n = the literal text "\n", not a newline). The MATCHER separately normalizes double-vs-single escape drift (each mismatch NOTEs "matched via escape-normalization"), so a wrong old_string can still match; the new_string is spliced literally.

Working pattern for string-literal byte fixes: (1) ground-truth the file with a shell backslash-count probe (Get-Content → ToCharArray → count, unpiped) before and after; (2) type ONE backslash per real escape in new_string (the +1 layer is the transport's serialization); (3) prove RED with the failing assertion before the fix and GREEN after; (4) re-probe byte counts + rebuild.

Regression test: agent::prompt::tests::volatile_tail_section_order_pinned — extended with separator-byte asserts (real newlines + the blank-line separator) and the literal-backslash-n sentinel; red pre-fix, green post-fix (the plan 339dfe72 fix landed them).


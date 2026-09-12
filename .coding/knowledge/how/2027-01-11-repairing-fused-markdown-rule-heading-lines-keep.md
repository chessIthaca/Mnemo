+++
title = "repairing fused markdown rule+heading lines — keep the space after ##"
created = "2027-01-11"
+++

HOW: when repairing fused `---## Heading` lines produced by chunked file_write appends, the replacement must preserve the space after the heading marker: old_string "---## " → new_string "---\n\n## " — a replacement ending in a bare "##" silently produces "##Heading", which CommonMark renders as a literal paragraph, not a heading (the section vanishes from the GitHub outline). Caught by round-1 review of plan 2c19aca0 (findings HIGH 1, .coding/reviews/2026-09-12-readme-marketing-rewrite-review.md): three README H2 headings (Highlights / How the repository works / Development & tests) were malformed by exactly this pattern. Corollary: after any multi-chunk file_write + fused-line repair, read the heading lines back and check for "##" without a following space before claiming the file clean.

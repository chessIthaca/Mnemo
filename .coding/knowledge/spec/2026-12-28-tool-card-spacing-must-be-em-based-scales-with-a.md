+++
title = "Tool-card spacing must be em-based — scales with --app-font-size"
created = "2026-12-28"
+++

Tool-card (and chat transcript) spacing must use em-based Tailwind arbitrary values (e.g. p-[0.5em], gap-[0.25em], py-[0.125em]), NOT rem-based utilities (py-0.5, gap-2): the Conversation container sets font-size: var(--app-font-size), so em values scale the tool cards with the user's font-size setting. Established by plan cf18d69a ("Make tool card text + frame scale with font size"); re-violated once by the frame-tightening change (plan 3de07a3b, caught as review finding L1 in .coding/reviews/2026-12-28-tool-card-frame-tightening-review.md, fixed to py-[0.125em]).

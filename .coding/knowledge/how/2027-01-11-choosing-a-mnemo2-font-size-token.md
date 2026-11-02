+++
title = "Choosing a Mnemo2 font size token"
supersedes = "2027-01-11-mnemo2-type-scale-rounding-and-code-font-convent"
created = "2027-01-11"
+++

When styling Mnemo2 text, pick by role, never by pixels: --text-meta for secondary labels, chips, timestamps and counts; --text-body for anything a person reads or clicks (text, buttons, tabs, inputs); --text-title for names and headings; --text-page only for page or sheet titles. Do not add a fifth size or hard-coded px font sizes; use em only for code inside text (0.92em). Use font-weight and colour (--muted, --ink-soft) for hierarchy within a size instead of a new size. Pixel line-heights on meta text must be at least 14px.

+++
title = "Mnemo2 type scale has four sizes (meta, body, title, page)"
supersedes = "2027-01-11-mnemo2-ui-type-scale-and-font-families"
created = "2027-01-11"
+++

Mnemo2's UI uses exactly four font-size tokens, defined in mnemo2/src/app.css: --text-meta 10px (chips, pills, labels, timestamps, counts, the step strip, file paths), --text-body 12px (the root size; message text, briefs, tool rows, buttons, tabs, filters, inputs), --text-title 14px (worker names in rows and the detail header, section and message headings h1/h2), --text-page 22px (page titles such as "Mission Control", sheet and empty-state headings). Nothing is smaller than 10px. Every component font-size uses one of these tokens; inline and block code inside Markdown use 0.92em so mono text matches the surrounding text. Inside Markdown, h1/h2 are title size and h3-h6 body size, all semibold Inter, so quoted plans never outrank the page. Tool rows in a thread (ToolActivity .tool-copy strong) are body size in the muted colour at weight 500, so messages lead and tool activity recedes. Mission control shows the live/slot count once, in the header pill; there is no separate summary line under the title. Why: the user found six sizes (8-26px) with 8-9px text and inverted hierarchy hard to read (2026-11-01).

+++
title = "reasoning effort \"off\" as highest curated level (833fa2fc) — MOOT, work off main"
created = "2027-01-07"
+++

MOOT 2027-01-10: the plan 833fa2fc allow-list work that shipped this bug is OFF main — merge 5cdd822 (which carried it) was rolled back at user request (main reset to the merge's first parent 2769fca). Original bug: reasoning effort "off" shipped as the highest curated level on seeded DeepSeek-family allow-lists (plan 833fa2fc FIX 3, user-reported) — the effort picker curated "off" at the top of the DeepSeek-family list instead of the lowest. The knowledge file (.coding/knowledge/bug/2027-01-07-reasoning-effort-off-shipped-as-the-highest-cura.md) was removed by the reset and restored from the reflog commit 5cdd822 to keep this record's truth file intact. Resurfaces only if the allow-list work is requeued (backlog 86fec233, status failed).

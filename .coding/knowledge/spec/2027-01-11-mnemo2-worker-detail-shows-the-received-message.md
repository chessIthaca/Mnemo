+++
title = "Mnemo2 worker detail shows the received-message inbox with senders"
created = "2027-01-11"
+++

Mnemo2's worker-detail thread tab renders WorkerInbox.svelte: the durable received-message ledger from Worker.inbox (TeamInboxEntry wire type, optional record.inbox so ledgers predating sender recording map to []). Each entry shows the sender label (external_lead → "Claude Code", lead → "Built-in lead", user → "You", mnemo → "Mnemo2"), a kind tag only for steer/fix, a relative stamp from elapsed.ts formatAgo(atMs, now=Date.now()) → "just now"/"Nm ago"/"Nh ago"/"Nd ago", and the delivered text as Markdown clamped by the shared Collapsible component; the section hides when the inbox is empty. store.workerFromRecord maps inbox via teamState inboxFromWire, which degrades unknown sender/kind values to mnemo/prompt instead of crashing. Component tests live next to WorkerInbox.svelte; the mapping test (with inbox, and absent → []) lives in store.phase3.test.ts.

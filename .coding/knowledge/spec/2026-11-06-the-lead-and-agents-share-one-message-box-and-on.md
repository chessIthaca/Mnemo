+++
title = "The lead and agents share one message box and one scroll-follow rule"
created = "2026-11-06"
+++

mnemo2/src/components/ComposerBox.svelte is the only message input shell; Composer.svelte (lead) and WorkerThread.svelte (agents, in Mission Control and the side panel) both render it. lib/workerComposer.ts `workerBox` decides an agent's box: a finished audit, or status completed, in_review, merged, merging or cancelled, is done and disabled (the user decided completed closes it); stopped or interrupted is disabled and points at Resume; compacting is disabled; working, dispatching or fixing steers; anything else starts a turn. This closes only the user's box: the backend and the lead's team_message still accept a completed worker, so the lead can send it back, and a fix round's `fixing` status reopens the box. Text clears only after the backend accepts, and half-typed drafts survive tab and agent switches. lib/stickToBottom.ts is the one follow rule (within 48 px of the end counts as at the bottom), used by LeadChat and WorkerThread; every agent view scrolls its messages on their own with the box pinned below.

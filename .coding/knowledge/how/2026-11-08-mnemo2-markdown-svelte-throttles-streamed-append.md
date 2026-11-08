+++
title = "Mnemo2 Markdown.svelte throttles streamed appends to 150 ms"
created = "2026-11-08"
+++

mnemo2/src/components/Markdown.svelte does not render on every `source` change. It keeps `shown` (the source its html came from) plus `lastShownAt`, and an `$effect` (reads only `source`; body wrapped in `untrack`) applies this rule: if the new source merely appends to `shown` (`next.startsWith(shown)`), it re-renders at most every `STREAM_RENDER_MS = 150` ms by arming one `setTimeout` for the remaining window, whose callback renders the newest `source`; any non-append change renders at once and cancels the pending timer; `onDestroy(cancelPending)` drops a pending render on unmount. The existing one-entry `cachedSource`/`cachedHtml` `$derived.by` reads `shown`. Timing uses `Date.now()` so `vi.useFakeTimers()` drives it; in Vitest 5 the fake clock is installed with the real current epoch, so the initial `lastShownAt = 0` means the first change after mount still renders immediately. Tests live in mnemo2/src/components/Markdown.test.ts.

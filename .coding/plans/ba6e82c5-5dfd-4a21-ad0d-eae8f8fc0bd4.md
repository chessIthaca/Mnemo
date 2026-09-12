# Plan: Fix duplicated streaming text from double event listener

## Goal
Make useAgentEvents robust to StrictMode double-mount so only one Tauri event listener is ever active, eliminating the duplicated streaming text.

## Context
React.StrictMode (main.tsx:8) double-invokes effects in dev. In useAgentEvents.ts, the effect subscribes to Tauri agent events via the async `listen()` (onAgentEvent). The cleanup sets `cancelled = true` but the unlisten function hasn't resolved yet (it's assigned in a `.then`), so the first listener is never removed. A second listener is registered on re-mount. Result: every `text_delta` is buffered/flushed twice via appendStreamingText → the assistant's streaming text is appended twice per token, appearing as text overwriting/duplicating itself line after line. The `cancelled` flag only handles the resolve-after-unmount case, not the cleanup-before-resolve case.

## Steps
- [x] 1. Fix useAgentEvents.ts: track the resolved unlisten fn and always call it in cleanup — if the promise resolves after cleanup, immediately invoke the received unlisten; if it resolved before, call the stored one. Also guard against a pending rAF flush referencing a torn-down buffer.
- [x] 2. Verify the fix: npm run build (tsc) to confirm no type errors; confirm only a single listener registration path remains.

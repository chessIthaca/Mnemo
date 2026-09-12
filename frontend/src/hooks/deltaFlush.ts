// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The 64 KiB occlusion cap for a per-agent streaming-delta buffer (the
 * text/reasoning/tool-arg buffers in `useAgentEvents.ts`). R2 of the
 * 2026-08-18 RDP freeze diagnosis (.coding/reviews/2026-08-18-rdp-freeze-
 * diagnosis.md).
 */
export const MAX_BUFFERED_DELTA_CHARS = 64 * 1024;

/**
 * Whether a buffered delta string must flush synchronously instead of waiting
 * for the next animation frame. `requestAnimationFrame` never fires while the
 * window is occluded (minimized / covered — e.g. an RDP desktop switch; and
 * no `visibilitychange` fires to compensate, upstream WebView2Feedback
 * #4879), so without this cap the buffer grows for the entire hidden period
 * while the agent keeps streaming, then flushes in one janky burst on return
 * — the user-visible "UI catching up real fast". Flushing synchronously past
 * the cap converts that burst into bounded incremental flushes.
 */
export function shouldFlushSync(bufferedChars: number): boolean {
  return bufferedChars >= MAX_BUFFERED_DELTA_CHARS;
}

/**
 * The maximum gap (ms) between delta flushes before a delta flushes
 * IMMEDIATELY instead of waiting for the next animation frame. See
 * [`shouldFlushNow`].
 */
export const MAX_FLUSH_INTERVAL_MS = 40;

/**
 * Whether a buffered delta must flush right now based on elapsed time since
 * the last flush. `requestAnimationFrame` stalls in occlusion / RDP states
 * (WebView2Feedback #4879 — no `visibilitychange` fires to compensate), so a
 * purely frame-based flush leaves sub-64 KiB responses invisible until a
 * structural event or turn end. When deltas arrive sparsely (>= 40ms apart —
 * the norm for reasoning models), each one lands in the store immediately:
 * the display streams token-by-token, "as soon as a token comes in".
 * Dense bursts (< 40ms apart) keep the per-frame batch, so the store update
 * rate stays bounded either way.
 */
export function shouldFlushNow(lastFlushAt: number, now: number): boolean {
  return now - lastFlushAt >= MAX_FLUSH_INTERVAL_MS;
}

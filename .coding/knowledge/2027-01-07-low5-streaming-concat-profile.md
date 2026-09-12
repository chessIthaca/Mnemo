# LOW 5 profile: appendStreamingText per-flush concat — NOT WORTH IT (2027-01-07)

Mem-perf review LOW 5 (.coding/reviews/2027-01-06-full-review-memory-perf.md:60-67)
claimed `appendStreamingText`/`appendStreamingReasoning` rebuild the full
accumulated string per rAF flush (~3-5 MB transient copying per 100 KB
response) and gated the proposed fix (accumulate chunks in an array, join on
finalize) on profiling: "Only if profiling ever shows it... Do not do this
speculatively."

## Verdict: profiled, not worth it — no code change

Pre-registered gate (fixed before measuring): implement only if the current
pattern's worst per-flush string cost at the 128 KB SANE-capped size
(SANE_MAX_OUTPUT_TOKENS = 32 K tokens, ~4 chars/token) exceeds ~10% of the
16.7 ms 60 fps frame budget (> 1.7 ms/flush) AND the array variant reduces
it >= 5x.

Measured (node v24.7.0, win32, V8 — same engine family as WebView2; JSC also
uses rope strings, so the conclusion is directionally valid on macOS):

| scenario            | flushes | A worst flush | %budget | A0 total  | B worst flush |
|---------------------|---------|---------------|---------|-----------|---------------|
| 100 KB @ 400 B      | 256     | 0.433 ms      | 2.59%   | 0.032 ms  | 0.116 ms      |
| 100 KB @ 4 KB       | 25      | 0.130 ms      | 0.78%   | 0.003 ms  | 0.009 ms      |
| 100 KB @ 64 KiB     | 2       | 0.158 ms      | 0.95%   | 0.001 ms  | 0.153 ms      |
| 128 KB @ 400 B      | 328     | 0.556 ms      | 3.33%   | 0.026 ms  | 0.031 ms      |
| 128 KB @ 4 KB       | 32      | 0.151 ms      | 0.90%   | 0.003 ms  | 0.059 ms      |
| 128 KB @ 64 KiB     | 2       | 0.147 ms      | 0.88%   | 0.001 ms  | 0.070 ms      |
| 512 KB @ 400 B      | 1311    | 5.06 ms       | 30.30%  | 0.111 ms  | 0.032 ms      |
| 512 KB @ 4 KB       | 128     | 2.07 ms       | 12.38%  | 0.014 ms  | 0.016 ms      |
| 512 KB @ 64 KiB     | 8       | 0.772 ms      | 4.62%   | 0.001 ms  | 0.105 ms      |

A  = current pattern: rope concat per flush + flatten-forcing full-string
     read per flush (Buffer.from(acc) as the DOM text-node update proxy:
     flatten + copy into fresh storage).
A0 = store side only (the concat, no render read).
B  = proposed: chunk push + append-only per-frame read + join once at
     finalize (join cost 0.007-0.1 ms, one-time).

## Why the gate does not fire

1. At the app's hard bound (128 KB), the worst flush is 0.556 ms = 3.3% of
   the frame budget — under the 10% threshold. The absolute saving variant B
   offers there is ~0.5 ms/frame at the very end of a max-length response,
   against React reconciliation + layout costs that exist in both variants
   and dwarf it.
2. The store-side concat is free (A0): `acc + chunk` is a V8 ConsString
   (rope) — O(1) allocation, no copy. A whole 128 KB response's store-side
   concat work totals 0.026 ms. The review's "~3-5 MB transient string
   copying" is entirely the RENDER-side flatten (the DOM text-node update
   needs the flat string), which any full-text display incurs — not the
   store pattern the finding named. The fix would also have to touch 4
   accumulation sites (streamingText, streamingReasoning, activityLog
   answer + reasoning entries) plus 2 display paths to realize even that
   ~0.5 ms saving.
3. 512 KB (4x beyond the cap) is where the pattern would matter (5.06 ms =
   30% of a frame at 400 B/flush) — but SANE_MAX_OUTPUT_TOKENS bounds
   responses at ~128 KB, so it is unreachable. RE-OPEN CONDITION: if that
   cap ever rises >= 4x, re-run the benchmark before reconsidering.

## Re-run

    node --expose-gc frontend/bench/streaming-concat.bench.mjs

The bench script is committed alongside this file; its header documents the
method, scenario matrix, and caveats (DOM-proxy, one-byte content, V8/JSC).

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Benchmark for mem-perf review LOW 5 (.coding/reviews/2027-01-06-full-
// review-memory-perf.md): appendStreamingText rebuilds the accumulated
// string per rAF flush. The review gates the proposed fix (accumulate
// chunks in an array, join on finalize) on profiling: 'Only if profiling
// ever shows it... Do not do this speculatively.'
//
// Run:  node --expose-gc frontend/bench/streaming-concat.bench.mjs
//
// METHOD - mirrors the app's real streaming path (useAgentEvents.ts rAF
// flush -> useAgentStore.appendStreamingText -> Conversation.tsx
// streamingBlock -> Message.tsx streaming case, which renders PLAIN text):
//
//   Variant A (current):  per flush `acc = acc + chunk` (V8 rope concat,
//     O(1) alloc) + a flatten-forcing full-string read. The read models the
//     per-frame DOM text-node update: setting a text node's data requires
//     the flat string, so the rope is flattened + copied into the node's
//     storage. Buffer.from(acc) is the Node proxy for that (flatten + copy
//     into fresh storage).
//
//   Variant A0 (store-only): the concat WITHOUT the render read - isolates
//     how much of A is the store-side concat vs. the render-side flatten.
//
//   Variant B (proposed):  chunks.push(chunk) + per-frame read of only the
//     new chunk (append-only DOM text node) + chunks.join('') once at
//     finalize.
//
// Scenario matrix: total sizes {100 KB (review reference), 128 KB
// (SANE_MAX_OUTPUT_TOKENS = 32 K tokens, ~4 chars/token), 512 KB (4x
// stress)} x flush chunk {400 B (typical 60 Hz coalescing), 4 KB (fast
// provider), 64 KiB (the rAF-buffer occlusion cap)}.
//
// CAVEATS:
// - Node's V8 ~= WebView2's V8 (primary platform). macOS WebKit uses JSC,
//   which also uses rope strings for concat; the flatten-on-render cost
//   applies there too, so the conclusion is directionally valid for both.
// - Chunk content is one-byte ASCII (the common case); two-byte content
//   would roughly double the copy costs of BOTH variants.
// - The DOM does more per frame than the string copy (React reconciliation,
//   layout); those costs exist in both variants and are NOT measured here -
//   this isolates the string handling the fix would change.

const FRAME_BUDGET_MS = 16.7; // 60 fps
const SIZES = [
  { label: '100 KB', bytes: 100 * 1024 },
  { label: '128 KB', bytes: 128 * 1024 },
  { label: '512 KB', bytes: 512 * 1024 },
];
const CHUNKS = [
  { label: '400 B/flush', bytes: 400 },
  { label: '4 KB/flush', bytes: 4 * 1024 },
  { label: '64 KiB/flush', bytes: 64 * 1024 },
];
const TRIALS = 7;

/** Realistic one-byte prose filler of exactly `bytes` characters. */
function makeChunk(bytes) {
  const unit = 'The quick brown fox jumps over the lazy dog. ';
  let s = '';
  while (s.length < bytes) s += unit;
  return s.slice(0, bytes);
}

/** Variant A - current: rope concat per flush + flatten-forcing full read. */
function runCurrent(totalBytes, chunkBytes) {
  const chunk = makeChunk(chunkBytes);
  const flushes = Math.ceil(totalBytes / chunkBytes);
  let acc = '';
  let worstFlush = 0;
  const t0 = performance.now();
  for (let i = 0; i < flushes; i++) {
    const f0 = performance.now();
    acc = acc + chunk;
    const sink = Buffer.from(acc); // DOM text-node update proxy
    const f1 = performance.now();
    if (f1 - f0 > worstFlush) worstFlush = f1 - f0;
    if (sink.length === 0) throw new Error('unreachable');
  }
  return { total: performance.now() - t0, worstFlush, flushes };
}

/** Variant A0 - store side only: the concat, no render read. */
function runStoreOnly(totalBytes, chunkBytes) {
  const chunk = makeChunk(chunkBytes);
  const flushes = Math.ceil(totalBytes / chunkBytes);
  let acc = '';
  let worstFlush = 0;
  const t0 = performance.now();
  for (let i = 0; i < flushes; i++) {
    const f0 = performance.now();
    acc = acc + chunk;
    const f1 = performance.now();
    if (f1 - f0 > worstFlush) worstFlush = f1 - f0;
  }
  // Keep the concats live (dead-store elimination guard).
  if (acc.length === 0) throw new Error('unreachable');
  return { total: performance.now() - t0, worstFlush, flushes };
}

/** Variant B - proposed: chunk push + append-only read + join on finalize. */
function runProposed(totalBytes, chunkBytes) {
  const chunk = makeChunk(chunkBytes);
  const flushes = Math.ceil(totalBytes / chunkBytes);
  const chunks = [];
  let worstFlush = 0;
  const t0 = performance.now();
  for (let i = 0; i < flushes; i++) {
    const f0 = performance.now();
    chunks.push(chunk);
    const sink = Buffer.from(chunk); // append-only text node
    const f1 = performance.now();
    if (f1 - f0 > worstFlush) worstFlush = f1 - f0;
    if (sink.length === 0) throw new Error('unreachable');
  }
  const final = chunks.join(''); // join once at finalize
  if (final.length === 0) throw new Error('unreachable');
  return { total: performance.now() - t0, worstFlush, flushes };
}

/** Median of a numeric array. */
function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Run one variant `trials` times; median total, worst flush across trials. */
function measure(variant, totalBytes, chunkBytes) {
  variant(totalBytes, chunkBytes); // warmup (JIT + string internals)
  const totals = [];
  let worstFlush = 0;
  for (let t = 0; t < TRIALS; t++) {
    if (typeof gc === 'function') gc();
    const r = variant(totalBytes, chunkBytes);
    totals.push(r.total);
    if (r.worstFlush > worstFlush) worstFlush = r.worstFlush;
  }
  return { totalMedian: median(totals), worstFlush };
}

const fmtMs = (ms) => (ms >= 1 ? ms.toFixed(2) : ms.toFixed(3));

console.log(`node ${process.version} (${process.platform})`);
console.log(
  `frame budget ${FRAME_BUDGET_MS} ms; ${TRIALS} trials/median; worst flush = max over trials`,
);
console.log('');

const header =
  'scenario'.padEnd(24) +
  'flushes'.padStart(7) +
  ' | A total  A worst  %budget | A0 total A0 worst | B total  B worst  join';
console.log(header);
console.log('-'.repeat(header.length));

for (const size of SIZES) {
  for (const chunk of CHUNKS) {
    const label = `${size.label} @ ${chunk.label}`;
    const flushes = Math.ceil(size.bytes / chunk.bytes);
    const a = measure(runCurrent, size.bytes, chunk.bytes);
    const a0 = measure(runStoreOnly, size.bytes, chunk.bytes);
    const b = measure(runProposed, size.bytes, chunk.bytes);
    // Clean join-only measurement for variant B's finalize cost.
    const joinChunks = new Array(flushes).fill(makeChunk(chunk.bytes));
    if (typeof gc === 'function') gc();
    const j0 = performance.now();
    joinChunks.join('');
    const joinMs = performance.now() - j0;
    const pct = ((a.worstFlush / FRAME_BUDGET_MS) * 100).toFixed(2);
    console.log(
      label.padEnd(24) +
        String(flushes).padStart(7) +
        ' |' +
        `${fmtMs(a.totalMedian)}ms`.padStart(9) +
        `${fmtMs(a.worstFlush)}ms`.padStart(9) +
        `${pct}%`.padStart(8) +
        ' |' +
        `${fmtMs(a0.totalMedian)}ms`.padStart(9) +
        `${fmtMs(a0.worstFlush)}ms`.padStart(9) +
        ' |' +
        `${fmtMs(b.totalMedian)}ms`.padStart(9) +
        `${fmtMs(b.worstFlush)}ms`.padStart(9) +
        `${fmtMs(joinMs)}ms`.padStart(7),
    );
  }
}

console.log('');
console.log('A  = current (concat + full flatten read per flush - DOM text update)');
console.log('A0 = store side only (concat, no render read)');
console.log('B  = proposed (chunk push + append-only read; join once at finalize)');
console.log('%budget = A worst flush as % of one 60 fps frame');

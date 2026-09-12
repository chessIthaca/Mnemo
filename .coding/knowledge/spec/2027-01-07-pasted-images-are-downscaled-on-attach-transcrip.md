+++
title = "Pasted images are downscaled on attach + transcript image byte budget evicts oldest payloads"
created = "2027-01-07"
+++

Pasted/dropped image attachments are memory-bounded (mem-perf review LOW 4, plan ab0ef952, commit 6ab0c97 on wt/agenticcoding — pre-merge, not yet in main):

1. Paste-time downscale — `frontend/src/lib/attachImages.ts`: every attached image is capped at a 1568 px long edge (vision-API input limit) and re-encoded as JPEG q0.85 (white-fill for alpha) before reaching the store/model. ≤512 KiB data URLs skip decode/re-encode (preserves small PNG alpha); any decode/canvas failure degrades gracefully to the ORIGINAL data URL (an image is never lost). Wired into InputBar + BacklogView (both paste/drop paths) via `fileToAttachedDataUrl`. Steered images get this automatically — they ride the same attach path (spec 2027-01-07-steered-images-ride-the-steer-payload-end-to-e).

2. Transcript rolling image byte budget — `agentState.ts` `capTranscriptImages` (32 MiB of data-URL chars, folded into `capTranscript` = single choke point for all append sites): newest-first retention; overflow entry + older image-bearing entries get `images` replaced by `imagesEvicted: n` (optional field on the user AND steer TranscriptEntry variants, types.ts — steer entries covered since 2027-01-07, same payload class); Message renders an "N images unloaded to save memory" placeholder chip (user + steer cases); `arePropsEqual` compares `imagesEvicted` so evicted rows re-render.

3. Documented trade-off: `/save` serializes the CAPPED transcript — evicted images are permanently absent from saves (restore as the chip). Intended: bounded saves vs unbounded multi-MB payloads.

Tests: `attachImages.test.ts` (14) + `agentState.images.test.ts` (14). Reviews: `.coding/reviews/2027-01-07-pasted-images-downscale-byte-budget-review.md` (round 1, 2 low — fixed) + `-round2.md` (PASS).

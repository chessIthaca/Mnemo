## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (HEAD eac25dc) for plan ab0ef952 — "Pasted images: downscale on paste + transcript image byte budget (mem-perf review LOW 4)". Both mechanisms (paste-time downscale via `lib/attachImages.ts`, rolling 32 MiB image-char budget via `capTranscriptImages` folded into `capTranscript`) are correctly implemented, well-tested, and wired through every attach/append path. No correctness, security, type-safety, or platform issues found. Two LOW findings: `/save` now exports the post-eviction transcript (budget-evicted images permanently absent from saves — should be a conscious, documented decision), and the README doesn't mention the two new user-visible behaviors.

### Findings

**LOW 1 — `/save` exports the post-eviction transcript: budget-evicted images are permanently lost from saved files.**
- Where: `frontend/src/components/layout/InputBar.tsx:478-480` (`/save` → `JSON.stringify(agent.transcript)`), interacting with `frontend/src/hooks/agentState.ts:460-480` (`capTranscriptImages`).
- Before this change, `/save` wrote every retained image. Now an entry whose images were evicted by the 32 MiB budget serializes as `{ kind: "user", text, imagesEvicted: n }` — no image data — and a subsequent `/load` restores the placeholder chip, not the images. The `capTranscriptImages` doc comment calls eviction "display-side only", but `/save` makes it export-side too.
- Impact is bounded (requires >32 MiB of retained image chars — roughly 64-300 downscaled attachments — in one conversation) and arguably desirable (unbounded multi-MB save files are the same defect class), but it is a silent behavior change on a path the plan did not mention.
- Fix suggestion (pick one): (a) accept and document — extend the `capTranscriptImages` doc comment ("display-side only" → "display- and export-side: `/save` serializes the capped transcript") and add a line to the `/save` case comment in InputBar; or (b) if export fidelity matters, serialize from a pre-eviction source (not recommended — it reintroduces the unbounded payload the budget exists to bound).

**LOW 2 — Documentation sync: README doesn't cover the two new user-visible behaviors.**
- Where: `README.md:67-68` (chat image features list).
- The README documents chat image behavior in detail (tool images inline, image-parsing cards) but says nothing about (a) pasted/dropped attachments now being auto-downscaled to a 1568 px long edge and re-encoded as JPEG q0.85 before reaching the transcript and the model, or (b) the new "N image(s) unloaded to save memory" placeholder chip shown for budget-evicted attachments. Both are user-visible.
- Fix suggestion: one line in the chat-features list near README.md:68, e.g. after the image-parsing bullet: pasted/dropped image attachments are downscaled to a 1568 px long edge (JPEG) to bound memory, and the oldest attachments are unloaded with a placeholder chip once the transcript's retained image payload exceeds its budget. Optionally a half-sentence in PLAN.md:879-886 (vision fallback for attached images) noting attachments are capped at 1568 px before sending — optional since the review report is the spec for this internal perf fix.

### Verification detail (per the requested checks)

**1. Eviction pass (`capTranscriptImages`, agentState.ts:460-480) — correct.**
- Newest-first walk: iterates `entries.length-1 → 0` accumulating `kept`; the first entry whose cumulative chars would exceed the budget sets `evictFrom` and breaks. Since the fast-path total (the same sum) exceeded the budget, the walk is guaranteed to find an overflow point — `evictFrom` is always set when over budget, so the `evictFrom = entries.length` initial value is unreachable in the over-budget path.
- Overflow semantics: the rebuild evicts the overflow entry itself (`i > evictFrom` keeps only strictly-newer entries) AND every older image-bearing user entry — exactly the spec'd newest-first retention (contiguous oldest-side eviction; an older entry that would individually fit is still evicted once a newer one overflows — pinned by the "evicts every image-bearing entry older than the overflow point" test).
- Boundary: total exactly == budget → fast path returns the SAME array (test: "keeps entries at exactly the budget").
- Single-oversized entry: evicted on the first walk iteration (kept=0 + chars > budget) — the retained total is always ≤ budget (test covers).
- Idempotence: evicted entries lose `images` entirely (destructured out, so `imagesEvicted` and `images` are genuinely mutually exclusive), so re-running sums them as 0 → under budget → same array back (test covers).
- Identity preservation: under budget the SAME array reference is returned; over budget the `map` keeps untouched entries by reference (tests assert `toBe(mid)` / `toBe(newest)` / `toBe(b)` / `toBe(c)`), so Message's memoized rows re-render only for actually-evicted entries. Evicted entries keep `entryId`/`ts` via `...rest` (test covers) — Conversation's React keys stay stable across eviction.
- `images: []` is never rewritten (explicit `entry.images.length === 0` guard; test covers) and never counted (the inner sum loop adds 0).
- Choke point: every append funnels through `capTranscript` — all 12 call sites verified (InputBar.tsx:175, 328, 528; agentEventReducer.ts:514, 569, 892, 935, 1186, 1284, 1354; agentState.ts:590, 607 — the latter two being `flushStreamingText`/`pushTranscriptEntry`, shared by the `prompt_dispatched`/`skill_started`/steer reducers). Every `transcript.push` in the reducer is followed by a `capTranscript` assignment. The single uncapped assignment, `agentEventReducer.ts:675` (`reduceToolResult`), operates on a copied array (`[...next.transcript]`, line 619) and replaces an existing tool/memory entry in place — no append, no length change, no user-images change; both cap invariants are unaffected. `sweepRunningCards` sites replace entries without appending (and `reduceChildFinished` re-caps at 1354).
- Perf of the fold: the fast-path total is O(#image-URLs) per append (`.length` is O(1) per string) — at most ~300 URLs at budget; `capTranscript` runs on appends/events, not per render frame.

**2. Downscale helper (attachImages.ts) — correct.**
- Fast path: non-`data:image/` URLs and ≤512 KiB-char URLs return unchanged before any decode (both unit-tested; the threshold test pins the exact boundary, at and +1 over).
- JPEG white fill (`ctx.fillStyle = "#ffffff"` + `fillRect` before `drawImage`) — transparent regions render white, not black (source-contract test).
- Keep-smaller-of-original-vs-reencoded (`reencoded.length < dataUrl.length ? reencoded : dataUrl`) — a small noisy PNG cannot grow through the pass (source-contract test).
- Zero-natural-dimension guard (`naturalWidth/naturalHeight === 0` → return original) — dimensionless SVGs and broken files never reach the canvas math.
- Graceful fallback: the entire decode/canvas path sits inside try/catch returning the ORIGINAL data URL — an image is never lost or corrupted, worst case full-size (unit test stubs a throwing `Image` and asserts the original comes back).
- Wiring: `addImageFiles` in InputBar.tsx and both BacklogView paths (new-item input + per-card editor) map `fileToAttachedDataUrl`; both local `fileToDataUrl` copies are deleted, and `readAsDataURL` now appears only in attachImages.ts — no attach path bypasses the downscale (verified by search + source-contract tests, including the `toHaveLength(2)` pin on BacklogView's two call sites).

**3. Message.tsx — correct.**
- The chip renders only when `entry.imagesEvicted != null && entry.imagesEvicted > 0` (line 349), placed after the images grid inside the user case; the `title` tooltip text is static — no injection.
- `arePropsEqual` (lines 53-60): the images→imagesEvicted flip changes both compared fields (`peImages !== neImages` — array ref vs undefined — and `peEvicted !== neEvicted` — undefined vs n), so the row re-renders exactly once per eviction; non-evicted entries keep their object references so existing memoization is untouched. The reverse flip cannot happen (eviction is monotone — images are only ever removed, never re-added to an entry).

**4. Type safety — clean.**
- `imagesEvicted?: number` sits on the user variant only (types.ts:320-333); Message accesses it inside the narrowed `case "user"`; `arePropsEqual`'s casts match the existing style already used for `images`. The rebuild's `const { images, ...rest } = entry` is guarded by `!entry.images` first, so `images` is `string[]` and `{ ...rest, imagesEvicted: images.length }` is assignable to the user variant. No `tsc` breakage risk found in the changed code (build runs `tsc && vite build`; the 917-test vitest suite passes).

**5. Project constitution.**
- Doc comments: every export in attachImages.ts and both new agentState.ts exports (`MAX_TRANSCRIPT_IMAGE_CHARS`, `capTranscriptImages`) have doc comments; the internal `loadImage` does too. ✓
- Multi-platform neutrality: `Image`, `FileReader`, `canvas.getContext("2d")`, `toDataURL("image/jpeg", q)`, `imageSmoothingQuality` are standard DOM APIs available in both WebView2 (Chromium) and macOS WebKit — no platform-specific code anywhere in the change. ✓
- Documentation sync → LOW 2.
- Warning-free build: no unused imports/vars introduced (the removed local functions took their imports with them; `Image as ImageIcon` is used).

**6. Tests — exercise the changed paths and would fail without the fix.**
- `agentState.images.test.ts` (13 tests): direct unit tests of `capTranscriptImages`/`capTranscript` covering every edge case above plus the count-cap composition (1001 entries → count drop + image eviction of the survivor). Without the implementation the imports fail; with wrong semantics the `toBe` identity assertions and `toEqual` shape assertions fail. Real regression tests.
- `attachImages.test.ts` (14 tests): constants pinned, `isSmallDataUrl` boundary, both fast paths, the never-lose-an-image fallback (a real unit test with a stubbed throwing `Image`), and source contracts for the canvas path (JPEG + white fill + long-edge cap + smaller-of), the three wiring sites, and the Message chip + memo compare. The canvas path itself is source-contract only (node env, no DOM) — the established pattern for DOM-bound code (InputBar.test.ts); acceptable.
- Minor gap (not a finding): no test constructs a multi-image entry asserting `imagesEvicted: 2` — the code is `images.length`, trivially correct; a one-line test would pin it.

**7. Security — clean.**
- No new IPC surface (`saveConversation`/`sendPrompt`/backlog IPC unchanged), no path handling, no new parsing of untrusted input beyond the browser's own data-URL decode. Data URLs render via `<img src>` (no script execution); the chip's `title` is static text. The canvas re-encode shrinks the payload sent over IPC. ✓

### Notes (non-findings)
- Animated GIFs over 512 KiB are flattened to a static first-frame JPEG by the re-encode — inherent to the spec'd JPEG re-encode; vision APIs consume the first frame anyway. Conscious tradeoff, no action needed.
- The `.coding/backlog.jsonl` change (item 0be09983 pending → in_flight, plan ab0ef952) is the expected bookkeeping.
- The doc-comment estimate "32 MiB ≈ 64-300 downscaled attachments" checks out (0.1-0.5 MB per downscaled attachment).
- Backlog sidecar files (`.coding/backlog-images/`) now receive the downscaled JPEGs too, since BacklogView attaches through the same pipeline — a bonus reduction in sidecar size, consistent with the earlier sidecar plan.

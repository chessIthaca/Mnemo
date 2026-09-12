# Review: Replace `describe_image` with 7 `image_*` tools + agentic zoom loop

**Scope:** All uncommitted changes (10 modified/deleted files + 4 new files under
`src/tool/agent/image_tools/`). Build + tests reported green (851 lib + 15 integration).

## Summary

The shared vision plumbing (`mod.rs`) is a clean, faithful move from the deleted
`describe_image.rs` — sandbox validation, magic-byte sniff, and the
`NeedsApproval` posture are all preserved. The prompt builders (`prompts.rs`)
and tool scaffolding (`tools.rs`) are well-structured with good doc coverage.
**However, the zoom loop (`zoom.rs`) has a critical serde bug that makes the
core "zoom" action non-functional, a latent while-loop bug that would surface
once the serde bug is fixed, and CPU-intensive image operations running on the
async runtime instead of `spawn_blocking` as the plan specified.** The
`image_ui_diff` tool also cannot perform a real visual diff given the
single-image `ImageDescriber` trait.

---

## CRITICAL — correctness

### C1. `ZoomVote.box_` is never deserialized — the zoom action is completely broken
**File:** `src/tool/agent/image_tools/zoom.rs:84-95`

The `ZoomVote` struct names its field `box_` (trailing underscore, because `box`
is a reserved keyword in Rust) but has **no `#[serde(rename = "box")]`**:

```rust
#[derive(Debug, Deserialize)]
struct ZoomVote {
    action: String,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    box_: Option<Vec<f64>>,   // ← serde looks for JSON key "box_", NOT "box"
    ...
}
```

serde uses the Rust field name as the JSON key by default. So it looks for
`"box_"` in the model's response. But `zoom_control_prompt` (prompts.rs:162)
instructs the model to emit `"box":[x,y,w,h]`. The keys don't match, so `box_`
always deserializes to `None` (via `#[serde(default)]`).

**Impact — the entire zoom loop is dead code:**
1. Model votes `{"action":"zoom","box":[0.1,0.2,0.3,0.4]}`.
2. `parse_vote` → `ZoomVote { action:"zoom", box_: None, ... }`.
3. `vote_crop_url` (zoom.rs:359): `vote.box_.as_ref().filter(|b| b.len()==4)` →
   `None` → returns `None`.
4. `zoom_loop` (zoom.rs:253): `if let Some(crop_url) = vote_crop_url(...)` is
   `None` → falls through to the "couldn't crop" branch (zoom.rs:332-340).
5. Returns `vote.answer.unwrap_or(overview_answer)` — but `answer` is `None`
   (the model left it empty per the zoom instruction), so it returns
   `overview_answer` = the **raw JSON vote string** as the user-facing markdown.

So every zoom vote returns `{"action":"zoom","box":[...]}` as the "answer" with
a warning "model voted a region that could not be cropped; used overview". The
agentic auto-zoom loop never zooms.

**Why the tests miss it:** `parse_vote_strips_fences_and_extracts_json`
(zoom.rs:500) only asserts `v.action == "zoom"` — it never checks `v.box_`.
`zoom_loop_done_on_first_vote` (zoom.rs:577) uses `action:"done"` (no box
needed). No test exercises the zoom-vote → crop path.

**Fix:** add `#[serde(rename = "box")]` to the `box_` field:
```rust
#[serde(default, rename = "box")]
box_: Option<Vec<f64>>,
```
Then add a test asserting `parse_vote(...).unwrap().box_ == Some(vec![0.1,0.2,0.3,0.4])`.

---

### C2. The zoom while-loop re-reads the first crop and discards new crops
**File:** `src/tool/agent/image_tools/zoom.rs:277-321`

Once C1 is fixed, the while-loop (the "Fine: continue zooming" path) is itself
broken. Tracing `zoom_loop` with `detail_level != Auto` (or Auto with low
confidence):

- Line 253: `crop_url` is bound to the **first** voted crop.
- Line 277: `while rounds < max_rounds` enters.
- Line 281: `describe_image_data_url(vision, &crop_url, ...)` — re-reads the
  **same first crop** every iteration (the variable is never reassigned).
- Line 294: `vote_crop_url(&v, full_img, &mut regions)` computes a **new** crop
  URL into `url`.
- Line 296-297: `let _ = describe_image_data_url(vision, &url, ...)` — calls the
  vision model on the new crop but **discards the result** (`let _`).
- Line 299: `continue` — loops back, re-reading the first crop again.

So progressive zoom never works: subsequent rounds re-read the first crop, and
when the model votes to zoom further, the new crop is read but its answer is
thrown away. The loop can never converge on a progressively-zoomed view.

**Fix:** reassign `crop_url` from each new vote (make it `mut`), and use the new
crop's answer instead of discarding it. The loop should be:
```
crop_url = new_crop_url;   // reassign
let answer = describe_image_data_url(vision, &crop_url, ...).await?;
// parse answer, check done/confidence, continue or return
```

*(Currently masked by C1 — `vote_crop_url` always returns `None`, so this loop is
never entered. But it must be fixed alongside C1.)*

---

## BUGS

### B1. Blocking image decode/resize/crop/encode runs on the async runtime, not `spawn_blocking`
**File:** `src/tool/agent/image_tools/zoom.rs:115, 139, 157-169, 193-194, 253, 370-371**

The plan (step 3) explicitly required: *"All image decode/crop/re-encode runs in
`tokio::task::spawn_blocking` (mirror describe_image_file's pattern); the vision
calls stay async."* The implementation does **not** use `spawn_blocking` for any
image operation in the zoom path:

| Operation | Location | Runs on |
|---|---|---|
| `image::load_from_memory(bytes)` | zoom.rs:115 | async runtime |
| `full_img.resize(1568, 1568, Lanczos3)` | zoom.rs:159 | async runtime |
| `full_img.crop_imm(px,py,pw,ph)` | zoom.rs:433 | async runtime |
| `img.write_to(&mut buf, Png)` (re-encode) | zoom.rs:175 | async runtime |

Lanczos3 resize on a large screenshot (e.g. 4K) + PNG re-encode can take
hundreds of milliseconds, blocking the entire async runtime and stalling all
other tasks. The file-read path (`load_image_data_url_and_bytes`,
`describe_image_file`) correctly uses `spawn_blocking`, but the zoom path does
not — an inconsistency.

**Fix:** wrap the decode + resize + crop + re-encode sequence in
`tokio::task::spawn_blocking`, passing the raw bytes in and returning the
data URL out. The vision `describe_image` call stays async.

### B2. `image_ui_diff` cannot perform a real visual diff — each image is analyzed alone
**File:** `src/tool/agent/image_tools/tools.rs:530-566`

The `UiDiff` prompt (prompts.rs:124) says *"Compare two UI screenshots
(first=A/before, second=B/after); describe what changed."* But `execute`
(tools.rs:530-558) calls `analyze_with_zoom` **separately** on each image, and
`analyze_with_zoom` → `describe_image_data_url` → `ImageDescriber::describe_image`
takes a **single** `image_url`. The model never sees both images together, so it
cannot compare them.

The "combined" output (tools.rs:563-566) is just two independent descriptions
concatenated:
```
# Image A (before)
{description of A alone}

# Image B (after)
{description of B alone}
```

This is two separate analyses, not a diff. The tool's name, description, and
prompt all promise a diff but deliver two standalone descriptions. The test
(`image_ui_diff_execute_success`, tools.rs:744) only checks the output contains
"Image A"/"Image B" and 2 calls were made — it doesn't verify an actual diff.

**Fix options:** (a) extend `ImageDescriber` to accept multiple image URLs and
send both in one call (the vision-mcp spec supports this); or (b) if the trait
stays single-image, change the prompt to describe each image independently
(rather than "compare two") and rename the output sections to be honest about
what it produces. As-is, the tool misrepresents its capability.

### B3. The overview pass sends contradictory instructions (markdown sections vs "ONLY JSON")
**File:** `src/tool/agent/image_tools/zoom.rs:235-237`

The overview pass combines the task prompt and the control prompt:
```rust
let overview_prompt = format!("{task_prompt}\n\n{control}");
```

`task_prompt` (from `build_prompt`) instructs: *"Respond with these sections:
## Overview / ## Notes ..."*. `control` (from `zoom_control_prompt`) instructs:
*"Respond with ONLY a JSON object on one line, no prose."* These are
contradictory. If the model follows the task prompt (markdown sections),
`parse_vote` fails (no JSON) and the raw markdown is returned as the answer —
losing the vote. If it follows the control prompt (JSON), the section-header
instruction is wasted.

**Fix:** the overview pass should ask only for the vote (control prompt), not
the task's section format. The section format should apply only to the final
"done" answer. Or, restructure so the model returns JSON with an `answer` field
that itself contains the markdown sections.

### B4. `vote_crop_url` records the region before verifying the crop succeeded
**File:** `src/tool/agent/image_tools/zoom.rs:366-376`

```rust
regions.push(Region { box_: bbox, note: vote.region.clone() });  // recorded now
let crop = crop_bbox(full_img, &bbox);
let url = encode_png_data_url(&crop);
if url.is_empty() {
    None   // ← region was already pushed, but crop failed
} else {
    Some(url)
}
```

If `encode_png_data_url` returns empty (re-encode failure), the region is still
in `regions`. The returned `ZoomResult.regions` / tool `data.regions` will
contain regions that were never actually zoomed into — inaccurate metadata.

**Fix:** push to `regions` only after the crop/encode succeeds (return `None`
before pushing).

---

## MINOR

### M1. `parse_vote` called twice on the same `crop_answer`
**File:** `src/tool/agent/image_tools/zoom.rs:261, 264`

```rust
if let Some(c) = parse_vote(&crop_answer).and_then(|v| v.confidence) {  // parse #1
    if c >= AUTO_CONFIDENCE_THRESHOLD {
        return Ok(ZoomResult {
            markdown: parse_vote(&crop_answer).and_then(|v| v.answer)  // parse #2
```

`crop_answer` is parsed twice. Bind the first `parse_vote` result to a variable
and reuse it.

### M2. `parse_region`'s `_w` / `_h` parameters are dead code
**File:** `src/tool/agent/image_tools/zoom.rs:389`

`fn parse_region(spec: &str, _w: u32, _h: u32)` — the width/height params are
never used (named regions return fixed bboxes; bbox parsing clamps to 0..1, not
to image bounds). They're silenced with the `_` prefix. The constitution says
"fix the root cause (remove the dead code)". Either remove the params (and
update callers) or actually use them to clamp the bbox to image bounds.

### M3. `image_ui_diff` reports only image A's confidence
**File:** `src/tool/agent/image_tools/tools.rs:570`

`"confidence": res_a.confidence` — image B's confidence is dropped. Consider
`res_a.confidence.min(res_b.confidence)` or reporting both.

### M4. `overview_data_url` re-encodes small images to PNG unnecessarily
**File:** `src/tool/agent/image_tools/zoom.rs:157-169`

When the image is already small (no resize needed), it's still cloned, converted
to RGBA8, and re-encoded as PNG — changing the format (e.g. a JPEG becomes a
larger PNG) and wasting work. The original `data_url` is available in
`analyze_with_zoom` and could be sent directly for the overview pass when no
resize is needed.

---

## Verified OK (no findings)

- **Sandbox validation + magic-byte sniff preserved** on
  `load_image_data_url_and_bytes` (mod.rs:166-184): `sandbox.validate` → ext→MIME
  → `std::fs::read` → `magic_matches` → reject on mismatch. Identical to the old
  `load_image_data_url`. Exfiltration gap stays closed. ✓
- **`NeedsApproval` posture** — all 7 tools return `SafetyLevel::NeedsApproval`. ✓
- **No `#[allow(...)]` suppressions** anywhere in the new files. ✓
- **Doc comments** present on all public items (`mime_from_ext`, `magic_matches`,
  `load_image_data_url`, `describe_image_data_url`, `describe_image_file`,
  `DEFAULT_DESCRIBE_PROMPT`, all 7 tool structs + their `new` methods,
  `ImageTool`, `PromptArgs`, `build_prompt`, `DetailLevel`, `Region`,
  `ZoomResult`, `analyze_with_zoom`, `parse_detail_level`). ✓
- **No lock held across `.await`** — `MockDescriber` uses `StdMutex`, acquires +
  releases within the synchronous `push` (mod.rs:226-229); the `describe_image`
  method is async but the lock scope is synchronous. ✓
- **Mock tests don't hit network** — `MockDescriber` returns canned strings. ✓
- **`thinking` field warning** (tools.rs:89-95) — intentional advisory no-op:
  the schema says "no-op on our vision backend", and `thinking=true` pushes a
  warning so the caller knows the flag was accepted but had no effect. Working as
  designed, not a bug. ✓
- **Crops come from the full-res original** — `crop_bbox` (zoom.rs:419-434)
  operates on `full_img` (the decoded original), not the downsampled overview.
  ✓ (The crop *logic* is correct; the *triggering* of crops is broken by C1.)
- **Decode-fail fallback** (zoom.rs:115-130) — on `load_from_memory` error, falls
  back to a single overview pass with a warning. Correct. ✓
- **Region single-pass** (zoom.rs:132-135) — a `region` param short-circuits to
  `crop_and_describe` (single pass on the crop). Correct. ✓
- **`parse_vote` lenient JSON extraction** (zoom.rs:438-458) — strips ```json
  fences, tries direct parse, falls back to first `{`...last `}` extraction.
  Correct (though it can't compensate for C1's field-name mismatch). ✓
- **Registration** (factory.rs:556-563) — all 7 tools registered against the
  swappable slot; empty-slot test checks `image_analysis` is present. ✓
- **Prompt.rs / spawn.rs / agent.rs / browser/mod.rs** — all `describe_image`
  references correctly updated to the 7 `image_*` names / new module path. ✓

---

## Required fixes before merge

1. **C1** — add `#[serde(rename = "box")]` to `ZoomVote.box_` + a test asserting
   the box is parsed. *(Without this, the zoom loop is entirely non-functional.)*
2. **C2** — fix the while-loop to reassign `crop_url` and use (not discard) new
   crops' answers. *(Currently masked by C1; will surface once C1 is fixed.)*
3. **B1** — wrap image decode/resize/crop/encode in `spawn_blocking` (the plan
   required this; large-image Lanczos3 + PNG encode blocks the async runtime).
4. **B2** — decide: extend `ImageDescriber` for multi-image, or make `image_ui_diff`
   honest about producing two independent descriptions (not a diff).
5. **B3** — resolve the contradictory overview-pass instructions (markdown sections
   vs "ONLY JSON").
6. **B4** — push to `regions` only after crop/encode succeeds.

Recommended (minor): M1–M4.

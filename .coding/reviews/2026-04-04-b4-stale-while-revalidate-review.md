# Review — B4: stale-while-revalidate model picker refresh

**Date:** 2026-04-04
**Plan:** Fix B4: stale-while-revalidate model picker refresh
**Scope:** ALL uncommitted changes (`git diff HEAD` + `git status`)

## Files reviewed

B4 plan changes (frontend):
- `frontend/src/components/settings/sections/ModelPickerDropdown.tsx`
- `frontend/src/components/settings/sections/EndpointCard.tsx`
- `frontend/src/components/settings/sections/VisionSection.tsx`

Pre-existing uncommitted changes (reviewed too):
- `src/provider/trace.rs` (`floor_char_boundary` → manual `is_char_boundary` loop)
- `package-lock.json` (removal of `libc` fields on optional Linux binaries)
- `src-tauri/Cargo.toml` (line-ending normalization only — no content diff)
- `.coding/plans/stack.json` + untracked `.coding/plans/1a378eea-….md` (bookkeeping)

## Correctness — no findings

The stale-while-revalidate logic is correct. The four render blocks in
`ModelPickerDropdown.tsx` (lines 93, 96, 99, 104) form a sound state matrix:

| status | models.length | rendered |
|---|---|---|
| loading | 0 | "Fetching models…" (line 93) |
| loading | >0 | stale list (line 104) — the intended fix |
| ready | 0 | "No models available/match" (line 99) |
| ready | >0 | list (line 104) |
| error | 0 | error message (line 96) |
| error | >0 | error message only (line 104 guarded out) |

Verified each focus area:

- **Stale list stays visible during refetch:** `onRefresh` no longer clears
  `modelsCache`/`visionModels`/`modelsCacheKey` (EndpointCard.tsx:476-478,
  540-542; VisionSection.tsx:278-280), and `fetchServerModels(true)` /
  `fetchVisionModels(true)` only call `setFetchState({status:"loading"})`
  (EndpointCard.tsx:96; VisionSection.tsx:86) without touching the cache. So
  during a refetch `status==="loading"` while `models.length>0` → the list
  block (`models.length > 0 && status !== "error"`, line 104) renders the
  previous list. The refresh icon still spins (line 88). ✓
- **Stale list never shows alongside an error:** double-guaranteed. (1) The
  error path clears the cache — `setModelsCache([])` (EndpointCard.tsx:105)
  and `setVisionModels([])` (VisionSection.tsx:95) — so `models.length===0`
  on error. (2) Even if the cache were non-empty, the list condition
  `status !== "error"` (line 104) excludes the error state. ✓
- **Loading + empty shows "Fetching models…":** first-ever fetch (or a refetch
  after a prior empty result) has `models.length===0` while loading → line 93
  fires. ✓
- **Ready + empty shows "No models available/match":** line 99 unchanged,
  still gated on `status === "ready" && models.length === 0`. ✓
- **`force=true` still bypasses the cache guard:** the guard at
  EndpointCard.tsx:88 / VisionSection.tsx:82 is `if (!force && …)`, so the
  dropped `setModelsCacheKey("")` was never needed to trigger a refetch —
  confirming the plan's rationale. ✓

One non-blocking observation (NOT a bug): the list condition broadened from
`status === "ready"` to `models.length > 0 && status !== "error"`, so a
hypothetical `status === "idle"` with non-empty `models` would now render the
list. This state is unreachable while the dropdown is open — opening the
picker (`openRowPicker`/`openAddPicker`/VisionSection effect) always calls a
fetch that synchronously moves `status` to `loading`/`ready`/`error`, never
leaving it `idle` — and even if reached, showing cached data is harmless. No
action needed.

## Bugs — no findings

- **No unused state:** all dropped-clear setters remain referenced in the
  success/error/reset paths. `setModelsCache` (EndpointCard.tsx:100, 105, 119),
  `setModelsCacheKey` (101, 106, 120), `setVisionModels` (VisionSection.tsx:90,
  95, 107), `setModelsCacheKey` (91, 96, 108). `modelsCache`/`visionModels` are
  still read via `filteredModels` passed as `models={…}` (EndpointCard.tsx:475,
  539; VisionSection.tsx:277). ✓
- **No new race / stale closure:** `onRefresh` handlers are inline arrows
  capturing `fetchServerModels`/`fetchVisionModels` from component scope, same
  as before. The `fetchReqId` race guard (EndpointCard.tsx:95,99,104;
  VisionSection.tsx:85,89,94) is unchanged. ✓

## Security — no findings

No changes to API-key handling, logging, or the data flow into the dropdown.
The model list and `apiKey` plumbing are untouched. No new secret-logging
surfaces. ✓

## Constitution compliance — no findings

- **Doc comments:** `ModelPickerDropdown` doc comment updated to describe
  stale-while-revalidate (lines 3-11). `append_response` in `trace.rs` already
  has an accurate doc comment (lines 245-250). All public functions documented. ✓
- **`#![deny(warnings)]` / no `#[allow(...)]`:** no suppressions added. The
  `trace.rs` change replaces the unstable nightly-only `str::floor_char_boundary`
  with a stable manual loop (`while cut > 0 && !text.is_char_boundary(cut) {
  cut -= 1; }`, lines 261-264) — this is the textbook equivalent of
  `floor_char_boundary` (largest byte index `≤ remaining` on a char boundary,
  or 0) and fixes a build on stable Rust. No `#[allow]` introduced. ✓
- **Line endings:** frontend diffs are LF (preserved). The `src-tauri/Cargo.toml`
  "LF will be replaced by CRLF" warning is a pre-existing git autocrlf artifact
  with no content diff — not introduced by this plan. ✓

## Pre-existing changes (non-B4)

- **`src/provider/trace.rs`:** correct stable reimplementation of
  `floor_char_boundary`; behavior identical (verified edge cases: `remaining`
  already on a boundary → `cut` unchanged; `remaining === 0` → empty slice). ✓
- **`package-lock.json`:** removes `libc: ["glibc"|"musl"]` fields from optional
  Linux-only native-binary entries. Benign normalization (likely an npm-version
  artifact); `os`/`cpu` fields still gate optional deps, and this is a Windows
  project so Linux variant selection is irrelevant. No resolution impact. ✓
- **`src-tauri/Cargo.toml`:** no content change (line-ending normalization only). ✓
- **`stack.json` / plan file:** bookkeeping only. ✓

## Verdict

**No findings.** The B4 stale-while-revalidate fix is correct, introduces no
bugs, security issues, or constitution-compliance problems, and the
pre-existing uncommitted changes are also clean. The build is warning-free
under `#![deny(warnings)]` (main agent confirmed `npm run build`, `vitest`,
and `cargo test` all green).

# Review: Clickable full-resolution screenshot viewer with Close + Copy

**Date:** 2026-08-22
**Scope:** ALL uncommitted changes (`git status` / `git diff HEAD`): `frontend/src/components/chat/ToolImage.tsx`, `frontend/src/components/chat/ToolImage.test.ts`, `README.md`, plus `.coding/` bookkeeping (plan stack/state — not code).
**Verdict:** Approve with minor fixes. Four findings (2 low, 2 trivial); no correctness blockers, no security issues.

---

## Findings

### LOW 1 — "Copied" is shown even when both clipboard writes fail
**File:** `frontend/src/components/chat/ToolImage.tsx:95-106`

```ts
} catch {
  // Fallback: copy the data URL as text (paste-able into a browser).
  await navigator.clipboard.writeText(dataUrl).catch(() => {});
}
setCopied(true);
setTimeout(() => setCopied(false), 2000);
```

If `navigator.clipboard.write` rejects **and** the `writeText` fallback also fails (its rejection is swallowed by `.catch(() => {})`), `setCopied(true)` still runs unconditionally — the button displays "Copied ✓" although nothing reached the clipboard. That's false user feedback for exactly the failure case the fallback exists for (e.g. WKWebView permission denial).

Related edge: if `navigator.clipboard` itself is `undefined` (non-secure context; not expected under Tauri's secure-origin protocol, but possible in a plain dev browser), the member access `navigator.clipboard.writeText` in the catch block throws synchronously, escaping `copy()` as an unhandled promise rejection at the `void copy()` call site (line 119).

**Suggested fix:** track success and only show the check mark on an actual copy, e.g.

```ts
const copy = async () => {
  let done = true;
  try {
    await navigator.clipboard.write([
      new ClipboardItem({ "image/png": dataUrlToBlob(dataUrl) }),
    ]);
  } catch {
    try {
      await navigator.clipboard.writeText(dataUrl);
    } catch {
      done = false;
    }
  }
  if (done) {
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }
};
```

(The nested try/catch also fixes the undefined-`clipboard` rethrow.)

### LOW 2 — `dataUrlToBlob` hard-codes `image/png`, but the backend can emit jpeg/gif/webp/bmp data URLs
**File:** `frontend/src/components/chat/ToolImage.tsx:72, 98`; backend: `src-tauri/src/ipc/files.rs:73-74`

`read_image_data_url` derives the MIME from the file extension (`mime_from_ext`; the allowlist per the error text is png, jpg, jpeg, gif, webp, bmp) and returns `data:{mime};base64,…`. The `image_*` vision tools can analyze non-PNG files, so a thumbnail/lightbox can hold e.g. a `data:image/jpeg;base64,…` URL. The copy path then builds `new Blob([bytes], { type: "image/png" })` and a `ClipboardItem({ "image/png": … })` whose payload is JPEG bytes.

Impact is bounded: Chromium (WebView2) and WebKit (WKWebView) generally decode clipboard image blobs by content and transcode on write, and if `write()` rejects, the `writeText` fallback fires — so the worst case is degraded (a data-URL string lands on the clipboard instead of an image), not a crash. The user's primary case (screenshots from `browser_screenshot`/`game_screenshot`) is always PNG and unaffected.

**Suggested fix (pick one):** parse the MIME from the data-URL header (`dataUrl.slice(dataUrl.indexOf(":") + 1, dataUrl.indexOf(";"))`) and, when it isn't `image/png`, transcode via an offscreen canvas (`drawImage` → `toBlob("image/png")`) before building the ClipboardItem — or, if you accept the engine-transcoding reliance, say so in the `dataUrlToBlob` doc comment so the assumption is explicit.

### TRIVIAL 3 — Missing `DialogDescription` → Radix dev-console warning
**File:** `frontend/src/components/chat/ToolImage.tsx:110`

`DialogContent` contains a `DialogTitle` but no `DialogDescription` and no `aria-describedby`. `@radix-ui/react-dialog` logs `Missing Description or aria-describedby={undefined} for {DialogContent}` in development builds. Every other dialog in the app includes a description: AboutDialog.tsx:107, MergeToMainDialog.tsx:65, SafetyToggleDialog.tsx:56, and ProjectPicker.tsx:264 (which uses `className="sr-only"`).

**Suggested fix:** follow the ProjectPicker precedent — add `<DialogDescription className="sr-only">Full-resolution view of {alt}</DialogDescription>` — or pass `aria-describedby={undefined}` to `DialogContent` to opt out explicitly.

### TRIVIAL 4 — The open-button's `title` tooltip is shadowed by the inner img's `title`
**File:** `frontend/src/components/chat/ToolImage.tsx:181` vs `:188`

The button sets `title="Open image in full resolution"`, but the nested `<img title={path}>` is the element actually under the cursor, and the nearest titled ancestor wins — so hovering always shows the path, never the intended hint. Harmless (the button's `aria-label="Open image viewer"` is still announced correctly, and the path tooltip is arguably useful), but the button title is dead code as shipped.

**Suggested fix:** drop `title={path}` from the img (the `alt` remains for a11y) or accept the path tooltip and remove the button's `title`.

---

## Verified clean (checked per review brief)

- **Malformed data URL:** `dataUrlToBlob(dataUrl)` is evaluated *inside* the try block (line 98), so an `atob` throw on invalid base64 (or a URL with no comma) lands in the catch → `writeText` fallback. ✓
- **Natural size / full resolution:** `className="max-w-none"` (line 143) overrides Tailwind preflight's `max-width: 100%` (utility layer beats base; preflight confirmed active in `frontend/src/styles/globals.css:1`). The fixed-width `w-[min(90vw,1400px)] max-h-[90vh] flex-col` panel (line 110) + `min-h-0 flex-1 overflow-auto` body (line 142) yields scrollbars when the image exceeds the box. Tailwind arbitrary value `w-[min(90vw,1400px)]` is valid syntax. ✓
- **Close paths:** X button `onClick={onClose}` (line 133); Esc/backdrop → Radix `onOpenChange(false)` → `onClose()` (line 109). Lightbox is mounted only while open (line 192), so `useBrowserOverlay(open)` enter/exit balance via mount/unmount; the backend overlay counter is saturating (useBrowserOverlay.ts doc), so a stray exit can't wedge the WebView2 hidden. ✓
- **Fail-quiet preserved:** early `return null` while `dataUrl === null` (line 175); the open button (and lightbox) only render after a successful load. ✓
- **Layout stability:** the thumbnail's container (`Message.tsx:606`) is `flex flex-wrap gap-2` with no `> img`-scoped styles; the button is inline-block (UA default) wrapping a preflight-block img — no baseline-gap artifact, footprint essentially unchanged from the bare img. ✓
- **Security:** clipboard write fires only from the explicit Copy click (user gesture); no new IPC, no new file access; the `atob` decode path is purely local — no `fetch(dataUrl)`, so the CSP `default-src 'self'` concern doesn't arise (the doc comment at lines 58-63 documents this). Data URLs originate from the backend-hardened `read_image_data_url` (sandbox validation, extension allowlist, magic-byte match, 20 MiB cap — files.rs:65-94). ✓
- **Tests non-vacuous:** all 13 assertions across the 5 new tests were spot-checked verbatim against ToolImage.tsx (`cursor-zoom-in`, `onClick={() => setOpen(true)}`, `aria-label="Open image viewer"`, `useBrowserOverlay(open)`, `<Dialog open={open}`, `navigator.clipboard.write([`, `new ClipboardItem({ "image/png": dataUrlToBlob(dataUrl) })`, `navigator.clipboard.writeText(dataUrl)`, `className="max-w-none"`, `overflow-auto`, `aria-label="Close image viewer"`, `onClick={onClose}`) — removing the feature removes the strings, so the tests fail. The `?raw` static-contract style matches 8 existing suites (BacklogView, InflightBar, SourceEditor, GraphView, ChatSection, SoundsSection, …). I could not re-run the suite (read-only), but 5 new `it()` blocks matches the claimed 481 → 486. ✓
- **Constitution — docs sync:** README.md:56 accurately describes the shipped behavior ("Clicking a tool image opens it at full resolution in a viewer with Copy-image and Close buttons"). Exported `ImageLightbox` and helper `dataUrlToBlob` have doc comments. ✓
- **Constitution — multi-platform neutrality:** pure frontend change (`git status`: only ToolImage.tsx/.test.ts, README.md, `.coding/` bookkeeping — no Rust touched, no `cfg(windows)`, no `#[allow]`). `navigator.clipboard`/`ClipboardItem`/`atob`/`Blob` are standard web APIs in both WebView2 and WKWebView; any `write()` rejection reaches the `writeText` fallback via the catch. ✓

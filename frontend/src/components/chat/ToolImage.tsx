// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useState } from "react";
import { Check, Copy, X } from "lucide-react";
import { readImageDataUrl } from "../../lib/tauri";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "../ui/dialog";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";

/**
 * Resolve the image(s) a tool call produced, for inline rendering in the chat:
 * - the 7 `image_*` vision tools analyze files given in their args → the
 *   `image` key (all single-image tools), or `image_a` + `image_b` for
 *   `image_ui_diff` (both thumbnails render, before/after);
 * - any tool whose name ends `_screenshot` writes a PNG and reports the
 *   sandbox-relative path in the structured result `data.path` — today
 *   `browser_screenshot` (Windows live-tab capture, writes
 *   `.coding/browser/screenshots/browser-<ts>.png`) and the headless
 *   `offscreen_browser_screenshot` (cross-platform, writes
 *   `.coding/browser/screenshots/<page>-<ts>.png` with the page id, e.g.
 *   `active`); a fallback scans the output text for
 *   `.coding/browser/screenshots/….png` so an older/edge-case payload still
 *   resolves;
 *
 * Returns an empty array for failed calls, non-image tools, or anything
 * unparseable — the ToolCard then simply renders no image (fail-quiet; the
 * tool's text result is the source of truth).
 */
export function toolImagePaths(
  name: string,
  argsJson: string,
  result: { success: boolean; output: string; data?: unknown } | null,
): string[] {
  if (!result || !result.success) return [];
  if (name.startsWith("image_")) {
    try {
      const parsed = JSON.parse(argsJson) as Record<string, unknown>;
      if (name === "image_ui_diff") {
        const a = parsed.image_a;
        const b = parsed.image_b;
        const out: string[] = [];
        if (typeof a === "string" && a.length > 0) out.push(a);
        if (typeof b === "string" && b.length > 0) out.push(b);
        return out;
      }
      const image = parsed.image;
      if (typeof image === "string" && image.length > 0) return [image];
    } catch {
      // unparseable args → no image
    }
    return [];
  }
  if (name.endsWith("_screenshot")) {
    const data = result.data as { path?: unknown } | undefined;
    if (typeof data?.path === "string" && data.path.length > 0) return [data.path];
    const m = result.output.match(/\.coding\/browser\/screenshots\/[\w.-]+\.png/);
    return m ? [m[0]] : [];
  }
  return [];
}

/**
 * Decode a `data:<mime>;base64,...` URL into a `Blob` for the clipboard,
 * preserving the payload's actual MIME type (the backend emits png, jpeg,
 * gif, webp, or bmp per the file extension — see `read_image_data_url`).
 * Decoding via `atob` (not `fetch(dataUrl)`) keeps the copy path independent
 * of the app's CSP (`default-src 'self'` would block fetching a `data:` URL
 * from script context). Clipboard engines decode image blobs by content and
 * transcode on write; a non-PNG MIME that `write()` rejects falls through to
 * the caller's writeText fallback.
 */
function dataUrlToBlob(dataUrl: string): Blob {
  const comma = dataUrl.indexOf(",");
  const header = comma >= 0 ? dataUrl.slice(0, comma) : dataUrl;
  // Header shape: "data:<mime>;base64". Default to png when unparseable.
  const mimeMatch = /^data:([^;,]+)/.exec(header);
  const mime = mimeMatch?.[1] ?? "image/png";
  const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return new Blob([bytes], { type: mime });
}

/**
 * Full-resolution image viewer: a Radix Dialog showing the image at its
 * natural size (scrollable when larger than the viewport) with a Copy-image
 * button and a Close button. `useBrowserOverlay` hides the native child
 * WebView2 while open, like the other full-viewport modals.
 */
export function ImageLightbox({
  dataUrl,
  alt,
  open,
  onClose,
}: {
  dataUrl: string;
  alt: string;
  open: boolean;
  onClose: () => void;
}) {
  useBrowserOverlay(open);
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    let done = true;
    try {
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": dataUrlToBlob(dataUrl) }),
      ]);
    } catch {
      // Fallback: copy the data URL as text (paste-able into a browser).
      try {
        await navigator.clipboard.writeText(dataUrl);
      } catch {
        done = false;
      }
    }
    // Only show the success state when a copy actually reached the
    // clipboard — a double failure (e.g. no clipboard API in a dev browser)
    // must not lie with a "Copied" check mark.
    if (done) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o: boolean) => { if (!o) onClose(); }}>
      <DialogContent className="flex max-h-[90vh] w-[min(90vw,1400px)] flex-col rounded-lg border border-border bg-bg-secondary shadow-2xl">
        {/* Header: filename + Copy + Close. */}
        <div className="flex shrink-0 items-center justify-between gap-3 border-b border-border px-4 py-2">
          <DialogTitle className="min-w-0 truncate text-xs text-[color:var(--text-muted)]">
            {alt}
          </DialogTitle>
          <DialogDescription className="sr-only">
            Full-resolution view of {alt}
          </DialogDescription>
          <div className="flex shrink-0 items-center gap-1">
            <button
              type="button"
              onClick={() => void copy()}
              title="Copy image to clipboard"
              aria-label="Copy image to clipboard"
              className="flex items-center gap-1 rounded px-2 py-1 text-xs text-[color:var(--text-muted)] transition-colors hover:bg-bg-tertiary hover:text-[color:var(--text-primary)]"
            >
              {copied ? (
                <Check className="h-3.5 w-3.5 text-cyan-400" />
              ) : (
                <Copy className="h-3.5 w-3.5" />
              )}
              {copied ? "Copied" : "Copy"}
            </button>
            <button
              type="button"
              onClick={onClose}
              aria-label="Close image viewer"
              className="rounded p-1 text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
        {/* Body: natural-size image, scrollable. */}
        <div className="min-h-0 flex-1 overflow-auto">
          <img src={dataUrl} alt={alt} className="max-w-none" />
        </div>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Lazily load `path` (sandbox-relative, forward slashes) through the
 * `read_image_data_url` IPC and render it as an inline thumbnail. Fail-quiet:
 * loading or IPC errors render nothing (the tool's text result still shows),
 * so a missing/oversized image never breaks the chat.
 */
export function ToolImage({ path }: { path: string }) {
  const [dataUrl, setDataUrl] = useState<string | null>(null);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let disposed = false;
    setDataUrl(null);
    readImageDataUrl(path)
      .then((url) => {
        if (!disposed) setDataUrl(url);
      })
      .catch(() => {
        // Fail-quiet — see doc comment.
      });
    return () => {
      disposed = true;
    };
  }, [path]);

  if (dataUrl === null) return null;
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        title="Open image in full resolution"
        aria-label="Open image viewer"
        className="cursor-zoom-in"
      >
        <img
          src={dataUrl}
          alt={path}
          className="max-h-40 rounded border border-border object-contain"
        />
      </button>
      {open && (
        <ImageLightbox
          dataUrl={dataUrl}
          alt={path}
          open={open}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

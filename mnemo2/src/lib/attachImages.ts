// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Image-attachment pipeline for pasted/dropped files (mem-perf review LOW 4).
 *
 * Pasted screenshots used to be kept as raw full-size data URLs — a 4K
 * screenshot is ~5-10 MB of base64 that then lived in the zustand store AND
 * the DOM for the whole session (the transcript cap is entry-COUNT only).
 * This module caps the damage at the source: every attached image is
 * downscaled to a bounded long edge and re-encoded as JPEG before it ever
 * reaches the store, so both the transcript payload and what is sent to the
 * model stay small. (The transcript-side rolling byte budget that evicts
 * payloads from OLD entries lives in `agentState.ts` — the two mechanisms
 * complement: this bounds each image, that bounds the accumulation.)
 */

/** Long-edge cap for attached images, in pixels. Matches common vision-API
 * input limits (OpenAI/Anthropic recommend ~1568 px on the long edge) — the
 * model cannot resolve more, so anything larger is pure RAM waste. */
export const MAX_IMAGE_LONG_EDGE = 1568;

/** Data URLs at or under this many characters skip the decode + re-encode
 * entirely: small images (icons, small screenshots) are already cheap to
 * retain, and skipping the canvas round-trip preserves PNG alpha. */
export const SMALL_IMAGE_DATA_URL_CHARS = 512 * 1024;

/** JPEG quality for re-encoded attachments. 0.85 keeps screenshots crisp
 * (text stays readable) while typically shrinking them 5-20x. */
export const ATTACH_JPEG_QUALITY = 0.85;

/**
 * Whether a data URL is small enough to keep as-is — no decode, no
 * re-encode, no quality loss. Pure so it is unit-testable in the node env.
 */
export function isSmallDataUrl(dataUrl: string): boolean {
  return dataUrl.length <= SMALL_IMAGE_DATA_URL_CHARS;
}

/**
 * Convert a File (image) to a base64 data URL via `FileReader`.
 * Shared by the chat input (InputBar) and the backlog input/editor
 * (BacklogView) paste + drop paths.
 */
export function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

/**
 * Decode a data URL into an `HTMLImageElement` (natural dimensions available
 * once loaded). Rejects on decode failure.
 */
function loadImage(dataUrl: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("failed to decode image"));
    img.src = dataUrl;
  });
}

/**
 * Downscale an image data URL for attachment: cap the long edge at
 * [`MAX_IMAGE_LONG_EDGE`] and re-encode as JPEG (quality
 * [`ATTACH_JPEG_QUALITY`]).
 *
 * Behavior:
 * - Non-image data URLs and small ones (≤ [`SMALL_IMAGE_DATA_URL_CHARS`]
 *   chars) are returned unchanged — no decode, no quality loss.
 * - Larger images are drawn onto a canvas scaled to the long-edge cap and
 *   re-encoded as JPEG. JPEG has no alpha, so the canvas is filled white
 *   first (transparent regions would otherwise render black).
 * - If the re-encode somehow produces a LARGER string than the original
 *   (rare, e.g. a tiny noisy PNG), the original is kept.
 * - Any failure (decode error, zero natural dimensions — e.g. an SVG without
 *   intrinsic size —, missing canvas context) degrades gracefully: the
 *   ORIGINAL data URL is returned. An image is never lost or corrupted by
 *   this pass; worst case it stays full-size.
 */
export async function downscaleDataUrl(dataUrl: string): Promise<string> {
  if (!dataUrl.startsWith("data:image/")) return dataUrl;
  if (isSmallDataUrl(dataUrl)) return dataUrl;
  try {
    const img = await loadImage(dataUrl);
    // Zero natural dimensions (dimensionless SVG, broken file) — the canvas
    // math below would collapse to 1x1; keep the original instead.
    if (img.naturalWidth === 0 || img.naturalHeight === 0) return dataUrl;
    const longEdge = Math.max(img.naturalWidth, img.naturalHeight);
    const scale = longEdge > MAX_IMAGE_LONG_EDGE ? MAX_IMAGE_LONG_EDGE / longEdge : 1;
    const width = Math.max(1, Math.round(img.naturalWidth * scale));
    const height = Math.max(1, Math.round(img.naturalHeight * scale));
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext("2d");
    if (!ctx) return dataUrl;
    // JPEG has no alpha channel — composite onto white so transparent
    // regions render white instead of black.
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, width, height);
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(img, 0, 0, width, height);
    const reencoded = canvas.toDataURL("image/jpeg", ATTACH_JPEG_QUALITY);
    return reencoded.length < dataUrl.length ? reencoded : dataUrl;
  } catch {
    // Decode or canvas failure — degrade gracefully to the original.
    return dataUrl;
  }
}

/**
 * Read an image File into an attachment-ready data URL: read as a data URL,
 * then downscale/re-encode when it is large. This is the single entry point
 * for paste/drop image attachment in the app.
 */
export async function fileToAttachedDataUrl(file: File): Promise<string> {
  return downscaleDataUrl(await fileToDataUrl(file));
}

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Attachment-image downscale pipeline (mem-perf review LOW 4): pasted images
 * are capped at a 1568 px long edge and re-encoded as JPEG before they ever
 * reach the store, so a 4K screenshot (~5-10 MB base64) no longer lives in
 * the zustand store AND the DOM for the whole session.
 *
 * vitest runs in `node` env (no DOM), so the canvas path itself is covered
 * by source contracts (the established pattern for DOM-bound code — see
 * InputBar.test.ts); the pure decision logic and the graceful-fallback
 * contract (an image is NEVER lost — worst case it stays full-size) are
 * covered by real unit tests.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ATTACH_JPEG_QUALITY,
  MAX_IMAGE_LONG_EDGE,
  SMALL_IMAGE_DATA_URL_CHARS,
  downscaleDataUrl,
  isSmallDataUrl,
} from "./attachImages";
import source from "./attachImages.ts?raw";
import inputBarSource from "../components/layout/InputBar.tsx?raw";
import backlogSource from "../components/views/BacklogView.tsx?raw";
import messageSource from "../components/chat/Message.tsx?raw";

describe("attachment image constants", () => {
  it("caps the long edge at the vision-API input limit (1568 px)", () => {
    expect(MAX_IMAGE_LONG_EDGE).toBe(1568);
  });

  it("keeps the small-image fast path at 512 KiB of data-URL chars", () => {
    expect(SMALL_IMAGE_DATA_URL_CHARS).toBe(512 * 1024);
  });

  it("re-encodes at JPEG quality 0.85", () => {
    expect(ATTACH_JPEG_QUALITY).toBe(0.85);
  });
});

describe("isSmallDataUrl", () => {
  it("accepts data URLs at the threshold", () => {
    expect(isSmallDataUrl("x".repeat(SMALL_IMAGE_DATA_URL_CHARS))).toBe(true);
  });

  it("rejects data URLs one char over the threshold", () => {
    expect(isSmallDataUrl("x".repeat(SMALL_IMAGE_DATA_URL_CHARS + 1))).toBe(
      false,
    );
  });
});

describe("downscaleDataUrl (node env — no DOM)", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns small image data URLs unchanged (fast path, no decode)", async () => {
    const small = "data:image/png;base64," + "A".repeat(1024);
    await expect(downscaleDataUrl(small)).resolves.toBe(small);
  });

  it("returns non-image data URLs unchanged", async () => {
    const text =
      "data:text/plain;base64," + "A".repeat(SMALL_IMAGE_DATA_URL_CHARS + 1);
    await expect(downscaleDataUrl(text)).resolves.toBe(text);
  });

  it("degrades gracefully when decoding fails — never loses the image", async () => {
    // Stub Image to throw deterministically (in the node env it is undefined
    // anyway). The contract: ANY failure in the decode/canvas path resolves
    // to the ORIGINAL data URL — an attachment is never lost or corrupted,
    // worst case it stays full-size.
    vi.stubGlobal(
      "Image",
      class {
        constructor() {
          throw new Error("no DOM in tests");
        }
      },
    );
    const huge =
      "data:image/png;base64," + "A".repeat(SMALL_IMAGE_DATA_URL_CHARS + 1);
    await expect(downscaleDataUrl(huge)).resolves.toBe(huge);
  });
});

describe("attachment downscale wiring (source contracts)", () => {
  it("re-encodes via canvas JPEG with the long-edge cap and white fill", () => {
    expect(source).toContain('toDataURL("image/jpeg"');
    expect(source).toContain("MAX_IMAGE_LONG_EDGE / longEdge");
    // JPEG has no alpha — the canvas must be filled white first so
    // transparent regions do not render black.
    expect(source).toContain('ctx.fillStyle = "#ffffff"');
    expect(source).toContain("ctx.fillRect(0, 0, width, height)");
  });

  it("keeps the smaller of original vs re-encoded output", () => {
    expect(source).toContain(
      "reencoded.length < dataUrl.length ? reencoded : dataUrl",
    );
  });

  it("InputBar pastes through fileToAttachedDataUrl (no local raw reader)", () => {
    expect(inputBarSource).toContain(
      'import { fileToAttachedDataUrl } from "../../lib/attachImages"',
    );
    expect(inputBarSource).toContain(
      "imageFiles.map(fileToAttachedDataUrl)",
    );
    expect(inputBarSource).not.toContain("function fileToDataUrl");
  });

  it("BacklogView pastes through fileToAttachedDataUrl (input + editor)", () => {
    expect(backlogSource).toContain(
      'import { fileToAttachedDataUrl } from "../../lib/attachImages"',
    );
    // Both paste/drop paths: the new-item input and the per-card editor.
    expect(
      backlogSource.match(/imageFiles\.map\(fileToAttachedDataUrl\)/g),
    ).toHaveLength(2);
    expect(backlogSource).not.toContain("function fileToDataUrl");
  });

  it("Message renders a placeholder chip for budget-evicted images", () => {
    expect(messageSource).toContain(
      "entry.imagesEvicted != null && entry.imagesEvicted > 0",
    );
    expect(messageSource).toContain("unloaded to save memory");
  });
});

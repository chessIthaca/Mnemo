// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import { toolImagePaths } from "./ToolImage";

function ok(output: string, data?: unknown) {
  return { success: true, output, ...(data === undefined ? {} : { data }) };
}

const failed = { success: false, output: "boom" };

describe("toolImagePaths", () => {
  it("image_* tools resolve the path from the image arg", () => {
    const result = ok("analyzed");
    expect(
      toolImagePaths("image_analysis", JSON.stringify({ image: "shot.png", question: "what?" }), result),
    ).toEqual(["shot.png"]);
    expect(
      toolImagePaths("image_ui_to_artifact", JSON.stringify({ image: "a/b.png", target: "code" }), result),
    ).toEqual(["a/b.png"]);
  });

  it("image_ui_diff resolves BOTH images from its real image_a/image_b args", () => {
    const result = ok("diff summary");
    expect(
      toolImagePaths("image_ui_diff", JSON.stringify({ image_a: "before.png", image_b: "after.png" }), result),
    ).toEqual(["before.png", "after.png"]);
    // A missing side renders the other one only.
    expect(
      toolImagePaths("image_ui_diff", JSON.stringify({ image_a: "before.png" }), result),
    ).toEqual(["before.png"]);
  });

  it("image_* tools with unparseable args or missing image resolve empty", () => {
    const result = ok("analyzed");
    expect(toolImagePaths("image_analysis", "{not json", result)).toEqual([]);
    expect(toolImagePaths("image_analysis", JSON.stringify({ question: "no image" }), result)).toEqual([]);
  });

  it("browser_screenshot/offscreen_browser_screenshot prefer structured data.path", () => {
    // data.path and the output text deliberately DIFFER so the assertion
    // proves the structured path wins (a plain output scan would return
    // active-123.png).
    const result = ok("screenshot saved: .coding/browser/screenshots/active-123.png", {
      path: ".coding/browser/screenshots/structured-123.png",
    });
    expect(toolImagePaths("browser_screenshot", "{}", result)).toEqual([
      ".coding/browser/screenshots/structured-123.png",
    ]);
    expect(toolImagePaths("offscreen_browser_screenshot", "{}", result)).toEqual([
      ".coding/browser/screenshots/structured-123.png",
    ]);
  });

  it("offscreen_browser_screenshot falls back to scanning the output text (regression: no preview in chat)", () => {
    // User report: the headless tool's card showed no inline PNG for
    // .coding/browser/screenshots/active-1788366238690.png — the name gate
    // used to match only browser_screenshot/game_screenshot.
    const result = ok("screenshot saved: .coding/browser/screenshots/active-1788366238690.png");
    expect(toolImagePaths("offscreen_browser_screenshot", "{}", result)).toEqual([
      ".coding/browser/screenshots/active-1788366238690.png",
    ]);
    expect(toolImagePaths("browser_screenshot", "{}", result)).toEqual([
      ".coding/browser/screenshots/active-1788366238690.png",
    ]);
  });

  it("screenshot tools with neither data.path nor a matching output line resolve empty", () => {
    expect(toolImagePaths("browser_screenshot", "{}", ok("no screenshot"))).toEqual([]);
    expect(toolImagePaths("offscreen_browser_screenshot", "{}", ok("no screenshot"))).toEqual([]);
  });

  it("failed calls and non-image tools resolve empty", () => {
    expect(toolImagePaths("image_analysis", '{"image":"x.png"}', failed)).toEqual([]);
    expect(toolImagePaths("browser_screenshot", "{}", failed)).toEqual([]);
    expect(toolImagePaths("file_edit", '{"path":"src/x.rs"}', ok("done"))).toEqual([]);
    expect(toolImagePaths("image_analysis", '{"image":"x.png"}', null)).toEqual([]);
  });
});

/**
 * Clickable-image lightbox contract (user request 2026-08-22): tool images in
 * the agent window must open a full-resolution viewer with Copy + Close. The
 * repo's node-env vitest has no DOM, so these are static source-contract
 * tests in the style of BacklogView.test.ts.
 */
import source from "./ToolImage.tsx?raw";

describe("ToolImage lightbox", () => {
  it("makes the thumbnail a clickable button that opens the viewer", () => {
    expect(source).toContain("cursor-zoom-in");
    expect(source).toContain('onClick={() => setOpen(true)}');
    expect(source).toContain('aria-label="Open image viewer"');
  });

  it("renders the viewer as a Radix Dialog with the browser overlay", () => {
    expect(source).toContain("useBrowserOverlay(open)");
    expect(source).toContain("<Dialog open={open}");
  });

  it("offers Copy image via ClipboardItem with a data-URL text fallback", () => {
    expect(source).toContain("navigator.clipboard.write([");
    expect(source).toContain("new ClipboardItem({ \"image/png\": dataUrlToBlob(dataUrl) })");
    expect(source).toContain("navigator.clipboard.writeText(dataUrl)");
  });

  it("shows the natural-size image (full resolution) in a scrollable body", () => {
    expect(source).toContain('className="max-w-none"');
    expect(source).toContain("overflow-auto");
  });

  it("offers a Close button in the viewer header", () => {
    expect(source).toContain('aria-label="Close image viewer"');
    expect(source).toContain("onClick={onClose}");
  });
});

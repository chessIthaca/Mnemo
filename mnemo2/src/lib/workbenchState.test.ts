import { describe, expect, it } from "vitest";
import { workerScreenshotPaths } from "./workbenchState";

describe("worker screenshot artifacts", () => {
  it("finds real sandbox screenshots once and rejects parent traversal", () => {
    const paths = workerScreenshotPaths({
      messages: [
        { role: "tool", content: "Saved .coding/browser/screenshots/page-1.png" },
        { role: "assistant", content: "Captured .coding/browser/screenshots/page-1.png and .coding/browser/screenshots/page-2.png" },
        { role: "tool", content: ".coding/browser/screenshots/../secret.png" },
      ],
    });
    expect(paths).toEqual([
      ".coding/browser/screenshots/page-1.png",
      ".coding/browser/screenshots/page-2.png",
    ]);
  });
});

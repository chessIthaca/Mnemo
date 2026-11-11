import { beforeEach, describe, expect, it } from "vitest";
import {
  clearToolImageCache,
  createToolImageTransport,
  dataUrlToBlob,
  imageName,
  loadToolImage,
  toolImagePaths,
} from "./toolImages";

describe("tool image paths", () => {
  it("reads a screenshot's structured path", () => {
    expect(
      toolImagePaths("offscreen_browser_screenshot", "{}", {
        success: true,
        output: "saved",
        data: { path: ".coding/browser/screenshots/shot.png" },
      }),
    ).toEqual([".coding/browser/screenshots/shot.png"]);
  });

  it("falls back to the path a screenshot's output text names", () => {
    expect(
      toolImagePaths("browser_screenshot", "{}", {
        success: true,
        output: "Saved the page to .coding/browser/screenshots/page-1.png",
      }),
    ).toEqual([".coding/browser/screenshots/page-1.png"]);
  });

  it("reads an image tool's image argument", () => {
    expect(
      toolImagePaths("image_describe", '{"image":"shots/a.png"}', {
        success: true,
        output: "a cat",
      }),
    ).toEqual(["shots/a.png"]);
  });

  it("reads both sides of a UI diff in order", () => {
    expect(
      toolImagePaths("image_ui_diff", '{"image_a":"before.png","image_b":"after.png"}', {
        success: true,
        output: "diffed",
      }),
    ).toEqual(["before.png", "after.png"]);
  });

  it("shows nothing for a failed call, another tool, or unreadable arguments", () => {
    expect(toolImagePaths("browser_screenshot", "{}", { success: false, output: "boom" })).toEqual(
      [],
    );
    expect(toolImagePaths("browser_screenshot", "{}", null)).toEqual([]);
    expect(toolImagePaths("shell", '{"command":"ls"}', { success: true, output: "ok" })).toEqual([]);
    expect(toolImagePaths("image_describe", "{not json", { success: true, output: "a cat" })).toEqual(
      [],
    );
  });

  it("names an image by its last path segment", () => {
    expect(imageName(".coding/browser/screenshots/shot.png")).toBe("shot.png");
    expect(imageName("C:\\shots\\shot.png")).toBe("shot.png");
  });
});

describe("the tool image transport", () => {
  it("asks the host for a worker's or the lead's image", async () => {
    const calls: Array<[string, unknown]> = [];
    const invoke = async <T,>(command: string, args?: unknown): Promise<T> => {
      calls.push([command, args]);
      return "data:image/png;base64,AAAA" as T;
    };
    const transport = createToolImageTransport({ invoke });
    await expect(transport?.read("w1", "p.png")).resolves.toBe("data:image/png;base64,AAAA");
    await transport?.read(null, "lead.png");
    expect(calls).toEqual([
      ["workbench_tool_image", { workerId: "w1", path: "p.png" }],
      ["workbench_tool_image", { workerId: null, path: "lead.png" }],
    ]);
  });
});

describe("loading a tool image", () => {
  beforeEach(() => clearToolImageCache());

  it("shares one host read per image and retries after a failure", async () => {
    let calls = 0;
    let failing = true;
    const transport = {
      read: async () => {
        calls += 1;
        if (failing) throw new Error("no image");
        return "data:image/png;base64,AAAA";
      },
    };
    const first = loadToolImage(transport, "w1", "p.png");
    const second = loadToolImage(transport, "w1", "p.png");
    expect(second).toBe(first);
    await expect(first).rejects.toThrow("no image");
    expect(calls).toBe(1);
    failing = false;
    await expect(loadToolImage(transport, "w1", "p.png")).resolves.toBe(
      "data:image/png;base64,AAAA",
    );
    expect(calls).toBe(2);
  });
});

describe("a data URL's blob", () => {
  it("decodes the image with the MIME type its header declares", () => {
    const blob = dataUrlToBlob("data:image/webp;base64,AAAA");
    expect(blob.type).toBe("image/webp");
    expect(blob.size).toBe(3);
  });

  it("falls back to PNG when the header omits a type", () => {
    expect(dataUrlToBlob("data:;base64,AAAA").type).toBe("image/png");
  });
});

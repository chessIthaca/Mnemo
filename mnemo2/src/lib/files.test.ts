import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createFilesTransport,
  formatBytes,
  highlightFile,
  languageFor,
  seedFilesPreview,
  visibleRows,
  type FileEntry,
} from "./files";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

const entry = (path: string, kind: FileEntry["kind"]): FileEntry => ({
  path,
  name: path.split("/").pop() ?? path,
  kind,
});

describe("visibleRows", () => {
  const listings: Record<string, FileEntry[]> = {
    "": [entry("src", "dir"), entry("README.md", "file")],
    src: [entry("src/main.rs", "file"), entry("src/lib", "dir")],
    "src/lib": [entry("src/lib/store.ts", "file")],
  };

  it("shows the root listing collapsed when nothing is expanded", () => {
    expect(visibleRows(listings, {})).toEqual([
      { entry: entry("src", "dir"), depth: 0, expanded: false },
      { entry: entry("README.md", "file"), depth: 0, expanded: false },
    ]);
  });

  it("inlines an expanded directory's children one level deeper", () => {
    const rows = visibleRows(listings, { src: true, "src/lib": true });
    expect(rows.map((row) => [row.entry.path, row.depth, row.expanded])).toEqual([
      ["src", 0, true],
      ["src/main.rs", 1, false],
      ["src/lib", 1, true],
      ["src/lib/store.ts", 2, false],
      ["README.md", 0, false],
    ]);
  });

  it("shows nothing for a directory whose listing has not arrived", () => {
    const rows = visibleRows({ "": [entry("src", "dir")] }, { src: true });
    expect(rows).toEqual([{ entry: entry("src", "dir"), depth: 0, expanded: true }]);
  });
});

describe("languageFor", () => {
  it("maps the repository's file types to highlight.js grammars", () => {
    expect(languageFor("src-tauri/src/mnemo2/files_view.rs")).toBe("rust");
    expect(languageFor("mnemo2/src/components/FilesTab.svelte")).toBe("xml");
    expect(languageFor("mnemo2/src/lib/files.ts")).toBe("typescript");
    expect(languageFor("agent.md")).toBe("markdown");
    expect(languageFor("Cargo.toml")).toBe("ini");
    expect(languageFor("Dockerfile")).toBe("dockerfile");
  });

  it("falls back to plain text for unknown types", () => {
    expect(languageFor("assets/logo.unknownext")).toBe("text");
    expect(languageFor("LICENSE")).toBe("text");
  });
});

describe("highlightFile", () => {
  it("highlights known source and keeps the text intact", () => {
    const html = highlightFile("fn main() {}\n", "src/main.rs");
    expect(html).toContain("hljs-keyword");
    expect(html).toContain("main");
  });

  it("escapes markup for unknown types instead of rendering it", () => {
    const html = highlightFile("<script>alert(1)</script>", "notes.unknownext");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });

  it("keeps very large files unhighlighted rather than stalling", () => {
    const html = highlightFile("let x = 1;\n".repeat(20_000), "big.rs");
    expect(html.length).toBeGreaterThan(100_000);
    expect(html).not.toContain("hljs-keyword");
  });
});

describe("formatBytes", () => {
  it("renders bytes, kilobytes and megabytes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(3 * 1024 * 1024)).toBe("3.0 MB");
  });
});

describe("createFilesTransport", () => {
  afterEach(() => {
    seedFilesPreview(null);
  });

  it("is null outside the desktop app, so the tab can say so", () => {
    expect(createFilesTransport()).toBeNull();
  });

  it("uses the preview seed when one is installed", async () => {
    seedFilesPreview({
      list: async (path) => ({ path, entries: [], truncated: false }),
      read: async (path) => ({ path, text: "x", bytes: 1, truncated: false, binary: false }),
      find: async () => [],
    });
    const transport = createFilesTransport();
    expect(transport).not.toBeNull();
    expect(await transport?.list("src")).toEqual({ path: "src", entries: [], truncated: false });
    expect((await transport?.read("src/main.rs"))?.text).toBe("x");
  });
});

import { beforeEach, describe, expect, it, vi } from "vitest";
import { isTauriRuntime } from "./runtime";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

import {
  createMemoryTransport,
  displayTitle,
  embedderLabel,
  formatRecordDate,
  formatScore,
  KIND_CHIPS,
  kindLabel,
  mergePage,
  seedMemoryPreview,
  type MemoryDetail,
  type MemoryHit,
  type MemoryListPage,
  type MemoryOverview,
  type MemoryRow,
  type MemoryTransport,
} from "./memoryView";

/** Point `isTauriRuntime` for the current test. */
function setTauri(value: boolean) {
  vi.mocked(isTauriRuntime).mockReturnValue(value);
}

function row(overrides: Partial<MemoryRow> = {}): MemoryRow {
  return {
    id: "row-1",
    title: "SPEC: typed records as files",
    kind: "spec",
    tier: "semantic",
    preview: "typed records",
    path: ".coding/knowledge/spec/2027-01-01-typed-records.md",
    created_at: 1_800_000_000,
    last_accessed_at: 1_800_000_100,
    access_count: 3,
    strength: 0.9,
    superseded: false,
    quarantined: null,
    ...overrides,
  };
}

function overview(): MemoryOverview {
  return {
    available: true,
    reason: "",
    model_id: "bge-small-en-v1.5",
    dim: 384,
    configured_model: "bge-small-en-v1.5",
    fingerprints: [{ model: "bge-small-en-v1.5", dim: 384 }],
    fingerprint_matches: true,
    counts: { working: 1, episodic: 2, semantic: 3, procedural: 4, total: 10 },
    index: { authored_count: 9, derived_count: 1, last_built_at: 1_800_000_000 },
    embedder: "ready",
  };
}

describe("memory transport", () => {
  beforeEach(() => {
    setTauri(false);
  });

  it("returns null in a plain browser with nothing planted", () => {
    expect(createMemoryTransport()).toBeNull();
  });

  it("serves the planted preview seed outside Tauri, never inside it", async () => {
    const planted: MemoryTransport = {
      list: async () => ({ rows: [row()], offset: 0, has_more: false }),
      search: async () => [],
      read: async (id) => ({ detail: id }) as unknown as MemoryDetail,
      backlinks: async () => [],
      overview: async () => overview(),
      recallTest: async () => [],
    };
    seedMemoryPreview(planted);
    expect(createMemoryTransport()).toBe(planted);

    setTauri(true);
    expect(createMemoryTransport()).not.toBe(planted);
  });

  it("invokes the memory commands with the wire argument names", async () => {
    setTauri(true);
    const answers: Record<string, unknown> = {
      memory_list: { rows: [], offset: 0, has_more: false } satisfies MemoryListPage,
      memory_search: [] satisfies MemoryHit[],
      memory_recall_test: [] satisfies MemoryHit[],
      memory_backlinks: [] satisfies MemoryRow[],
      memory_overview: overview() satisfies MemoryOverview,
      memory_read: {} satisfies Partial<MemoryDetail>,
    };
    const invoke = vi.fn(async (command: string) => answers[command]);
    const transport = createMemoryTransport({
      invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke,
    });
    expect(transport).not.toBeNull();

    await transport!.list({ kind: "spec", offset: 40, limit: 20 });
    expect(invoke).toHaveBeenLastCalledWith("memory_list", {
      kind: "spec",
      tier: null,
      offset: 40,
      limit: 20,
    });

    await transport!.list();
    expect(invoke).toHaveBeenLastCalledWith("memory_list", {
      kind: null,
      tier: null,
      offset: 0,
      limit: null,
    });

    await transport!.search("typed records", 10);
    expect(invoke).toHaveBeenLastCalledWith("memory_search", { query: "typed records", limit: 10 });

    await transport!.read("row-1");
    expect(invoke).toHaveBeenLastCalledWith("memory_read", { id: "row-1" });

    await transport!.backlinks("row-1");
    expect(invoke).toHaveBeenLastCalledWith("memory_backlinks", { id: "row-1" });

    await transport!.overview();
    expect(invoke).toHaveBeenLastCalledWith("memory_overview");

    await transport!.recallTest("why the store", 5);
    expect(invoke).toHaveBeenLastCalledWith("memory_recall_test", {
      query: "why the store",
      limit: 5,
    });
  });
});

describe("memory formatting", () => {
  it("orders the filter chips all, the four knowledge types, then other", () => {
    expect(KIND_CHIPS.map((chip) => chip.value)).toEqual([
      "all",
      "spec",
      "decision",
      "bug",
      "how",
      "other",
    ]);
  });

  it("labels typed records the way the badges read", () => {
    expect(kindLabel("spec")).toBe("SPEC");
    expect(kindLabel("decision")).toBe("DECISION");
    expect(kindLabel("how")).toBe("HOW");
    expect(kindLabel("none")).toBe("NOTE");
    expect(kindLabel("unexpected")).toBe("UNEXPECTED");
  });

  it("drops a typed title prefix the badge already shows", () => {
    expect(displayTitle("SPEC: typed records as files")).toBe("typed records as files");
    expect(displayTitle("DECISION: the tab is read-only")).toBe("the tab is read-only");
    expect(displayTitle("worker row kept its timer")).toBe("worker row kept its timer");
    expect(displayTitle("SESSIONS: not a typed prefix")).toBe("SESSIONS: not a typed prefix");
  });

  it("formats a record's date as the UTC day it was written", () => {
    expect(formatRecordDate(Date.UTC(2027, 1, 14, 9) / 1000)).toBe("2027-02-14");
    expect(formatRecordDate(null)).toBe("unknown date");
    expect(formatRecordDate(Number.NaN)).toBe("unknown date");
  });

  it("formats recall scores at two decimals", () => {
    expect(formatScore(0.9123)).toBe("0.91");
    expect(formatScore(Number.NaN)).toBe("–");
  });

  it("reads the embedder status, including the downloading variant", () => {
    expect(embedderLabel("ready")).toBe("ready");
    expect(embedderLabel("fallback")).toBe("fallback (hash embedder)");
    expect(embedderLabel("failed")).toContain("Settings");
    expect(embedderLabel({ downloading: { model: "bge-small", progress: 0.42 } })).toBe(
      "downloading bge-small (42%)",
    );
    expect(embedderLabel(null)).toBe("unknown");
  });

  it("appends a page without repeating a row", () => {
    const first = [row({ id: "a" }), row({ id: "b" })];
    const page: MemoryListPage = {
      rows: [row({ id: "b" }), row({ id: "c" })],
      offset: 2,
      has_more: false,
    };
    expect(mergePage(first, page).map((entry) => entry.id)).toEqual(["a", "b", "c"]);
  });
});

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import MemoryView from "./MemoryView.svelte";
import { actions } from "../lib/store";
import type {
  MemoryDetail,
  MemoryHit,
  MemoryListPage,
  MemoryListQuery,
  MemoryOverview,
  MemoryRow,
  MemoryTransport,
} from "../lib/memoryView";

const mocks = vi.hoisted(() => ({
  transport: null as MemoryTransport | null,
}));

// MemoryView builds its own transport on mount; the tests plant one instead.
vi.mock("../lib/memoryView", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/memoryView")>();
  return { ...original, createMemoryTransport: () => mocks.transport };
});

function row(
  id: string,
  title: string,
  kind: string,
  createdAt: number,
  overrides: Partial<MemoryRow> = {},
): MemoryRow {
  return {
    id,
    title,
    kind,
    tier: kind === "how" ? "procedural" : "semantic",
    preview: `${title} — preview`,
    path: `.coding/knowledge/${kind}/2027-02-14-${id}.md`,
    created_at: createdAt,
    last_accessed_at: createdAt + 60,
    access_count: 3,
    strength: 0.9,
    superseded: false,
    quarantined: null,
    ...overrides,
  };
}

const SPEC = row("spec-store", "SPEC: the tab reads the store", "spec", 1_800_000_200);
const DECISION = row("decision-read-only", "DECISION: the tab is read-only", "decision", 1_800_000_100);
const HOW = row("how-run-tests", "HOW: run the workspace tests", "how", 1_800_000_000);

const ROWS = [SPEC, DECISION, HOW];

const DETAILS: Record<string, MemoryDetail> = {
  [SPEC.id]: {
    id: SPEC.id,
    title: SPEC.title,
    kind: "spec",
    tier: "semantic",
    markdown: "The tab renders what the agent's own tools read.\n\n## Why the store",
    path: SPEC.path,
    created: "2027-02-14",
    status: "live",
    superseded_by: null,
    quarantined: null,
    created_at: SPEC.created_at,
    last_accessed_at: SPEC.last_accessed_at,
    access_count: 9,
    strength: 0.9,
    links: ["spec/2027-02-09-knowledge-as-files"],
    backlinks: [DECISION],
  },
  [DECISION.id]: {
    id: DECISION.id,
    title: DECISION.title,
    kind: "decision",
    tier: "semantic",
    markdown: "A read surface cannot corrupt a corpus.",
    path: DECISION.path,
    created: "2027-02-13",
    status: "live",
    superseded_by: null,
    quarantined: null,
    created_at: DECISION.created_at,
    last_accessed_at: DECISION.last_accessed_at,
    access_count: 4,
    strength: 0.9,
    links: [],
    backlinks: [],
  },
  [HOW.id]: {
    id: HOW.id,
    title: HOW.title,
    kind: "how",
    tier: "procedural",
    markdown: "Run `cargo test --workspace`.",
    path: HOW.path,
    created: "2027-02-12",
    status: "live",
    superseded_by: null,
    quarantined: null,
    created_at: HOW.created_at,
    last_accessed_at: HOW.last_accessed_at,
    access_count: 2,
    strength: 0.9,
    links: [],
    backlinks: [],
  },
};

/** The Diagnostics card the tab reads. */
function overview(): MemoryOverview {
  return {
    available: true,
    reason: "",
    model_id: "bge-small-en-v1.5",
    dim: 384,
    configured_model: "bge-small-en-v1.5",
    fingerprints: [{ model: "bge-small-en-v1.5", dim: 384 }],
    fingerprint_matches: true,
    counts: { working: 128, episodic: 41, semantic: 214, procedural: 18, total: 401 },
    index: { authored_count: 386, derived_count: 15, last_built_at: 1_800_000_000 },
    embedder: "ready",
  };
}

/** A stand-in store that records the list queries the view makes. */
function plantedTransport(): MemoryTransport & { queries: MemoryListQuery[] } {
  const queries: MemoryListQuery[] = [];
  const hits = (rows: MemoryRow[], score: number): MemoryHit[] =>
    rows.map((entry, index) => ({ ...entry, score: Math.max(0.2, score - index * 0.1) }));
  return {
    queries,
    list: async (query = {}) => {
      queries.push(query);
      const kind = query.kind ?? "all";
      const matching = ROWS.filter(
        (entry) => kind === "all" || kind === "other" || entry.kind === kind,
      );
      const offset = query.offset ?? 0;
      const limit = query.limit ?? matching.length;
      const page: MemoryListPage = {
        rows: matching.slice(offset, offset + limit),
        offset,
        has_more: offset + limit < matching.length,
      };
      return page;
    },
    search: async (text) =>
      hits(ROWS.filter((entry) => entry.title.toLowerCase().includes(text.toLowerCase())), 0.93),
    read: async (id) => {
      const detail = DETAILS[id];
      if (!detail) throw new Error(`no record ${id}`);
      return detail;
    },
    backlinks: async (id) => DETAILS[id]?.backlinks ?? [],
    overview: async () => overview(),
    recallTest: async (_text, limit = 8) => hits(ROWS, 0.71).slice(0, limit),
  };
}

/** The list pane (the left column) and the reader. */
const listPane = () => screen.getByRole("complementary", { name: "Records" });
const reader = () => screen.getByRole("article", { name: "Record" });

describe("MemoryView", () => {
  beforeEach(() => {
    mocks.transport = plantedTransport();
  });

  it("lists the records newest first and reads the newest one", async () => {
    render(MemoryView);

    await waitFor(() => expect(listPane().querySelectorAll("button.record-row")).toHaveLength(3));
    const titles = [...listPane().querySelectorAll("button.record-row")].map(
      (button) => button.querySelector(".record-title")?.textContent,
    );
    expect(titles).toEqual(["the tab reads the store", "the tab is read-only", "run the workspace tests"]);

    await waitFor(() => expect(reader()).toBeTruthy());
    const article = within(reader());
    expect(article.getByRole("heading", { level: 2, name: "the tab reads the store" })).toBeTruthy();
    expect(article.getByText(SPEC.path as string)).toBeTruthy();
    expect(article.getByText("2027-02-14")).toBeTruthy();
    expect(article.getByText("9 recalls")).toBeTruthy();
    expect(article.getByText("spec/2027-02-09-knowledge-as-files")).toBeTruthy();
    // The Markdown body is rendered, not printed: its heading is a real heading.
    expect(article.getByRole("heading", { level: 2, name: "Why the store" })).toBeTruthy();
  });

  it("opens a backlinked record when it is clicked", async () => {
    render(MemoryView);
    await waitFor(() => expect(reader()).toBeTruthy());

    const backlink = within(within(reader()).getByRole("region", { name: "Backlinks" })).getByRole(
      "button",
    );
    await fireEvent.click(backlink);

    await waitFor(() =>
      expect(
        within(reader()).getByRole("heading", { level: 2, name: "the tab is read-only" }),
      ).toBeTruthy(),
    );
    expect(within(reader()).getByText("A read surface cannot corrupt a corpus.")).toBeTruthy();
  });

  it("filters the list with the type chips", async () => {
    const transport = plantedTransport();
    mocks.transport = transport;
    render(MemoryView);
    await waitFor(() => expect(within(listPane()).getByText("the tab reads the store")).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: "Decision" }));

    await waitFor(() => expect(within(listPane()).queryByText("the tab reads the store")).toBeNull());
    expect(within(listPane()).getByText("the tab is read-only")).toBeTruthy();
    expect(transport.queries.at(-1)).toMatchObject({ kind: "decision", offset: 0 });
  });

  it("searches the store from the search box and shows the hits", async () => {
    render(MemoryView);
    await waitFor(() => expect(within(listPane()).getByText("the tab reads the store")).toBeTruthy());

    await fireEvent.input(screen.getByRole("searchbox", { name: "Search records" }), {
      target: { value: "workspace" },
    });

    await waitFor(() => expect(within(listPane()).getByText("1 matching record")).toBeTruthy());
    expect(within(listPane()).getByText("run the workspace tests")).toBeTruthy();
    expect(within(listPane()).queryByText("the tab reads the store")).toBeNull();
  });

  it("keeps Diagnostics collapsed until it is opened, then shows the counts and ranks a recall", async () => {
    const { container } = render(MemoryView);
    await waitFor(() => expect(within(listPane()).getByText("the tab reads the store")).toBeTruthy());

    const details = container.querySelector("details.diagnostics") as HTMLDetailsElement;
    expect(details.open).toBe(false);

    await fireEvent.click(within(listPane()).getByText("Diagnostics"));

    await waitFor(() => expect(within(listPane()).getByText(/384 dims · ready/)).toBeTruthy());
    expect(within(listPane()).getByText(/128 working · 41 episodic/)).toBeTruthy();
    expect(within(listPane()).getByText(/386 authored · 15 derived/)).toBeTruthy();
    expect(within(listPane()).getByText("bge-small-en-v1.5 · 384")).toBeTruthy();

    const recall = screen.getByRole("searchbox", { name: "Recall test" });
    await fireEvent.input(recall, { target: { value: "why the store" } });
    await fireEvent.submit(recall.closest("form") as HTMLFormElement);

    await waitFor(() => expect(within(listPane()).getByText("0.71")).toBeTruthy());
    expect(within(listPane()).getByText(/0\.61/)).toBeTruthy();
  });

  it("explains that the tab needs the desktop app when no transport exists", async () => {
    mocks.transport = null;
    render(MemoryView);

    expect(await screen.findByText("The Memory tab is available in the desktop app.")).toBeTruthy();
    expect(screen.queryByRole("complementary", { name: "Records" })).toBeNull();
  });

  it("opens a plan record's path in the Plans tab", async () => {
    const transport = plantedTransport();
    transport.read = async (id) => ({ ...DETAILS[id], path: ".coding/plans/fc536488.md" });
    mocks.transport = transport;
    const openPlan = vi.spyOn(actions, "openPlanDocument").mockImplementation(() => {});
    try {
      render(MemoryView);
      await waitFor(() => expect(reader()).toBeTruthy());

      const path = await within(reader()).findByText(".coding/plans/fc536488.md");
      const button = path.closest("button") as HTMLButtonElement;
      expect(button.title).toBe("Open plan fc536488 in the Plans tab");
      await fireEvent.click(button);

      expect(openPlan).toHaveBeenCalledWith("fc536488");
    } finally {
      openPlan.mockRestore();
    }
  });

  it("opens a record's own path in the Files tab", async () => {
    const openFile = vi.spyOn(actions, "openFileInWorkbench").mockImplementation(() => {});
    try {
      render(MemoryView);
      await waitFor(() => expect(reader()).toBeTruthy());

      const path = await within(reader()).findByText(SPEC.path as string);
      const button = path.closest("button") as HTMLButtonElement;
      expect(button.title).toBe(`Open ${SPEC.path} in the Files tab`);
      await fireEvent.click(button);

      expect(openFile).toHaveBeenCalledWith(SPEC.path, null);
    } finally {
      openFile.mockRestore();
    }
  });

  it("opens a path named in the record's text at its line", async () => {
    const transport = plantedTransport();
    transport.read = async (id) => ({ ...DETAILS[id], markdown: "It edits `src/agent/turn.rs:4352` first." });
    mocks.transport = transport;
    const openFile = vi.spyOn(actions, "openFileInWorkbench").mockImplementation(() => {});
    try {
      render(MemoryView);
      await waitFor(() => expect(within(reader()).getByText(/It edits/)).toBeTruthy());

      const inline = await waitFor(() => {
        const button = reader().querySelector<HTMLButtonElement>("button.md-path");
        expect(button).toBeTruthy();
        return button as HTMLButtonElement;
      });
      expect(inline.title).toBe("Open src/agent/turn.rs at line 4352 in the Files tab");
      await fireEvent.click(inline);

      expect(openFile).toHaveBeenCalledWith("src/agent/turn.rs", 4352);
    } finally {
      openFile.mockRestore();
    }
  });

  it("leaves a link that is not a path as plain text", async () => {
    render(MemoryView);
    await waitFor(() => expect(reader()).toBeTruthy());

    const links = within(reader()).getByRole("region", { name: "Links" });
    const entry = within(links).getByText("spec/2027-02-09-knowledge-as-files");

    expect(entry.closest("button")).toBeNull();
  });
});

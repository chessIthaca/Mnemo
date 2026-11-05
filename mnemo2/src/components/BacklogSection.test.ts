import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { seedBacklogPreview, type BacklogItem, type BacklogTransport } from "../lib/backlog";
import BacklogSection from "./BacklogSection.svelte";

/** One backlog row with only the fields under test spelled out. */
function row(id: string, overrides: Partial<BacklogItem> = {}): BacklogItem {
  return { id, text: `Idea ${id}`, images: [], status: "pending", created_at: 1, ...overrides };
}

/** An in-memory transport that answers like the native store, with spies. */
function fakeTransport(initial: BacklogItem[]) {
  let items = [...initial];
  const add = vi.fn(async (text: string) => {
    items = [...items, row(`id-${items.length + 1}`, { text })];
    return items;
  });
  const edit = vi.fn(async (id: string, text: string) => {
    items = items.map((item) => (item.id === id ? { ...item, text } : item));
    return items;
  });
  const remove = vi.fn(async (id: string) => {
    items = items.filter((item) => item.id !== id);
    return items;
  });
  const reorder = vi.fn(async (ids: string[]) => {
    const rank = (id: string) => {
      const index = ids.indexOf(id);
      return index < 0 ? ids.length : index;
    };
    items = [...items].sort((a, b) => rank(a.id) - rank(b.id));
    return items;
  });
  const setDeferred = vi.fn(async (id: string, deferred: boolean) => {
    items = items.map((item) => (item.id === id ? { ...item, deferred } : item));
    return items;
  });
  const clearFinished = vi.fn(async () => {
    items = items.filter((item) => item.status === "pending");
    return items;
  });
  const planThis = vi.fn(async (id: string) => {
    items = items.map((item) =>
      item.id === id
        ? { ...item, status: "in_flight" as const, note: "handed to the lead" }
        : item,
    );
    return items;
  });
  const onChanged = vi.fn(async (_handler: () => void) => () => {});
  const transport: BacklogTransport = {
    items: async () => items,
    add,
    edit,
    remove,
    reorder,
    setDeferred,
    clearFinished,
    planThis,
    onChanged,
  };
  return {
    transport,
    calls: { add, edit, remove, reorder, setDeferred, clearFinished, planThis, onChanged },
    append: (item: BacklogItem) => {
      items = [...items, item];
    },
  };
}

/** The rendered rows' idea texts, top to bottom. */
function shownIdeas(): string[] {
  return screen.queryAllByRole("listitem").map((item) => item.querySelector(".text")?.textContent ?? "");
}

describe("BacklogSection", () => {
  afterEach(() => seedBacklogPreview(null));

  it("lists the pending ideas in order and hides finished ones behind a count", async () => {
    const { transport } = fakeTransport([
      row("a"),
      row("b", { deferred: true }),
      row("done", { status: "done" }),
      row("failed", { status: "failed" }),
    ]);
    render(BacklogSection, { props: { transport } });

    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a", "Idea b"]));
    expect(screen.getByText("2 finished")).toBeTruthy();
    expect(screen.getByText("Deferred")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Backlog/ })).toBeTruthy();
  });

  it("adds an idea from the box and clears the box", async () => {
    const { transport, calls } = fakeTransport([row("a")]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a"]));

    const box = screen.getByLabelText("New backlog item") as HTMLTextAreaElement;
    await fireEvent.input(box, { target: { value: "  A new idea  " } });
    await fireEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(calls.add).toHaveBeenCalledWith("A new idea");
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a", "A new idea"]));
    expect(box.value).toBe("");
  });

  it("edits an idea inline", async () => {
    const { transport, calls } = fakeTransport([row("a"), row("b")]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a", "Idea b"]));

    await fireEvent.click(screen.getByRole("button", { name: "Edit Idea a" }));
    const box = screen.getByLabelText("Edit text of Idea a") as HTMLTextAreaElement;
    await fireEvent.input(box, { target: { value: "Idea a, edited" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(calls.edit).toHaveBeenCalledWith("a", "Idea a, edited");
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a, edited", "Idea b"]));
  });

  it("removes an idea only after an inline confirm", async () => {
    const { transport, calls } = fakeTransport([row("a"), row("b")]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a", "Idea b"]));

    await fireEvent.click(screen.getByRole("button", { name: "Remove Idea a" }));
    expect(calls.remove).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole("button", { name: "Confirm remove Idea a" }));
    expect(calls.remove).toHaveBeenCalledWith("a");
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea b"]));
  });

  it("defers and undefer an idea without removing it", async () => {
    const { transport, calls } = fakeTransport([row("a")]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a"]));

    await fireEvent.click(screen.getByRole("button", { name: "Defer Idea a" }));
    expect(calls.setDeferred).toHaveBeenCalledWith("a", true);
    await waitFor(() => expect(screen.getByRole("button", { name: "Undefer Idea a" })).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: "Undefer Idea a" }));
    expect(calls.setDeferred).toHaveBeenLastCalledWith("a", false);
  });

  it("reorders the queue with the up and down controls", async () => {
    const { transport, calls } = fakeTransport([row("a"), row("b"), row("c")]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a", "Idea b", "Idea c"]));

    await fireEvent.click(screen.getByRole("button", { name: "Move Idea a down" }));
    expect(calls.reorder).toHaveBeenCalledWith(["b", "a", "c"]);
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea b", "Idea a", "Idea c"]));

    await fireEvent.click(screen.getByRole("button", { name: "Move Idea c up" }));
    expect(calls.reorder).toHaveBeenLastCalledWith(["b", "c", "a"]);
  });

  it("clears the finished ideas behind the count", async () => {
    const { transport, calls } = fakeTransport([row("a"), row("done", { status: "done" })]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(screen.getByText("1 finished")).toBeTruthy());

    await fireEvent.click(screen.getByRole("button", { name: "Clear finished" }));
    expect(calls.clearFinished).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.queryByText("1 finished")).toBeNull());
  });

  it("hands an idea to the lead and confirms it was sent", async () => {
    const { transport, calls } = fakeTransport([row("a")]);
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a"]));

    await fireEvent.click(screen.getByRole("button", { name: "Plan this Idea a" }));
    expect(calls.planThis).toHaveBeenCalledWith("a");

    await waitFor(() => expect(screen.getByText("Sent to the lead — Idea a")).toBeTruthy());
    await waitFor(() => expect(shownIdeas()).toEqual([]));
  });

  it("shows a failed write's message", async () => {
    const { transport, calls } = fakeTransport([row("a")]);
    calls.add.mockRejectedValueOnce(new Error("the disk is full"));
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a"]));

    const box = screen.getByLabelText("New backlog item") as HTMLTextAreaElement;
    await fireEvent.input(box, { target: { value: "An idea" } });
    await fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("the disk is full"));
  });

  it("re-reads the file when another view reports a change", async () => {
    const { transport, calls, append } = fakeTransport([row("a")]);
    const listeners: (() => void)[] = [];
    calls.onChanged.mockImplementation(async (handler: () => void) => {
      listeners.push(handler);
      return () => {};
    });
    render(BacklogSection, { props: { transport } });
    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a"]));

    // Another writer (a second window, the preview, a git merge) appends.
    append(row("late"));
    listeners[0]();

    await waitFor(() => expect(shownIdeas()).toEqual(["Idea a", "Idea late"]));
    expect(calls.onChanged).toHaveBeenCalledTimes(1);
  });

  it("explains that the backlog lives in the desktop app outside it", async () => {
    seedBacklogPreview(null);
    render(BacklogSection, { props: { transport: null } });

    await waitFor(() =>
      expect(screen.getByText(/open the Mnemo2 desktop app/)).toBeTruthy(),
    );
  });
});

import { beforeEach, describe, expect, it, vi } from "vitest";
import { isTauriRuntime } from "./runtime";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

import {
  BACKLOG_EVENT_CHANNEL,
  createBacklogTransport,
  createTauriBacklogTransport,
  finishedCount,
  ideaTitle,
  isDeferred,
  movePending,
  pendingItems,
  seedBacklogPreview,
  type BacklogItem,
  type BacklogTransport,
} from "./backlog";

/** Point `isTauriRuntime` for the current test. */
function setTauri(value: boolean) {
  vi.mocked(isTauriRuntime).mockReturnValue(value);
}

/** One backlog row with only the fields under test spelled out. */
function item(id: string, overrides: Partial<BacklogItem> = {}): BacklogItem {
  return { id, text: `Idea ${id}`, images: [], status: "pending", created_at: 1, ...overrides };
}

describe("backlog transport", () => {
  beforeEach(() => {
    setTauri(false);
    seedBacklogPreview(null);
  });

  it("has no transport outside the native shell", () => {
    expect(createBacklogTransport()).toBeNull();
  });

  it("serves the planted preview transport outside the native shell, never inside it", async () => {
    const planted: BacklogTransport = {
      items: vi.fn(async () => [item("a")]),
      add: vi.fn(async () => [item("a"), item("b")]),
      edit: vi.fn(async () => [item("a")]),
      remove: vi.fn(async () => []),
      reorder: vi.fn(async () => [item("a")]),
      setDeferred: vi.fn(async () => [item("a")]),
      clearFinished: vi.fn(async () => [item("a")]),
      planThis: vi.fn(async () => [item("a")]),
      onChanged: vi.fn(async () => () => {}),
    };
    seedBacklogPreview(planted);

    expect(createBacklogTransport()).toBe(planted);
    await expect(createBacklogTransport()?.items()).resolves.toEqual([item("a")]);

    setTauri(true);
    const native = createBacklogTransport();
    expect(native).not.toBe(planted);
    expect(native).not.toBeNull();
  });

  it("maps every method to its command with its payload", async () => {
    const invoked: { command: string; args?: Record<string, unknown> }[] = [];
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      invoked.push({ command, args });
      return [item("a")];
    });
    const listen = vi.fn(async (_channel: string, handler: () => void) => {
      handler();
      return () => {};
    });
    const transport = createTauriBacklogTransport({
      invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke,
      listen: listen as unknown as typeof import("@tauri-apps/api/event").listen,
    });

    await transport.items();
    await transport.add("An idea");
    await transport.edit("a", "An edited idea");
    await transport.remove("a");
    await transport.reorder(["b", "a"]);
    await transport.setDeferred("a", true);
    await transport.clearFinished();
    await transport.planThis("a");
    const changed = vi.fn();
    await transport.onChanged(changed);

    expect(invoked).toEqual([
      { command: "backlog_items", args: undefined },
      { command: "backlog_add", args: { text: "An idea" } },
      { command: "backlog_edit", args: { id: "a", text: "An edited idea" } },
      { command: "backlog_remove", args: { id: "a" } },
      { command: "backlog_reorder", args: { ids: ["b", "a"] } },
      { command: "backlog_set_deferred", args: { id: "a", deferred: true } },
      { command: "backlog_clear_finished", args: undefined },
      { command: "plan_this", args: { id: "a" } },
    ]);
    expect(listen.mock.calls[0][0]).toBe(BACKLOG_EVENT_CHANNEL);
    expect(changed).toHaveBeenCalledTimes(1);
  });
});

describe("backlog helpers", () => {
  it("keeps the pending queue in the store's order", () => {
    const items = [
      item("a"),
      item("done", { status: "done" }),
      item("b", { deferred: true }),
      item("failed", { status: "failed" }),
    ];
    expect(pendingItems(items).map((row) => row.id)).toEqual(["a", "b"]);
    expect(pendingItems(items)[1].deferred).toBe(true);
  });

  it("counts only finished statuses behind Clear finished", () => {
    const items = [
      item("a"),
      item("running", { status: "in_flight" }),
      item("done", { status: "done" }),
      item("failed", { status: "failed" }),
      item("cant", { status: "cant_resolve" }),
    ];
    expect(finishedCount(items)).toBe(3);
  });

  it("treats an absent deferred flag as not deferred", () => {
    expect(isDeferred(item("a"))).toBe(false);
    expect(isDeferred(item("a", { deferred: true }))).toBe(true);
  });

  it("moves a pending idea and returns the full persisted order", () => {
    const items = [item("a"), item("b"), item("c"), item("done", { status: "done" })];
    expect(movePending(items, "a", 1)).toEqual(["b", "a", "c", "done"]);
    expect(movePending(items, "c", -1)).toEqual(["a", "c", "b", "done"]);
    expect(movePending(items, "b", 1)).toEqual(["a", "c", "b", "done"]);
  });

  it("refuses a move past the ends of the queue", () => {
    const items = [item("a"), item("b")];
    expect(movePending(items, "a", -1)).toBeNull();
    expect(movePending(items, "b", 1)).toBeNull();
    expect(movePending(items, "missing", 1)).toBeNull();
  });

  it("labels a row by its first line, shortened", () => {
    expect(ideaTitle(item("a", { text: "First line\nsecond line" }))).toBe("First line");
    expect(ideaTitle(item("a", { text: "x".repeat(100) }))).toHaveLength(60);
    expect(ideaTitle(item("a", { text: "x".repeat(100) })).endsWith("…")).toBe(true);
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BOARD_EVENT_CHANNEL,
  createBoardTransport,
  createTauriBoardTransport,
  seedBoardPreview,
  type BoardTransport,
} from "./boardEngine";

describe("boardEngine", () => {
  afterEach(() => seedBoardPreview(null));

  it("reads the board and listens for changes", async () => {
    const invoke = vi.fn(async () => ({ plans: [] }));
    const listen = vi.fn(async (_channel: string, handler: () => void) => {
      handler();
      return () => {};
    });
    const transport = createTauriBoardTransport({ invoke: invoke as never, listen: listen as never });
    await transport.snapshot();
    expect(invoke).toHaveBeenCalledWith("board_snapshot");
    const changed = vi.fn();
    await transport.onChanged(changed);
    expect(listen.mock.calls[0][0]).toBe(BOARD_EVENT_CHANNEL);
    expect(changed).toHaveBeenCalledTimes(1);
  });

  it("reads one plan's document under the plan id", async () => {
    const invoke = vi.fn(async () => "# Plan: Sample\n");
    const listen = vi.fn(async () => () => {});
    const transport = createTauriBoardTransport({ invoke: invoke as never, listen: listen as never });

    await expect(transport.document("plan-open-0001")).resolves.toContain("# Plan: Sample");
    expect(invoke).toHaveBeenCalledWith("board_plan_document", { planId: "plan-open-0001" });
  });

  it("has no transport outside the native shell", () => {
    expect(createBoardTransport()).toBeNull();
  });

  it("serves the planted preview transport outside the native shell", async () => {
    const seeded: BoardTransport = {
      snapshot: vi.fn(async () => ({ plans: [] })),
      document: vi.fn(async () => "# Plan: Preview\n"),
      onChanged: vi.fn(async () => () => {}),
    };
    seedBoardPreview(seeded);

    expect(createBoardTransport()).toBe(seeded);
    await expect(createBoardTransport()?.document("plan-preview")).resolves.toContain("# Plan: Preview");

    seedBoardPreview(null);
    expect(createBoardTransport()).toBeNull();
  });
});

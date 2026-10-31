import { describe, expect, it, vi } from "vitest";
import { BOARD_EVENT_CHANNEL, createBoardTransport, createTauriBoardTransport } from "./boardEngine";

describe("boardEngine", () => {
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

  it("has no transport outside the native shell", () => {
    expect(createBoardTransport()).toBeNull();
  });
});

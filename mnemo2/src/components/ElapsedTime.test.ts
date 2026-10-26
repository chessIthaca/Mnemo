import { render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { formatElapsed } from "../lib/elapsed";
import ElapsedTime from "./ElapsedTime.svelte";

describe("elapsed time", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("formats elapsed seconds, minutes, and hours without going negative", () => {
    expect(formatElapsed(1000, 1000)).toBe("0:00");
    expect(formatElapsed(0, 61_000)).toBe("1:01");
    expect(formatElapsed(0, 3_661_000)).toBe("1:01:01");
    expect(formatElapsed(5000, 1000)).toBe("0:00");
  });

  it("ticks while running and cleans up its interval", async () => {
    const startedAtMs = Date.now() - 2000;
    const { unmount } = render(ElapsedTime, { startedAtMs });
    expect(screen.getByLabelText("Elapsed time 0:02")).toBeTruthy();
    await vi.advanceTimersByTimeAsync(3000);
    expect(screen.getByLabelText("Elapsed time 0:05")).toBeTruthy();
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps a completed duration fixed without a live interval", () => {
    const startedAtMs = Date.now() - 4000;
    render(ElapsedTime, { startedAtMs, endedAtMs: startedAtMs + 4000 });
    expect(screen.getByLabelText("Elapsed time 0:04")).toBeTruthy();
    expect(vi.getTimerCount()).toBe(0);
  });
});

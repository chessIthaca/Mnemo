import { describe, expect, it } from "vitest";
import { createStreamCoalescer, type StreamCoalescerOptions } from "./streamCoalescer";

/** A hand-driven clock, frame loop, timer and visibility for the coalescer. */
function harness(overrides: Partial<StreamCoalescerOptions> = {}) {
  const batches: number[][] = [];
  let clock = 0;
  let hidden = false;
  let frames: (() => void)[] = [];
  let timers: { callback: () => void; at: number; id: number }[] = [];
  let nextTimer = 1;
  let visible: (() => void) | null = null;
  const coalescer = createStreamCoalescer<number>((items) => batches.push(items), {
    now: () => clock,
    requestFrame: (callback) => frames.push(callback),
    setTimer: (callback, ms) => {
      const id = nextTimer++;
      timers.push({ callback, at: clock + ms, id });
      return id;
    },
    clearTimer: (id) => {
      timers = timers.filter((timer) => timer.id !== id);
    },
    isHidden: () => hidden,
    onVisible: (callback) => {
      visible = callback;
      return () => {
        visible = null;
      };
    },
    stallMs: 100,
    fallbackMs: 250,
    ...overrides,
  });
  return {
    coalescer,
    batches,
    advance(ms: number) {
      clock += ms;
      const due = timers.filter((timer) => timer.at <= clock);
      timers = timers.filter((timer) => timer.at > clock);
      for (const timer of due) timer.callback();
    },
    paint() {
      const pending = frames;
      frames = [];
      for (const frame of pending) frame();
    },
    hide() {
      hidden = true;
    },
    show() {
      hidden = false;
      visible?.();
    },
    get visibleListener() {
      return visible;
    },
  };
}

describe("stream coalescer", () => {
  it("applies each item at once while frames flow", () => {
    const h = harness();
    h.coalescer.push(1);
    h.coalescer.push(2);
    h.paint();
    h.advance(16);
    h.coalescer.push(3);
    expect(h.batches).toEqual([[1], [2], [3]]);
    expect(h.coalescer.pending()).toBe(0);
  });

  it("queues everything while hidden and applies the backlog in one batch, in order, on visible", () => {
    const h = harness();
    h.hide();
    for (let item = 0; item < 1000; item += 1) h.coalescer.push(item);
    expect(h.batches).toEqual([]);
    expect(h.coalescer.pending()).toBe(1000);

    h.show();
    expect(h.batches).toHaveLength(1);
    expect(h.batches[0]).toEqual(Array.from({ length: 1000 }, (_, item) => item));
    expect(h.coalescer.pending()).toBe(0);
  });

  it("treats a frame that never painted as a throttled view and batches the burst that follows", () => {
    const h = harness();
    h.coalescer.push(1);
    // The window lost its frames (occluded or minimised) but still reports visible.
    h.advance(5_000);
    expect(h.batches).toEqual([[1]]);
    for (let item = 2; item <= 500; item += 1) h.coalescer.push(item);
    expect(h.batches).toHaveLength(1);

    h.paint();
    expect(h.batches).toHaveLength(2);
    expect(h.batches[1]).toEqual(Array.from({ length: 499 }, (_, index) => index + 2));
    // Frames flow again: back to applying each item at once.
    h.coalescer.push(501);
    expect(h.batches.at(-1)).toEqual([501]);
  });

  it("keeps later items behind a non-empty queue so order never changes", () => {
    const visible: { flip: boolean } = { flip: false };
    const h = harness({ isHidden: () => !visible.flip });
    h.coalescer.push(1);
    h.coalescer.push(2);
    // No longer hidden and no frame stall, but 1 and 2 still wait: 3 must queue behind them.
    visible.flip = true;
    h.coalescer.push(3);
    expect(h.batches).toEqual([]);
    h.paint();
    expect(h.batches).toEqual([[1, 2, 3]]);
  });

  it("flushes a queue on the fallback timer when no frame and no visibility change come", () => {
    const h = harness();
    h.hide();
    h.coalescer.push(1);
    h.coalescer.push(2);
    h.advance(249);
    expect(h.batches).toEqual([]);
    h.advance(1);
    expect(h.batches).toEqual([[1, 2]]);
  });

  it("flush applies the queue now; clear drops it", () => {
    const h = harness();
    h.hide();
    h.coalescer.push(1);
    h.coalescer.flush();
    expect(h.batches).toEqual([[1]]);
    h.coalescer.push(2);
    h.coalescer.clear();
    h.advance(1_000);
    h.show();
    expect(h.batches).toEqual([[1]]);
  });

  it("dispose stops listening for visibility changes", () => {
    const h = harness();
    expect(h.visibleListener).not.toBeNull();
    h.coalescer.dispose();
    expect(h.visibleListener).toBeNull();
  });

  it("reads document visibility by default", () => {
    const batches: string[][] = [];
    const coalescer = createStreamCoalescer<string>((items) => batches.push(items), {
      requestFrame: () => {},
    });
    const descriptor = Object.getOwnPropertyDescriptor(document, "visibilityState");
    let state: DocumentVisibilityState = "hidden";
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
    try {
      coalescer.push("a");
      coalescer.push("b");
      expect(batches).toEqual([]);
      state = "visible";
      document.dispatchEvent(new Event("visibilitychange"));
      expect(batches).toEqual([["a", "b"]]);
    } finally {
      coalescer.dispose();
      if (descriptor) Object.defineProperty(document, "visibilityState", descriptor);
      else delete (document as { visibilityState?: unknown }).visibilityState;
    }
  });
});

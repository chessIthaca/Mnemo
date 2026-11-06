import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createStickToBottom, STICK_TO_BOTTOM_PX } from "./stickToBottom";

/** A scroller stand-in: a plain object whose metrics the test sets by hand. */
interface FakeScroller {
  scrollHeight: number;
  clientHeight: number;
  scrollTop: number;
}

let frames: Map<number, FrameRequestCallback>;
let nextFrame: number;

/** Run every frame the code under test requested, oldest first. */
function flushFrame() {
  const callbacks = [...frames.values()];
  frames.clear();
  for (const callback of callbacks) callback(16);
}

function fakeScroller(scrollHeight: number, clientHeight: number, scrollTop: number): FakeScroller {
  return { scrollHeight, clientHeight, scrollTop };
}

/** The follow controls for one fake scroller; only the metrics are real. */
function followFor(scroller: FakeScroller) {
  return createStickToBottom(() => scroller as unknown as HTMLElement);
}

describe("createStickToBottom", () => {
  beforeEach(() => {
    frames = new Map();
    nextFrame = 1;
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const id = nextFrame++;
        frames.set(id, callback);
        return id;
      }),
    );
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn((id: number) => frames.delete(id)),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("follows while the reader is at the bottom", () => {
    const scroller = fakeScroller(1000, 600, 400);
    const follow = followFor(scroller);

    follow.onScroll();
    follow.schedule();
    scroller.scrollHeight = 1200;
    flushFrame();

    expect(scroller.scrollTop).toBe(1200);
  });

  it("counts within 48 px as the bottom", () => {
    // 48 px above the tail is still the bottom; one pixel further is history.
    const atBottom = fakeScroller(1000, 600, 1000 - 600 - STICK_TO_BOTTOM_PX);
    const followAt = followFor(atBottom);
    followAt.onScroll();
    followAt.schedule();
    flushFrame();
    expect(atBottom.scrollTop).toBe(1000);

    const above = fakeScroller(1000, 600, 1000 - 600 - STICK_TO_BOTTOM_PX - 1);
    const followAbove = followFor(above);
    followAbove.onScroll();
    followAbove.schedule();
    flushFrame();
    expect(above.scrollTop).toBe(1000 - 600 - STICK_TO_BOTTOM_PX - 1);
  });

  it("leaves a reader who scrolled up alone", () => {
    const scroller = fakeScroller(1000, 600, 400);
    const follow = followFor(scroller);

    scroller.scrollTop = 160;
    follow.onScroll();
    follow.schedule();
    scroller.scrollHeight = 1200;
    flushFrame();

    expect(scroller.scrollTop).toBe(160);
  });

  it("follows again after the reader returns to the bottom", () => {
    const scroller = fakeScroller(1000, 600, 160);
    const follow = followFor(scroller);

    follow.onScroll();
    follow.schedule();
    flushFrame();
    expect(scroller.scrollTop).toBe(160);

    scroller.scrollTop = 400;
    follow.onScroll();
    follow.schedule();
    scroller.scrollHeight = 1200;
    flushFrame();

    expect(scroller.scrollTop).toBe(1200);
  });

  it("reset follows again", () => {
    const scroller = fakeScroller(1000, 600, 160);
    const follow = followFor(scroller);

    follow.onScroll();
    follow.reset();
    follow.schedule();
    flushFrame();

    expect(scroller.scrollTop).toBe(1000);
  });

  it("coalesces schedules to one frame", () => {
    const scroller = fakeScroller(1000, 600, 400);
    const follow = followFor(scroller);

    follow.schedule();
    follow.schedule();

    expect(requestAnimationFrame).toHaveBeenCalledTimes(1);
  });

  it("dispose cancels a pending frame", () => {
    const scroller = fakeScroller(1000, 600, 400);
    const follow = followFor(scroller);

    follow.schedule();
    follow.dispose();
    flushFrame();

    expect(cancelAnimationFrame).toHaveBeenCalledTimes(1);
    expect(scroller.scrollTop).toBe(400);
  });

  it("observe is a no-op without ResizeObserver", () => {
    vi.stubGlobal("ResizeObserver", undefined);
    const scroller = fakeScroller(1000, 600, 400);
    const follow = followFor(scroller);

    const unobserve = follow.observe(document.createElement("div"));

    expect(() => unobserve()).not.toThrow();
    follow.dispose();
  });
});

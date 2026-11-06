/**
 * One conversation's scroll-follow state: keep the newest content in view while
 * the reader stays at the bottom, and leave them alone once they scroll up to
 * read history. Shared by the lead chat and every worker view so the rule is
 * written once.
 */

/** A reader within this many pixels of the end still counts as at the bottom. */
export const STICK_TO_BOTTOM_PX = 48;

/** The follow controls a conversation renders against: scroll events, scheduling, and cleanup. */
export interface StickToBottom {
  /** The scroller's onscroll: at the bottom means within STICK_TO_BOTTOM_PX. */
  onScroll(): void;
  /** On the next frame, scroll to the end if the reader is at the bottom. Coalesced to one frame. */
  schedule(): void;
  /** Count the reader as at the bottom again (a different conversation). */
  reset(): void;
  /** Follow this content's size changes; returns the unobserve. A no-op without ResizeObserver. */
  observe(content: Element): () => void;
  /** Cancel the frame and disconnect the observer. */
  dispose(): void;
}

/**
 * Build the follow state for one conversation. `getScroller` is read at every
 * use, so the helper works before the scroller is bound and after a tab or
 * worker switch replaces the element.
 */
export function createStickToBottom(
  getScroller: () => HTMLElement | null | undefined,
): StickToBottom {
  let nearBottom = true;
  let frame: number | null = null;
  let observer: ResizeObserver | null = null;

  function onScroll() {
    const scroller = getScroller();
    if (!scroller) return;
    const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    nearBottom = distance <= STICK_TO_BOTTOM_PX;
  }

  function schedule() {
    if (!getScroller() || frame !== null) return;
    frame = requestAnimationFrame(() => {
      frame = null;
      const scroller = getScroller();
      if (!scroller || !nearBottom) return;
      scroller.scrollTop = scroller.scrollHeight;
    });
  }

  function reset() {
    nearBottom = true;
  }

  function observe(content: Element): () => void {
    if (typeof ResizeObserver === "undefined") return () => {};
    if (!observer) observer = new ResizeObserver(() => schedule());
    observer.observe(content);
    schedule();
    return () => observer?.unobserve(content);
  }

  function dispose() {
    if (frame !== null) {
      cancelAnimationFrame(frame);
      frame = null;
    }
    observer?.disconnect();
    observer = null;
  }

  return { onScroll, schedule, reset, observe, dispose };
}

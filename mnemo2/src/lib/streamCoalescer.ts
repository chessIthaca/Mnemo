/**
 * Coalesce high-frequency host events (text deltas, usage, activity) so a
 * backlog lands in one store update instead of replaying event by event.
 *
 * A hidden or throttled webview (a minimised window, or one covered over
 * Remote Desktop) stops painting but keeps the host's event queue growing.
 * When the window comes back, every queued event runs as its own task; one
 * `app.update` per event lets the browser paint between them, so the thread
 * re-types itself and the token counters visibly race up. While frames flow
 * the coalescer applies each event at once, exactly as before. It holds events
 * only while the document is hidden or no frame has painted for `stallMs`,
 * then hands them over in one batch on the next frame, when the document turns
 * visible, or after `fallbackMs` (timers still run, slowly, while hidden).
 * Batches keep arrival order; `apply` decides what a batch folds to.
 */
export interface StreamCoalescer<T> {
  /** Apply `item` now while frames flow; otherwise queue it for the next flush. */
  push(item: T): void;
  /** Apply every queued item now, in arrival order. */
  flush(): void;
  /** Drop queued items without applying them. */
  clear(): void;
  /** Queued items not yet applied. */
  pending(): number;
  /** Stop listening for visibility changes and cancel pending timers. */
  dispose(): void;
}

export interface StreamCoalescerOptions {
  now?: () => number;
  requestFrame?: (callback: () => void) => void;
  setTimer?: (callback: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  isHidden?: () => boolean;
  /** Register a callback for "the document became visible"; returns an unsubscribe. */
  onVisible?: (callback: () => void) => () => void;
  /** A frame requested longer ago than this without painting means the view is throttled. */
  stallMs?: number;
  /** Upper bound on how long a queued item waits when no frame comes. */
  fallbackMs?: number;
}

const DEFAULT_STALL_MS = 100;
const DEFAULT_FALLBACK_MS = 250;

function defaultNow(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

function defaultRequestFrame(callback: () => void): void {
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => callback());
  else setTimeout(callback, 16);
}

function defaultIsHidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

function defaultOnVisible(callback: () => void): () => void {
  if (typeof document === "undefined") return () => undefined;
  const listener = () => {
    if (document.visibilityState === "visible") callback();
  };
  document.addEventListener("visibilitychange", listener);
  return () => document.removeEventListener("visibilitychange", listener);
}

export function createStreamCoalescer<T>(
  apply: (items: T[]) => void,
  options: StreamCoalescerOptions = {},
): StreamCoalescer<T> {
  const now = options.now ?? defaultNow;
  const requestFrame = options.requestFrame ?? defaultRequestFrame;
  const setTimer = options.setTimer ?? ((callback, ms) => setTimeout(callback, ms));
  const clearTimer = options.clearTimer ??
    ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));
  const isHidden = options.isHidden ?? defaultIsHidden;
  const stallMs = options.stallMs ?? DEFAULT_STALL_MS;
  const fallbackMs = options.fallbackMs ?? DEFAULT_FALLBACK_MS;

  let queue: T[] = [];
  // When the outstanding frame request was made; null once that frame painted.
  let frameRequestedAt: number | null = null;
  let timer: unknown = null;

  function armFrame(): void {
    if (frameRequestedAt !== null) return;
    frameRequestedAt = now();
    requestFrame(() => {
      frameRequestedAt = null;
      flush();
    });
  }

  function frameStalled(): boolean {
    return frameRequestedAt !== null && now() - frameRequestedAt > stallMs;
  }

  function cancelTimer(): void {
    if (timer === null) return;
    clearTimer(timer);
    timer = null;
  }

  function flush(): void {
    cancelTimer();
    if (queue.length === 0) return;
    const items = queue;
    queue = [];
    apply(items);
  }

  const stopVisible = (options.onVisible ?? defaultOnVisible)(flush);

  return {
    push(item) {
      if (queue.length > 0 || isHidden() || frameStalled()) {
        queue.push(item);
        armFrame();
        if (timer === null) {
          timer = setTimer(() => {
            timer = null;
            flush();
          }, fallbackMs);
        }
        return;
      }
      apply([item]);
      armFrame();
    },
    flush,
    clear() {
      queue = [];
      cancelTimer();
    },
    pending: () => queue.length,
    dispose() {
      queue = [];
      cancelTimer();
      stopVisible();
    },
  };
}

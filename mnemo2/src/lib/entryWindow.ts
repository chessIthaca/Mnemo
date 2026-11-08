/** How many of the newest entries a conversation renders; the button adds this many more. */
export const ENTRY_WINDOW = 200;

/** The entries a conversation renders and how many older ones it leaves out. */
export interface EntryWindow<T> {
  /** How many older entries are not in `visible`. */
  hidden: number;
  /** The newest entries, in their original order. */
  visible: T[];
}

/**
 * The newest `limit` entries and how many older ones are left out.
 *
 * The window is a contiguous tail: `visible` is the same array when everything
 * fits (so callers keep reference equality), otherwise the last `limit` items.
 * A fractional limit counts as its floor, and a negative one as zero.
 */
export function windowEntries<T>(entries: T[], limit: number): EntryWindow<T> {
  const hidden = Math.max(0, entries.length - Math.max(0, Math.floor(limit)));
  return { hidden, visible: hidden === 0 ? entries : entries.slice(hidden) };
}

/** The label of the button that shows older entries. */
export function earlierEntriesLabel(hidden: number): string {
  return hidden === 1 ? "Show 1 earlier message" : `Show earlier messages (${hidden} hidden)`;
}

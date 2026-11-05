export function formatElapsed(startedAtMs: number, endedAtMs: number): string {
  const seconds = Math.floor(Math.max(0, endedAtMs - startedAtMs) / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;
  const twoDigits = (value: number) => String(value).padStart(2, "0");
  return hours > 0
    ? `${hours}:${twoDigits(minutes)}:${twoDigits(remainder)}`
    : `${minutes}:${twoDigits(remainder)}`;
}

/**
 * A message's age as a compact relative stamp ("just now", "4m ago", "2h ago",
 * "3d ago"). `now` is the clock the age is measured against — tests pass it so
 * stamps stay deterministic; the UI lets it default to the real clock.
 */
export function formatAgo(atMs: number, now: number = Date.now()): string {
  const seconds = Math.max(0, now - atMs) / 1000;
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86_400)}d ago`;
}

/** The fields of a chat entry the turn walk reads; a LeadEntry and a WorkerThreadEntry both fit. */
export interface TurnEntry {
  id: string;
  kind?: string;
  author?: string;
  from?: string;
  startedAtMs?: number;
}

/**
 * The clock each tool card measures against, keyed by entry id.
 *
 * A turn starts at the first tool call after a boundary — a user message in
 * the lead chat, a lead message in a worker thread, or the top of the list —
 * so a run of fast calls reads as one running clock instead of a column of
 * 0:00s. The call stamps themselves are untouched: this only picks the origin
 * a card displays them from. A tool entry with no start time gets no entry
 * here, and so no time on its card.
 */
export function turnStartTimes(entries: ReadonlyArray<TurnEntry>): Record<string, number> {
  const turnStarts: Record<string, number> = {};
  let turnStart: number | undefined;
  for (const entry of entries) {
    if (
      (entry.kind === "message" && entry.author === "user") ||
      ((entry.kind === undefined || entry.kind === "message") && entry.from === "lead")
    ) {
      turnStart = undefined;
      continue;
    }
    if (entry.kind !== "tool" || typeof entry.startedAtMs !== "number") continue;
    if (turnStart === undefined) turnStart = entry.startedAtMs;
    turnStarts[entry.id] = turnStart;
  }
  return turnStarts;
}

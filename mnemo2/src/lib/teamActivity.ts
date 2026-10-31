import type { LeadEntry } from "./types";

/** A lead-chat card for team activity the lead transcript does not hold:
 *  a review, a landed merge, or a team message from the team snapshot. */
export type ActivityCard = Extract<LeadEntry, { kind: "review" | "merge" | "team-message" }>;

export function isActivityCard(entry: LeadEntry): entry is ActivityCard {
  if (entry.kind === "review" || entry.kind === "merge") return true;
  return entry.kind === "team-message" && !entry.id.startsWith("history-");
}

/** The lead tool whose call a card records. */
const anchorTool: Partial<Record<ActivityCard["kind"], string>> = {
  review: "team_review",
  merge: "team_merge",
};

function workerArgument(args: string): string | null {
  try {
    const parsed: unknown = JSON.parse(args);
    const id = parsed && typeof parsed === "object" ? (parsed as { worker_id?: unknown }).worker_id : undefined;
    return typeof id === "string" ? id : null;
  } catch {
    return null;
  }
}

/** When an entry happened, where the chat knows it. */
function entryTime(entry: LeadEntry): number | undefined {
  if (entry.kind === "tool") return entry.endedAtMs ?? entry.startedAtMs;
  return isActivityCard(entry) ? entry.atMs : undefined;
}

/** Skip past the cards that already follow `index` and happened no later. */
function afterOlderCards(entries: LeadEntry[], index: number, atMs: number | undefined): number {
  let at = index + 1;
  while (at < entries.length) {
    const entry = entries[at];
    if (!isActivityCard(entry) || (atMs !== undefined && (entry.atMs ?? 0) > atMs)) break;
    at += 1;
  }
  return at;
}

/** Where `card` belongs: right after the lead's own call that produced it;
 *  else after the last entry known to be earlier, when one is known to be
 *  later; else at the end, since it is the newest thing known. */
function cardSlot(entries: LeadEntry[], card: ActivityCard): number {
  const tool = anchorTool[card.kind];
  if (tool) {
    for (let index = entries.length - 1; index >= 0; index -= 1) {
      const entry = entries[index];
      if (
        entry.kind === "tool" && entry.toolName === tool && entry.result?.success !== false &&
        workerArgument(entry.arguments) === card.workerId
      ) {
        return afterOlderCards(entries, index, card.atMs);
      }
    }
  }
  const atMs = card.atMs;
  if (atMs === undefined) return entries.length;
  let lastEarlier = -1;
  entries.forEach((entry, index) => {
    const time = entryTime(entry);
    if (time !== undefined && time <= atMs) lastEarlier = index;
  });
  const laterKnown = entries.some((entry, index) => {
    const time = entryTime(entry);
    return index > lastEarlier && time !== undefined && time > atMs;
  });
  if (!laterKnown) return entries.length;
  return lastEarlier === -1 ? 0 : afterOlderCards(entries, lastEarlier, atMs);
}

/** Insert `card` where it happened (see `cardSlot`), in place. */
export function insertActivityCard(entries: LeadEntry[], card: ActivityCard): void {
  entries.splice(cardSlot(entries, card), 0, card);
}

/** The identity of a transcript-backed entry that survives a rebuild from
 *  the transcript (live and restored entries carry different ids). */
function entryKey(entry: LeadEntry): string | null {
  if (entry.kind === "tool") return `tool:${entry.toolCallId}`;
  if (entry.kind === "message") return `message:${entry.author}:${entry.body.trim()}`;
  if (entry.kind === "team-message") return `team:${entry.workerId}:${entry.body.trim()}`;
  return null;
}

/** Each keyed entry's key plus its occurrence, so repeated text maps 1:1. */
function occurrenceKeys(entries: LeadEntry[]): (string | null)[] {
  const seen = new Map<string, number>();
  return entries.map((entry) => {
    if (isActivityCard(entry)) return null;
    const key = entryKey(entry);
    if (key === null) return null;
    const count = seen.get(key) ?? 0;
    seen.set(key, count + 1);
    return `${key}#${count}`;
  });
}

/** Entries rebuilt from the transcript, with the activity cards of
 *  `previous` kept after the same entry they followed there; a card with no
 *  such entry is placed where it happened. */
export function keepActivityCards(rebuilt: LeadEntry[], previous: LeadEntry[]): LeadEntry[] {
  const cards = previous.filter(isActivityCard);
  if (cards.length === 0) return rebuilt;
  const rebuiltIndex = new Map<string, number>();
  occurrenceKeys(rebuilt).forEach((key, index) => {
    if (key !== null) rebuiltIndex.set(key, index);
  });
  const previousKeys = occurrenceKeys(previous);
  const following = new Map<number, ActivityCard[]>();
  const unplaced: ActivityCard[] = [];
  let predecessor: number | undefined;
  previous.forEach((entry, index) => {
    if (!isActivityCard(entry)) {
      const key = previousKeys[index];
      predecessor = key === null ? undefined : rebuiltIndex.get(key);
      return;
    }
    if (predecessor === undefined) {
      unplaced.push(entry);
      return;
    }
    following.set(predecessor, [...(following.get(predecessor) ?? []), entry]);
  });
  const next: LeadEntry[] = [];
  rebuilt.forEach((entry, index) => {
    next.push(entry, ...(following.get(index) ?? []));
  });
  unplaced
    .sort((left, right) => (left.atMs ?? 0) - (right.atMs ?? 0))
    .forEach((card) => insertActivityCard(next, card));
  return next;
}

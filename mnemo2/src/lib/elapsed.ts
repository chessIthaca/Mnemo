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

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

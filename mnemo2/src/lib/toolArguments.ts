/** A top-level argument that arrived as a JSON array or object inside a
 *  string (some providers stringify nested arguments), decoded; else as is. */
function decodeStringified(value: unknown): unknown {
  if (typeof value !== "string") return value;
  const text = value.trim();
  if (!/^\[[\s\S]*\]$|^\{[\s\S]*\}$/.test(text)) return value;
  try {
    const decoded: unknown = JSON.parse(text);
    return decoded !== null && typeof decoded === "object" ? decoded : value;
  } catch {
    return value;
  }
}

/** Tool-call arguments for display: indented JSON, with stringified nested
 *  arguments shown decoded instead of as escaped text. */
export function prettyArguments(raw: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return raw;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return JSON.stringify(parsed, null, 2);
  }
  const shown = Object.fromEntries(
    Object.entries(parsed).map(([key, value]) => [key, decodeStringified(value)]),
  );
  return JSON.stringify(shown, null, 2);
}

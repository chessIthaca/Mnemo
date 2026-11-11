/**
 * Sentinel values for the design tokens that component tests assert.
 *
 * jsdom 31 resolves `var(--token)` in getComputedStyle (jsdom 30 returned the
 * raw text), and the tests do not load app.css, so a token is undefined unless
 * a test defines it. A test that wants to prove a rule uses a token defines
 * these on :root and expects the computed style to equal the sentinel: no
 * default or inherited value can produce it.
 */
export const tokenSentinels = {
  "--font-ui": "sentinel-ui",
  "--text-meta": "17px",
  "--text-body": "14px",
  "--muted": "rgb(1, 2, 3)",
  "--danger": "rgb(4, 5, 6)",
} as const;

/** Define every sentinel on :root; the returned function removes them again. */
export function defineTokenSentinels(): () => void {
  const root = document.documentElement.style;
  for (const [name, value] of Object.entries(tokenSentinels)) root.setProperty(name, value);
  return () => {
    for (const name of Object.keys(tokenSentinels)) root.removeProperty(name);
  };
}

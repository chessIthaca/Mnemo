/**
 * The `[ui] theme` choice — `light`, `dark`, or `system` (follow the OS) — and
 * the one place that paints it.
 *
 * The stored word lives in the engine config (`[ui] theme`, see
 * `src/config/general.rs`); the Appearance page saves it through the same
 * `appearance_settings_*` commands its sound toggles use. Painting is a
 * `data-theme` attribute on `<html>`: `app.css` declares every colour token
 * twice, once under `:root` (light, the unchanged look) and once under
 * `:root[data-theme="dark"]`.
 *
 * The engine's CSP (`script-src 'self'`) forbids an inline pre-paint script, so
 * the entry point calls [`applyCachedTheme`] as its very first statement: the
 * last choice is mirrored into `localStorage`, which makes the app paint the
 * right palette before the first content shows, and [`applyStoredTheme`] then
 * reads the authoritative `[ui] theme` and corrects the cache.
 */

import { createAppearanceTransport, type AppearanceTransport } from "./sounds";

/** The three `[ui] theme` words. */
export type ThemeChoice = "light" | "dark" | "system";

/** The palette actually painted, after `system` is resolved. */
export type EffectiveTheme = "light" | "dark";

/** Every valid `[ui] theme` word, in the Appearance page's display order. */
export const THEME_CHOICES: readonly ThemeChoice[] = ["light", "dark", "system"];

/** The `[ui] theme` default, matching [`UiConfig`](src/config/general.rs). */
export const DEFAULT_THEME: ThemeChoice = "light";

/** The `localStorage` key mirroring the last choice for the pre-paint read. */
export const THEME_STORAGE_KEY = "mnemo2.theme";

/** The window chrome colour per palette (`<meta name="theme-color">`). */
export const THEME_COLORS: Record<EffectiveTheme, string> = {
  light: "#f7f7f4",
  dark: "#141510",
};

/** One row of the Appearance page's theme list. */
export interface ThemeRow {
  /** The word written to `[ui] theme`. */
  choice: ThemeChoice;
  /** The radio's label. */
  label: string;
  /** One line of plain-language explanation. */
  description: string;
}

/** The three theme rows, in page order. */
export const THEME_ROWS: readonly ThemeRow[] = [
  {
    choice: "light",
    label: "Light",
    description: "The paper palette: warm off-white pages with dark ink.",
  },
  {
    choice: "dark",
    label: "Dark",
    description: "A low-light palette: deep green-black pages with soft ink.",
  },
  {
    choice: "system",
    label: "System",
    description: "Follow the operating system and switch when it does.",
  },
];

/**
 * Read a `[ui] theme` word, falling back to [`DEFAULT_THEME`] for anything
 * unrecognised (an older config, a hand edit, a missing value).
 */
export function parseTheme(value: unknown): ThemeChoice {
  const word = typeof value === "string" ? value.trim().toLowerCase() : "";
  return (THEME_CHOICES as readonly string[]).includes(word)
    ? (word as ThemeChoice)
    : DEFAULT_THEME;
}

/** What `system` currently resolves to; no media query support means light. */
export function systemTheme(): EffectiveTheme {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return "light";
  }
  try {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  } catch {
    return "light";
  }
}

/** Resolve a choice to the palette it paints right now. */
export function resolveTheme(choice: ThemeChoice, system: EffectiveTheme = systemTheme()): EffectiveTheme {
  if (choice === "light" || choice === "dark") return choice;
  return system;
}

/** The choice last applied — what a live `prefers-color-scheme` change re-reads. */
let current: ThemeChoice = DEFAULT_THEME;
/** Whether the system change listener is installed (installed once, kept). */
let watching = false;

/** Repaint for the current choice; returns the palette that landed. */
function paint(): EffectiveTheme {
  const effective = resolveTheme(current);
  if (typeof document === "undefined") return effective;
  const root = document.documentElement;
  root.dataset.theme = effective;
  root.style.colorScheme = effective;
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute("content", THEME_COLORS[effective]);
  return effective;
}

/** Re-paint when the OS flips, but only while the choice is `system`. */
function watchSystem(): void {
  if (watching) return;
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
  try {
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    if (typeof query.addEventListener !== "function") return;
    query.addEventListener("change", () => {
      if (current === "system") paint();
    });
    watching = true;
  } catch {
    // No matchMedia (jsdom, an exotic webview): `system` stays on the last paint.
  }
}

/** Mirror the choice for the next pre-paint read; a blocked store is fine. */
function cache(choice: ThemeChoice): void {
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, choice);
  } catch {
    // Private mode / disabled storage: the config read still fixes the palette.
  }
}

/**
 * Apply a theme choice: paint it, mirror it for the next startup, and (for
 * `system`) start following `prefers-color-scheme`. Returns the palette painted.
 *
 * `remember` is false for previews and other read-only repaints, which must not
 * overwrite the user's saved choice in the mirror.
 */
export function applyTheme(choice: ThemeChoice, remember = true): EffectiveTheme {
  current = parseTheme(choice);
  watchSystem();
  const effective = paint();
  if (remember && typeof window !== "undefined") cache(current);
  return effective;
}

/**
 * Paint the mirrored choice synchronously — the entry point's first statement,
 * so a dark-theme user never sees a light frame. A missing or unreadable cache
 * falls back to the `[ui] theme` default.
 */
export function applyCachedTheme(): EffectiveTheme {
  let stored: string | null = null;
  try {
    stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  } catch {
    stored = null;
  }
  return applyTheme(parseTheme(stored));
}

/**
 * Read the authoritative `[ui] theme` and apply it, correcting the mirrored
 * cache. Failures leave the pre-paint palette in place; the Appearance page
 * reports a real error when the host is broken.
 */
export async function applyStoredTheme(
  transport: AppearanceTransport | null = createAppearanceTransport(),
): Promise<ThemeChoice | null> {
  if (!transport) return null;
  try {
    const settings = await transport.get();
    const choice = parseTheme(settings.theme);
    applyTheme(choice);
    return choice;
  } catch {
    return null;
  }
}

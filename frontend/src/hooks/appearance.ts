// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Pure appearance helpers + persisted-preference defaults for the frontend
// store.
//
// Extracted from `useAgentStore.ts` (Maint H3): theme/font/color handling,
// the localStorage keys + defaults, and the DOM/localStorage primitives.
// Nothing here touches the Zustand store — it is fully unit-testable and
// reused by the store facade and (for the CSS variable appliers) on app mount.

/** localStorage keys for display preferences. */
export const LS_FONT_FAMILY = "mh.fontFamily";
export const LS_FONT_SIZE = "mh.fontSize";
export const LS_THEME = "mh.theme";
export const LS_SHOW_TOKEN_USAGE = "mh.showTokenUsage";
export const LS_ACCENT_COLOR = "mh.accentColor";
export const LS_BORDER_COLOR = "mh.borderColor";
export const LS_TEXT_PRIMARY_COLOR = "mh.textPrimaryColor";
export const LS_TEXT_MUTED_COLOR = "mh.textMutedColor";
export const LS_CODE_TEXT_COLOR = "mh.codeTextColor";
export const LS_CODE_COMMENT_COLOR = "mh.codeCommentColor";
export const LS_CODE_KEYWORD_COLOR = "mh.codeKeywordColor";
export const LS_CODE_STRING_COLOR = "mh.codeStringColor";
export const LS_CODE_NUMBER_COLOR = "mh.codeNumberColor";
export const LS_CODE_TITLE_COLOR = "mh.codeTitleColor";
export const LS_CODE_VARIABLE_COLOR = "mh.codeVariableColor";
/**
 * Legacy persisted right-panel width in px (pre-fraction versions). Read
 * once as a seed by readRightPanelWidthFrac; no longer written.
 */
export const LS_RIGHT_PANEL_WIDTH = "mh.rightPanelWidth";
/**
 * Persisted right-panel width as a FRACTION of the window width
 * (null = use the default flex-grow width). A fraction tracks window
 * resizes for free and can never pin at a render cap on a small restored
 * window — the legacy px value's failure mode.
 */
export const LS_RIGHT_PANEL_WIDTH_FRAC = "mh.rightPanelWidthFrac";

/** Default font family — a safe system stack. */
export const DEFAULT_FONT_FAMILY = "system-ui";
/** Default font size in pixels. */
export const DEFAULT_FONT_SIZE = 14;
/** Default colors — match the dark theme tokens in globals.css. */
export const DEFAULT_ACCENT_COLOR = "#22d3ee";
/** Default accent for the light theme — cyan-700. The dark default cyan-400
 *  is ≈1.8:1 on white (unreadable), so an UNcustomized accent follows the
 *  theme instead of applying cyan-400 everywhere. Matches the `html.light`
 *  `--accent-color` token in globals.css. */
export const DEFAULT_ACCENT_COLOR_LIGHT = "#0e7490";
export const DEFAULT_BORDER_COLOR = "#334155";
export const DEFAULT_TEXT_PRIMARY_COLOR = "#e2e8f0";
export const DEFAULT_TEXT_MUTED_COLOR = "#94a3b8";
/** Default code block colors — match the github-dark .hljs subset in globals.css. */
export const DEFAULT_CODE_TEXT_COLOR = "#e2e8f0";
export const DEFAULT_CODE_COMMENT_COLOR = "#8b949e";
export const DEFAULT_CODE_KEYWORD_COLOR = "#ff7b72";
export const DEFAULT_CODE_STRING_COLOR = "#a5d6ff";
export const DEFAULT_CODE_NUMBER_COLOR = "#79c0ff";
export const DEFAULT_CODE_TITLE_COLOR = "#d2a8ff";
export const DEFAULT_CODE_VARIABLE_COLOR = "#ffa657";

export type Theme = "dark" | "light" | "system";

/**
 * Resolve the accent to actually write to `--accent-color`. A CUSTOMIZED
 * accent (anything but the dark default) applies as-is in both themes. The
 * dark default (#22d3ee) swaps to the light default (cyan-700) while the
 * light theme is in effect — cyan-400 on white is ≈1.8:1 (user-reported
 * unreadable). Pure + exported for tests; the DOM-coupled callers
 * (`applyTheme`, `applyColors`) pass the live theme.
 */
export function effectiveAccentFor(accent: string, light: boolean): string {
  if (accent.toLowerCase() !== DEFAULT_ACCENT_COLOR) return accent;
  return light ? DEFAULT_ACCENT_COLOR_LIGHT : DEFAULT_ACCENT_COLOR;
}

/**
 * Write the current font family + size onto the document root as CSS custom
 * properties, so any element using `var(--app-font-family)` /
 * `var(--app-font-size)` picks up the change immediately.
 *
 * Called from the store setters and on app mount (to restore persisted prefs).
 */
export function applyFontVars(family: string, size: number): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.style.setProperty("--app-font-family", family);
  root.style.setProperty("--app-font-size", `${size}px`);
}

/** Resolve a Theme preference to the effective dark/light mode. */
export function resolveTheme(theme: Theme): "dark" | "light" {
  if (theme === "light") return "light";
  if (theme === "dark") return "dark";
  if (typeof window !== "undefined" && window.matchMedia) {
    return window.matchMedia("(prefers-color-scheme: light)").matches
      ? "light"
      : "dark";
  }
  return "dark";
}

/**
 * Apply the theme by toggling the `.light` class on `<html>`. The CSS tokens
 * in globals.css swap when this class is present. `"system"` follows
 * `prefers-color-scheme`.
 *
 * Also re-applies the accent: an uncustomized accent follows the theme
 * (cyan-400 dark / cyan-700 light — see `effectiveAccentFor`), so a theme
 * switch updates it immediately without waiting for the next `applyColors`.
 */
export function applyTheme(theme: Theme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const light = resolveTheme(theme) === "light";
  if (light) {
    root.classList.add("light");
  } else {
    root.classList.remove("light");
  }
  const accent = readLs(LS_ACCENT_COLOR, DEFAULT_ACCENT_COLOR);
  root.style.setProperty("--accent-color", effectiveAccentFor(accent, light));
}

/**
 * Write the four configurable colors onto the document root as CSS custom
 * properties, so the whole UI picks up the change immediately:
 * - `--accent-color` — drives all cyan-* Tailwind classes (via the overrides
 *   in globals.css).
 * - `--border-color` — drives `border-border` + scrollbar.
 * - `--text-primary` — drives `bg-bg-primary` text + body text.
 * - `--text-muted` — drives muted text + scrollbar hover.
 *
 * Called from the store setters and on app mount (to restore persisted prefs).
 *
 * The accent passes through `effectiveAccentFor` first: the dark default
 * swaps to the light-theme default while `.light` is on `<html>` (the live
 * DOM class, so Settings previews get the not-yet-saved theme).
 */
export function applyColors(
  accent: string,
  border: string,
  textPrimary: string,
  textMuted: string,
): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const light = root.classList.contains("light");
  root.style.setProperty("--accent-color", effectiveAccentFor(accent, light));
  root.style.setProperty("--border-color", border);
  root.style.setProperty("--text-primary", textPrimary);
  root.style.setProperty("--text-muted", textMuted);
}

/**
 * Write the seven configurable code block colors onto the document root as
 * CSS custom properties, so syntax highlighting (the `.hljs-*` rules in
 * globals.css) picks up the change immediately:
 * - `--code-text` — base code block text.
 * - `--code-comment` — comments + meta.
 * - `--code-keyword` — keywords, types, literals.
 * - `--code-string` — strings, attrs, symbols, regex.
 * - `--code-number` — numbers, bullets, attributes.
 * - `--code-title` — function/class names, sections, built-ins.
 * - `--code-variable` — variables, template variables.
 *
 * Called from the store setters and on app mount (to restore persisted prefs).
 */
export function applyCodeColors(
  codeText: string,
  codeComment: string,
  codeKeyword: string,
  codeString: string,
  codeNumber: string,
  codeTitle: string,
  codeVariable: string,
): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.style.setProperty("--code-text", codeText);
  root.style.setProperty("--code-comment", codeComment);
  root.style.setProperty("--code-keyword", codeKeyword);
  root.style.setProperty("--code-string", codeString);
  root.style.setProperty("--code-number", codeNumber);
  root.style.setProperty("--code-title", codeTitle);
  root.style.setProperty("--code-variable", codeVariable);
}

/** Read a persisted font preference from localStorage (with fallback). */
export function readLs(key: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  try {
    return window.localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

/** Read a persisted numeric preference from localStorage (with fallback). */
export function readLsNumber(key: string, fallback: number): number {
  const raw = readLs(key, String(fallback));
  const n = Number(raw);
  return Number.isFinite(n) ? n : fallback;
}

/**
 * Read a persisted number from localStorage, returning null when the key is
 * absent or the stored value isn't a finite number. Used for optional numeric
 * prefs (e.g. the right-panel width) where "unset" is a meaningful value
 * distinct from any number.
 */
export function readLsNumberOrNull(key: string): number | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(key);
    if (raw === null || raw === "") return null;
    const n = Number(raw);
    return Number.isFinite(n) ? n : null;
  } catch {
    return null;
  }
}

/** Persist a preference to localStorage (best-effort). */
export function writeLs(key: string, value: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // ignore quota / privacy-mode errors
  }
}

/**
 * The chat column's guaranteed minimum width in px — the right-panel band
 * ceiling reserves this much of the window. Shared by clampPanelFraction
 * and the RightPanel render (the caps ride inline in the width style so
 * the layout engine re-clamps at every viewport — review L1).
 */
export const CHAT_MIN_PX = 480;
/**
 * The right panel's maximum share of the window width (the band ceiling).
 * Shared by clampPanelFraction and the RightPanel inline style caps.
 */
export const PANEL_MAX_FRAC = 0.5;

/**
 * Clamp a right-panel width FRACTION into the sane band for a viewport:
 * at least the 300px panel floor, at most half the window, and never so
 * wide that the chat column drops below a ~480px guaranteed minimum.
 * Replaces the old [300, 0.8×innerWidth] px clamp, whose 80% ceiling let
 * a px width persisted from a larger monitor fill the whole app on a
 * small restored window (the chat column was squeezed to a sliver).
 * Pure — used by the panel render, the resize handle, and tests.
 */
export function clampPanelFraction(frac: number, innerWidth: number): number {
  const w = Math.max(1, innerWidth);
  const min = Math.max(300 / w, 0.15);
  const max = Math.max(min, Math.min(PANEL_MAX_FRAC, (w - CHAT_MIN_PX) / w));
  return Math.min(Math.max(frac, min), max);
}

/**
 * Read the persisted right-panel width fraction. When the fraction key is
 * unset, seeds from the legacy absolute-px key (mh.rightPanelWidth,
 * pre-fraction versions) by reinterpreting it against the current
 * viewport, clamped into the band — no migration write. Returns null when
 * neither key is set (the panel then uses its default flex-grow width).
 */
export function readRightPanelWidthFrac(): number | null {
  const frac = readLsNumberOrNull(LS_RIGHT_PANEL_WIDTH_FRAC);
  if (frac !== null) return frac;
  if (typeof window === "undefined") return null;
  const legacyPx = readLsNumberOrNull(LS_RIGHT_PANEL_WIDTH);
  if (legacyPx === null) return null;
  return clampPanelFraction(legacyPx / window.innerWidth, window.innerWidth);
}

// ── Color preferences table (D4 dedup) ──────────────────────────────────
// One table drives the 11 color prefs (4 UI + 7 code) so the store's 11
// hand-written setters + resetColors/resetAppearance collapse to thin
// dispatchers over this table.

/** A color preference: its store state key, localStorage key, + default. */
export interface ColorPref {
  stateKey: ColorStateKey;
  lsKey: string;
  def: string;
}

/** The 11 color state keys (4 UI + 7 code). */
export type ColorStateKey =
  | "accentColor"
  | "borderColor"
  | "textPrimaryColor"
  | "textMutedColor"
  | "codeTextColor"
  | "codeCommentColor"
  | "codeKeywordColor"
  | "codeStringColor"
  | "codeNumberColor"
  | "codeTitleColor"
  | "codeVariableColor";

/** The 11 color preferences (4 UI + 7 code), in a single table. */
export const COLOR_PREFS: ColorPref[] = [
  { stateKey: "accentColor", lsKey: LS_ACCENT_COLOR, def: DEFAULT_ACCENT_COLOR },
  { stateKey: "borderColor", lsKey: LS_BORDER_COLOR, def: DEFAULT_BORDER_COLOR },
  { stateKey: "textPrimaryColor", lsKey: LS_TEXT_PRIMARY_COLOR, def: DEFAULT_TEXT_PRIMARY_COLOR },
  { stateKey: "textMutedColor", lsKey: LS_TEXT_MUTED_COLOR, def: DEFAULT_TEXT_MUTED_COLOR },
  { stateKey: "codeTextColor", lsKey: LS_CODE_TEXT_COLOR, def: DEFAULT_CODE_TEXT_COLOR },
  { stateKey: "codeCommentColor", lsKey: LS_CODE_COMMENT_COLOR, def: DEFAULT_CODE_COMMENT_COLOR },
  { stateKey: "codeKeywordColor", lsKey: LS_CODE_KEYWORD_COLOR, def: DEFAULT_CODE_KEYWORD_COLOR },
  { stateKey: "codeStringColor", lsKey: LS_CODE_STRING_COLOR, def: DEFAULT_CODE_STRING_COLOR },
  { stateKey: "codeNumberColor", lsKey: LS_CODE_NUMBER_COLOR, def: DEFAULT_CODE_NUMBER_COLOR },
  { stateKey: "codeTitleColor", lsKey: LS_CODE_TITLE_COLOR, def: DEFAULT_CODE_TITLE_COLOR },
  { stateKey: "codeVariableColor", lsKey: LS_CODE_VARIABLE_COLOR, def: DEFAULT_CODE_VARIABLE_COLOR },
];

/**
 * Write all 11 color CSS vars from a state snapshot (UI 4 + code 7).
 * Takes a record keyed by `ColorStateKey` so the store can pass its full
 * color slice in one call.
 */
export function applyColorPrefs<S extends Record<ColorStateKey, string>>(s: S): void {
  applyColors(s.accentColor, s.borderColor, s.textPrimaryColor, s.textMutedColor);
  applyCodeColors(
    s.codeTextColor,
    s.codeCommentColor,
    s.codeKeywordColor,
    s.codeStringColor,
    s.codeNumberColor,
    s.codeTitleColor,
    s.codeVariableColor,
  );
}

/** Persist all 11 color prefs to localStorage from a state snapshot. */
export function writeColorPrefs<S extends Record<ColorStateKey, string>>(s: S): void {
  for (const p of COLOR_PREFS) writeLs(p.lsKey, s[p.stateKey]);
}

/** A record of all 11 color defaults (for reset). */
export function defaultColorPrefs(): Record<ColorStateKey, string> {
  const out = {} as Record<ColorStateKey, string>;
  for (const p of COLOR_PREFS) out[p.stateKey] = p.def;
  return out;
}

/** Read the persisted theme, defaulting to "dark". */
export function readTheme(): Theme {
  const raw = readLs(LS_THEME, "dark");
  if (raw === "light" || raw === "system" || raw === "dark") return raw;
  return "dark";
}

/** Read show-token-usage preference (default true). */
export function readShowTokenUsage(): boolean {
  const raw = readLs(LS_SHOW_TOKEN_USAGE, "true");
  return raw !== "false" && raw !== "0";
}

/** One of the seven code-color state keys (each backed by a localStorage key). */
export type CodeColorKey =
  | "codeTextColor"
  | "codeCommentColor"
  | "codeKeywordColor"
  | "codeStringColor"
  | "codeNumberColor"
  | "codeTitleColor"
  | "codeVariableColor";

/** The seven code-color state keys, in `applyCodeColors` parameter order. */
export const CODE_COLOR_ORDER: CodeColorKey[] = [
  "codeTextColor",
  "codeCommentColor",
  "codeKeywordColor",
  "codeStringColor",
  "codeNumberColor",
  "codeTitleColor",
  "codeVariableColor",
];

/**
 * Update a single color (UI or code): persist it to localStorage, rebuild the
 * full color state (substituting the new value), and re-apply all 11 CSS
 * variables. Shared by all 11 `set*Color` setters so they collapse to
 * one-line dispatchers.
 */
export function setColor<S extends Record<ColorStateKey, string>>(
  key: ColorStateKey,
  value: string,
  get: () => S,
  set: (partial: Partial<S>) => void,
): void {
  const pref = COLOR_PREFS.find((p) => p.stateKey === key);
  if (pref) writeLs(pref.lsKey, value);
  const next = { ...get(), [key]: value } as S;
  applyColorPrefs(next);
  set({ [key]: value } as Partial<S>);
}

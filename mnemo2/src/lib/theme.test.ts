import { beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_THEME,
  THEME_CHOICES,
  THEME_COLORS,
  THEME_STORAGE_KEY,
  applyCachedTheme,
  applyStoredTheme,
  applyTheme,
  parseTheme,
  resolveTheme,
} from "./theme";
import type { AppearanceSettings, AppearanceTransport } from "./sounds";

/**
 * A controllable `prefers-color-scheme` stub — jsdom ships no `matchMedia`.
 * `theme.ts` installs its change listener once, at the first `applyTheme`, so
 * the stub is created at import time and every later call hands back the same
 * query object.
 */
function stubMatchMedia(dark: boolean): { set(next: boolean): void } {
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  const query = {
    matches: dark,
    media: "(prefers-color-scheme: dark)",
    onchange: null,
    addEventListener(_: string, listener: (event: MediaQueryListEvent) => void): void {
      listeners.add(listener);
    },
    removeEventListener(_: string, listener: (event: MediaQueryListEvent) => void): void {
      listeners.delete(listener);
    },
    addListener(listener: (event: MediaQueryListEvent) => void): void {
      listeners.add(listener);
    },
    removeListener(listener: (event: MediaQueryListEvent) => void): void {
      listeners.delete(listener);
    },
    dispatchEvent(): boolean {
      return false;
    },
  };
  window.matchMedia = (() => query) as unknown as typeof window.matchMedia;
  return {
    set(next: boolean): void {
      query.matches = next;
      for (const listener of [...listeners]) listener({ matches: next } as MediaQueryListEvent);
    },
  };
}

const media = stubMatchMedia(false);

/** A minimal stored snapshot for the transport fakes. */
function snapshot(theme: string): AppearanceSettings {
  return {
    sound_complete: true,
    sound_input_needed: true,
    sound_stopped_errors: true,
    theme,
  };
}

/** A transport that answers `get` with `theme` and echoes saves back. */
function fakeTransport(theme: string): AppearanceTransport {
  return {
    get: async () => snapshot(theme),
    save: async (patch) => snapshot(patch.theme ?? theme),
  };
}

beforeEach(() => {
  media.set(false);
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.style.colorScheme = "";
  window.localStorage.clear();
});

describe("parseTheme", () => {
  it("accepts every word the page offers, ignoring case and padding", () => {
    for (const word of THEME_CHOICES) {
      expect(parseTheme(word)).toBe(word);
      expect(parseTheme(`  ${word.toUpperCase()} `)).toBe(word);
    }
  });

  it("falls back to the [ui] default for a missing or unknown word", () => {
    expect(DEFAULT_THEME).toBe("dark");
    for (const value of [null, undefined, "", "midnight", 7, {}]) {
      expect(parseTheme(value)).toBe(DEFAULT_THEME);
    }
  });
});

describe("resolveTheme", () => {
  it("paints an explicit choice and follows the OS for system", () => {
    expect(resolveTheme("light", "dark")).toBe("light");
    expect(resolveTheme("dark", "light")).toBe("dark");
    expect(resolveTheme("system", "dark")).toBe("dark");
    expect(resolveTheme("system", "light")).toBe("light");
  });
});

describe("applyTheme", () => {
  it("paints <html> and mirrors the choice for the next pre-paint read", () => {
    expect(applyTheme("dark")).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");

    expect(applyTheme("light")).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });

  it("keeps the window chrome colour in step with the palette", () => {
    const meta = document.createElement("meta");
    meta.name = "theme-color";
    meta.content = THEME_COLORS.light;
    document.head.append(meta);
    try {
      applyTheme("dark");
      expect(meta.content).toBe(THEME_COLORS.dark);
      applyTheme("light");
      expect(meta.content).toBe(THEME_COLORS.light);
    } finally {
      meta.remove();
    }
  });

  it("follows a live OS change while the choice is system, and stops after", () => {
    applyTheme("system");
    expect(document.documentElement.dataset.theme).toBe("light");

    media.set(true);
    expect(document.documentElement.dataset.theme, "the OS flip repaints at once").toBe("dark");
    media.set(false);
    expect(document.documentElement.dataset.theme).toBe("light");

    applyTheme("light");
    media.set(true);
    expect(document.documentElement.dataset.theme, "an explicit choice stops following").toBe("light");
  });

  it("paints a read-only repaint without overwriting the mirrored choice", () => {
    applyTheme("light");
    applyTheme("dark", false);
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });
});

describe("applyCachedTheme", () => {
  it("paints the mirrored choice on the first frame", () => {
    window.localStorage.setItem(THEME_STORAGE_KEY, "system");
    media.set(true);
    expect(applyCachedTheme()).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("falls back to the [ui] default when nothing is mirrored", () => {
    expect(applyCachedTheme()).toBe(DEFAULT_THEME);
    expect(document.documentElement.dataset.theme).toBe(DEFAULT_THEME);
  });
});

describe("applyStoredTheme", () => {
  it("reads [ui] theme through the transport and paints it", async () => {
    expect(await applyStoredTheme(fakeTransport("light"))).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });

  it("paints system as the OS says right now", async () => {
    media.set(true);
    expect(await applyStoredTheme(fakeTransport("system"))).toBe("system");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("keeps the painted palette when there is no host to ask", async () => {
    applyTheme("dark");
    expect(await applyStoredTheme(null)).toBeNull();
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("keeps the painted palette when the host fails", async () => {
    applyTheme("light");
    const broken: AppearanceTransport = {
      get: async () => {
        throw new Error("ipc down");
      },
      save: async () => snapshot("dark"),
    };
    expect(await applyStoredTheme(broken)).toBeNull();
    expect(document.documentElement.dataset.theme).toBe("light");
  });
});

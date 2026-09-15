// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the InflightBar reasoning panel's persisted open/closed
 * preference.
 *
 * Regression (user report 2027-01-11): the panel opened itself when a
 * reasoning block started streaming. Whether it is open is the user's choice
 * alone, so closing the app with the panel collapsed must NOT reopen it on the
 * next start — and an unset key must mean CLOSED.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  LS_INFLIGHT_PANEL_OPEN,
  parsePanelOpen,
  readInflightPanelOpen,
  writeInflightPanelOpen,
} from "./inflightPanelPref";

/** A minimal in-memory localStorage stand-in. */
function fakeStorage(): Pick<Storage, "getItem" | "setItem"> {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parsePanelOpen", () => {
  it("is closed for an absent value (the fresh-install default)", () => {
    expect(parsePanelOpen(null)).toBe(false);
  });

  it("is closed for every value except the literal \"1\"", () => {
    for (const raw of ["", "0", "true", "open", "yes", "2", "-1"]) {
      expect(parsePanelOpen(raw)).toBe(false);
    }
  });

  it("is open only for \"1\"", () => {
    expect(parsePanelOpen("1")).toBe(true);
  });
});

describe("readInflightPanelOpen / writeInflightPanelOpen", () => {
  it("round-trips the user's choice through localStorage", () => {
    vi.stubGlobal("window", { localStorage: fakeStorage() });
    writeInflightPanelOpen(true);
    expect(window.localStorage.getItem(LS_INFLIGHT_PANEL_OPEN)).toBe("1");
    expect(readInflightPanelOpen()).toBe(true);

    writeInflightPanelOpen(false);
    expect(window.localStorage.getItem(LS_INFLIGHT_PANEL_OPEN)).toBe("0");
    expect(readInflightPanelOpen()).toBe(false);
  });

  it("starts closed when the key was never written (fresh install)", () => {
    vi.stubGlobal("window", { localStorage: fakeStorage() });
    expect(readInflightPanelOpen()).toBe(false);
  });

  it("degrades to closed with no window (node environment)", () => {
    expect(typeof window).toBe("undefined");
    expect(readInflightPanelOpen()).toBe(false);
    expect(() => writeInflightPanelOpen(true)).not.toThrow();
  });

  it("degrades to closed when localStorage throws (privacy mode / quota)", () => {
    vi.stubGlobal("window", {
      localStorage: {
        getItem: () => {
          throw new Error("denied");
        },
        setItem: () => {
          throw new Error("denied");
        },
      },
    });
    expect(readInflightPanelOpen()).toBe(false);
    expect(() => writeInflightPanelOpen(true)).not.toThrow();
  });
});

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Sounds contract tests — the tone tables are pure data, so their SHAPE is
 * pinned here (ascending chime / same-pitch double-ping / descending low
 * doom, all short and quiet, kinds pairwise distinct) plus the pure
 * `soundEnabled` gate table. `playSound` itself is impure (Web Audio) and
 * only smoke-checked for not throwing in a node environment (no
 * AudioContext → silent no-op).
 */

import { describe, expect, it } from "vitest";
import { playSound, soundEnabled, TONES, type SoundKind } from "./sounds";

describe("TONES shape contracts", () => {
  it("complete is a strictly ascending chime (bright, 'it went well')", () => {
    const freqs = TONES.complete.map((n) => n.freq);
    expect(freqs.length).toBeGreaterThanOrEqual(2);
    for (let i = 1; i < freqs.length; i++) {
      expect(freqs[i]).toBeGreaterThan(freqs[i - 1]);
    }
  });

  it("input is a same-pitch double-ping (a knock, distinct from the chime)", () => {
    const notes = TONES.input;
    expect(notes.length).toBe(2);
    expect(notes[0].freq).toBe(notes[1].freq);
    // The second ping starts after the first ends-ish (a gap, not a blend).
    expect(notes[1].startMs).toBeGreaterThan(notes[0].startMs);
  });

  it("doom strictly descends and is lowpass-filtered", () => {
    const notes = TONES.doom;
    expect(notes.length).toBeGreaterThanOrEqual(3);
    for (let i = 1; i < notes.length; i++) {
      expect(notes[i].freq).toBeLessThan(notes[i - 1].freq);
    }
    for (const n of notes) {
      expect(n.filterHz).toBeDefined();
    }
  });

  it("every sound is short and quiet", () => {
    for (const kind of Object.keys(TONES) as SoundKind[]) {
      for (const n of TONES[kind]) {
        expect(n.durMs).toBeLessThanOrEqual(300);
        expect(n.gain).toBeLessThanOrEqual(0.15);
        expect(n.gain).toBeGreaterThan(0);
      }
      const span = Math.max(...TONES[kind].map((n) => n.startMs + n.durMs));
      expect(span).toBeLessThanOrEqual(700);
    }
  });

  it("the three kinds are pairwise distinct", () => {
    const sig = (k: SoundKind) => TONES[k].map((n) => `${n.type}@${n.freq}`).join(",");
    expect(sig("complete")).not.toBe(sig("input"));
    expect(sig("complete")).not.toBe(sig("doom"));
    expect(sig("input")).not.toBe(sig("doom"));
  });
});

describe("soundEnabled", () => {
  const allOn = { soundComplete: true, soundInput: true, soundDoom: true };

  it("each flag gates exactly its own sound", () => {
    expect(soundEnabled("complete", allOn)).toBe(true);
    expect(soundEnabled("input", allOn)).toBe(true);
    expect(soundEnabled("doom", allOn)).toBe(true);

    expect(
      soundEnabled("complete", { ...allOn, soundComplete: false }),
    ).toBe(false);
    expect(soundEnabled("input", { ...allOn, soundInput: false })).toBe(false);
    expect(soundEnabled("doom", { ...allOn, soundDoom: false })).toBe(false);
  });

  it("disabling one sound leaves the others on", () => {
    const flags = { soundComplete: false, soundInput: true, soundDoom: true };
    expect(soundEnabled("complete", flags)).toBe(false);
    expect(soundEnabled("input", flags)).toBe(true);
    expect(soundEnabled("doom", flags)).toBe(true);
  });

  it("all-off disables everything", () => {
    const flags = { soundComplete: false, soundInput: false, soundDoom: false };
    expect(soundEnabled("complete", flags)).toBe(false);
    expect(soundEnabled("input", flags)).toBe(false);
    expect(soundEnabled("doom", flags)).toBe(false);
  });
});

describe("playSound", () => {
  it("never throws — in node there is no AudioContext, so it no-ops silently", () => {
    expect(() => playSound("complete")).not.toThrow();
    expect(() => playSound("input")).not.toThrow();
    expect(() => playSound("doom")).not.toThrow();
  });
});

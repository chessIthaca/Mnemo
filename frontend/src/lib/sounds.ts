// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Notification sounds — synthesized with the Web Audio API (no audio assets
 * in the repo; a few oscillator notes per sound keep the binary tiny and the
 * tones theme-able in code).
 *
 * Three sounds, each independently disableable in Settings → Sounds
 * (persisted as `[ui]` flags):
 * - `complete` — a bright ascending two-note chime: an agent's plan reached
 *   the Complete state (the turn is over; nothing needs the user).
 * - `input` — a soft double-ping: the agent is blocked on the user (an
 *   approval request or a question arrived).
 * - `doom` — a short descending low tone: three consecutive tool errors
 *   stopped the agent (mirrors the backend `agent::MAX_RETRIES` cap).
 *
 * The tone tables are exported pure data so the vitest suite can pin the
 * shape (ascending chime, double-ping, descending doom, short + quiet);
 * `playSound` is the only impure function (lazily creates ONE AudioContext
 * for the window and resumes it if the platform suspended it — WebView2
 * allows audio without a user gesture, but `resume()` covers the cases
 * where it doesn't).
 */

/** Which notification sound to play. */
export type SoundKind = "complete" | "input" | "doom";

/** One oscillator note in a sound's tone table. */
export interface ToneNote {
  /** Frequency in Hz. */
  freq: number;
  /** Offset from the sound's start, in ms. */
  startMs: number;
  /** Note length in ms (includes the decay tail). */
  durMs: number;
  /** Oscillator waveform. */
  type: "sine" | "triangle" | "sawtooth";
  /** Peak gain (0..1) — kept modest so notifications don't blast. */
  gain: number;
  /** Optional lowpass cutoff (Hz) for this note — the doom tone's warmth. */
  filterHz?: number;
}

/**
 * The tone table per sound. Data only — pinned by `sounds.test.ts`:
 * `complete` strictly ascends, `input` is a same-pitch double-ping, `doom`
 * strictly descends and is lowpass-filtered; every sound is short and quiet.
 */
export const TONES: Record<SoundKind, ToneNote[]> = {
  // Bright major-third chime (A5 → E6) — "done, and it went well".
  complete: [
    { freq: 880.0, startMs: 0, durMs: 140, type: "sine", gain: 0.12 },
    { freq: 1318.51, startMs: 110, durMs: 190, type: "sine", gain: 0.1 },
  ],
  // Soft same-pitch double-ping (two knocks) — "your turn".
  input: [
    { freq: 660.0, startMs: 0, durMs: 90, type: "triangle", gain: 0.1 },
    { freq: 660.0, startMs: 140, durMs: 120, type: "triangle", gain: 0.1 },
  ],
  // Short descending low sawtooth trio (A3 → F3 → A2), lowpass-filtered —
  // "three strikes, the agent stopped".
  doom: [
    { freq: 220.0, startMs: 0, durMs: 160, type: "sawtooth", gain: 0.11, filterHz: 600 },
    { freq: 174.61, startMs: 140, durMs: 160, type: "sawtooth", gain: 0.11, filterHz: 500 },
    { freq: 110.0, startMs: 280, durMs: 240, type: "sawtooth", gain: 0.1, filterHz: 400 },
  ],
};

/** The per-kind enable flags (mirrored from the store / `[ui]` config). */
export interface SoundFlags {
  /** The Complete chime is enabled. */
  soundComplete: boolean;
  /** The needs-input ping is enabled. */
  soundInput: boolean;
  /** The triple-error doom tone is enabled. */
  soundDoom: boolean;
}

/**
 * Whether `kind` may play under `flags` — each sound is gated by exactly its
 * own flag. Pure; pinned by test.
 */
export function soundEnabled(kind: SoundKind, flags: SoundFlags): boolean {
  switch (kind) {
    case "complete":
      return flags.soundComplete;
    case "input":
      return flags.soundInput;
    case "doom":
      return flags.soundDoom;
  }
}

/**
 * The window's single AudioContext, created lazily on the first play (and
 * never in tests/node — `getCtx` returns null there so `playSound` no-ops).
 * Module-level: one context shared by every play + preview button.
 */
let ctx: AudioContext | null = null;

function getCtx(): AudioContext | null {
  if (ctx) return ctx;
  try {
    const w = window as unknown as {
      AudioContext?: typeof AudioContext;
      webkitAudioContext?: typeof AudioContext;
    };
    const Ctor = w.AudioContext ?? w.webkitAudioContext;
    if (!Ctor) return null;
    ctx = new Ctor();
  } catch {
    // No audio available (headless test run, driver failure) — stay silent.
    return null;
  }
  return ctx;
}

/**
 * Play a notification sound. Impure (Web Audio); safe to call anywhere —
 * no-ops when the platform has no AudioContext and never throws. The
 * Settings → Sounds preview buttons call this directly regardless of the
 * enable flags (the click is the user's gesture, so the context unlocks).
 */
export function playSound(kind: SoundKind): void {
  const c = getCtx();
  if (!c) return;
  // Autoplay policy: a context created before a gesture starts suspended.
  if (c.state === "suspended") {
    void c.resume().catch(() => {
      /* stay silent if the platform refuses */
    });
  }
  const base = c.currentTime;
  for (const n of TONES[kind]) {
    const osc = c.createOscillator();
    osc.type = n.type;
    osc.frequency.value = n.freq;
    const gain = c.createGain();
    const t0 = base + n.startMs / 1000;
    const t1 = t0 + n.durMs / 1000;
    // Short attack, exponential decay — a "ding", not a blip or a drone.
    gain.gain.setValueAtTime(0.0001, t0);
    gain.gain.exponentialRampToValueAtTime(n.gain, t0 + 0.01);
    gain.gain.exponentialRampToValueAtTime(0.0001, t1);
    if (n.filterHz !== undefined) {
      const filter = c.createBiquadFilter();
      filter.type = "lowpass";
      filter.frequency.value = n.filterHz;
      osc.connect(filter).connect(gain).connect(c.destination);
    } else {
      osc.connect(gain).connect(c.destination);
    }
    osc.start(t0);
    osc.stop(t1 + 0.02);
  }
}

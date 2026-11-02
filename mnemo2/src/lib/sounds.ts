/**
 * Notification sounds and the Appearance page's sound toggles.
 *
 * Three short tones are synthesized with the Web Audio API — no audio assets
 * in the repo — and each maps to one `[ui]` flag the engine already persists
 * (`src-tauri/src/mnemo2/appearance_settings.rs`, the app-side `[ui]` writer):
 *
 * - `complete` (`[ui] sound_complete`) — a lead or worker turn finished:
 *   a bright ascending major-third chime;
 * - `input_needed` (`[ui] sound_input_needed`) — the lead or a worker needs
 *   the human (an approval or a question): a soft same-pitch double-ping;
 * - `stopped_errors` (`[ui] sound_stopped_errors`) — a turn stopped with
 *   errors: a short descending lowpass-filtered trio.
 *
 * `playSound` is the only impure function (it lazily creates ONE
 * `AudioContext` for the window and no-ops where the platform has none, so
 * tests and a plain browser stay silent). Everything else is pure and pinned
 * by `sounds.test.ts`.
 *
 * The store calls `notifySound(kind)`, which gates on the cached toggles and
 * debounces PER SOUND, so a burst of the same event — a team of workers
 * finishing together — plays one chime. A more urgent sound is never buried
 * by one that just played: `input_needed` gets through a `stopped_errors` or
 * `complete` window, and `stopped_errors` through a `complete` one, so the
 * sound that most needs the human is always heard. The Appearance page pushes
 * every save through `applySoundToggles`, so a flipped toggle takes effect on
 * the very next event without a restart.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/** Which notification sound to play (one per `[ui]` toggle). */
export type SoundKind = "complete" | "input_needed" | "stopped_errors";

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
  /** Optional lowpass cutoff (Hz) for this note — the error tone's warmth. */
  filterHz?: number;
}

/**
 * The tone table per sound. Data only — pinned by `sounds.test.ts`:
 * `complete` strictly ascends, `input_needed` is a same-pitch double-ping,
 * `stopped_errors` strictly descends and is lowpass-filtered; every sound is
 * short and quiet.
 */
export const TONES: Record<SoundKind, ToneNote[]> = {
  // Bright major-third chime (A5 → E6) — "done, and it went well".
  complete: [
    { freq: 880.0, startMs: 0, durMs: 140, type: "sine", gain: 0.12 },
    { freq: 1318.51, startMs: 110, durMs: 190, type: "sine", gain: 0.1 },
  ],
  // Soft same-pitch double-ping (two knocks) — "your turn".
  input_needed: [
    { freq: 660.0, startMs: 0, durMs: 90, type: "triangle", gain: 0.1 },
    { freq: 660.0, startMs: 140, durMs: 120, type: "triangle", gain: 0.1 },
  ],
  // Short descending low sawtooth trio (A3 → F3 → A2), lowpass-filtered —
  // "three strikes, the agent stopped".
  stopped_errors: [
    { freq: 220.0, startMs: 0, durMs: 160, type: "sawtooth", gain: 0.11, filterHz: 600 },
    { freq: 174.61, startMs: 140, durMs: 160, type: "sawtooth", gain: 0.11, filterHz: 500 },
    { freq: 110.0, startMs: 280, durMs: 240, type: "sawtooth", gain: 0.1, filterHz: 400 },
  ],
};

// ── Toggles ─────────────────────────────────────────────────────────────────

/** The three `[ui]` sound flags, in wire (snake_case) form. */
export interface SoundToggles {
  /** Play a sound when a lead or worker turn finishes. */
  sound_complete: boolean;
  /** Play a sound when the lead or a worker needs the human. */
  sound_input_needed: boolean;
  /** Play a sound when a turn stops with errors. */
  sound_stopped_errors: boolean;
}

/** The Appearance page's snapshot: the toggles plus the stored theme word. */
export interface AppearanceSettings extends SoundToggles {
  /** The stored `[ui] theme` (`"dark"`, `"light"` or `"system"`). */
  theme: string;
}

/** A partial toggle save — omitted flags keep their stored value. */
export type AppearancePatch = Partial<SoundToggles>;

/** Every sound is on until the user says otherwise (matches `UiConfig`). */
export const DEFAULT_SOUND_TOGGLES: SoundToggles = {
  sound_complete: true,
  sound_input_needed: true,
  sound_stopped_errors: true,
};

/** One row of the Appearance page's sound list. */
export interface SoundRow {
  /** The sound this row toggles. */
  kind: SoundKind;
  /** The `[ui]` flag the toggle writes. */
  flag: keyof SoundToggles;
  /** The row's title. */
  label: string;
  /** One line of plain-language explanation. */
  description: string;
}

/** The three sound rows, in page order. */
export const SOUND_ROWS: readonly SoundRow[] = [
  {
    kind: "complete",
    flag: "sound_complete",
    label: "Turn finished",
    description: "A short chime when the lead or a worker finishes a turn.",
  },
  {
    kind: "input_needed",
    flag: "sound_input_needed",
    label: "Needs you",
    description: "A soft double-ping when the lead or a worker waits for an approval or an answer.",
  },
  {
    kind: "stopped_errors",
    flag: "sound_stopped_errors",
    label: "Stopped with errors",
    description: "A low tone when a turn stops with errors instead of finishing.",
  },
];

/**
 * Whether `kind` may play under `toggles` — each sound is gated by exactly its
 * own flag. Pure; pinned by test.
 */
export function soundEnabled(kind: SoundKind, toggles: SoundToggles): boolean {
  switch (kind) {
    case "complete":
      return toggles.sound_complete;
    case "input_needed":
      return toggles.sound_input_needed;
    case "stopped_errors":
      return toggles.sound_stopped_errors;
  }
}

// ── Audio ───────────────────────────────────────────────────────────────────

/**
 * The window's single AudioContext, created lazily on the first play (and
 * never in tests/node — `getCtx` returns null there so `playSound` no-ops).
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
 * Play a notification sound. Impure (Web Audio); safe to call anywhere — it
 * no-ops when the platform has no AudioContext and never throws. The
 * Appearance page's per-row **Test** buttons call this directly regardless of
 * the toggles (the click is the user's gesture, so the context unlocks).
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

// ── Live toggles + debounce ─────────────────────────────────────────────────

/**
 * The burst window per sound: repeats of the SAME sound within this many ms
 * are one burst (four workers finishing together play one chime, not four).
 */
export const SOUND_DEBOUNCE_MS = 1500;

/** The toggles the store gates on — updated by the Appearance page's saves. */
let toggles: SoundToggles = { ...DEFAULT_SOUND_TOGGLES };

/** When each sound last actually played (`-Infinity` = never). */
const lastPlayedAtMs: Record<SoundKind, number> = {
  complete: Number.NEGATIVE_INFINITY,
  input_needed: Number.NEGATIVE_INFINITY,
  stopped_errors: Number.NEGATIVE_INFINITY,
};

/**
 * How much a sound needs the human's attention. A higher-priority sound plays
 * even inside a lower-priority one's burst window; the lower one waits.
 */
const SOUND_PRIORITY: Record<SoundKind, number> = {
  complete: 0,
  stopped_errors: 1,
  input_needed: 2,
};

/** The most recent sound of any kind, for the priority rule. */
let lastPlayed: { kind: SoundKind; atMs: number } | null = null;

/** The toggles a just-saved page pushes here, so the next event obeys them. */
export function applySoundToggles(next: Partial<SoundToggles>): SoundToggles {
  toggles = {
    sound_complete: next.sound_complete ?? toggles.sound_complete,
    sound_input_needed: next.sound_input_needed ?? toggles.sound_input_needed,
    sound_stopped_errors: next.sound_stopped_errors ?? toggles.sound_stopped_errors,
  };
  return { ...toggles };
}

/** The toggles the store currently gates on. */
export function currentSoundToggles(): SoundToggles {
  return { ...toggles };
}

/**
 * Reset the module to its startup state (defaults on, no debounce window).
 * Tests call this between cases; the app never needs it.
 */
export function resetSoundState(): void {
  toggles = { ...DEFAULT_SOUND_TOGGLES };
  lastPlayedAtMs.complete = Number.NEGATIVE_INFINITY;
  lastPlayedAtMs.input_needed = Number.NEGATIVE_INFINITY;
  lastPlayedAtMs.stopped_errors = Number.NEGATIVE_INFINITY;
  lastPlayed = null;
}

/**
 * Play `kind` if its toggle is on and it is not a repeat inside its own burst
 * window; returns whether a sound actually played (pure bookkeeping, the audio
 * itself is `playSound`). A sound of strictly higher priority — `input_needed`
 * over `stopped_errors` over `complete` — plays even inside the window of the
 * sound that just played, so a needs-you ping is never swallowed by a chime.
 * `atMs` is injectable for tests.
 */
export function notifySound(kind: SoundKind, atMs: number = Date.now()): boolean {
  if (!soundEnabled(kind, toggles)) return false;
  // Per-sound debounce: a burst of the SAME sound plays once.
  if (atMs - lastPlayedAtMs[kind] < SOUND_DEBOUNCE_MS) return false;
  // A lower-priority (or equally urgent) sound gives way to the one that just
  // played; only a strictly more urgent sound breaks into another's window.
  if (
    lastPlayed !== null &&
    atMs - lastPlayed.atMs < SOUND_DEBOUNCE_MS &&
    SOUND_PRIORITY[kind] <= SOUND_PRIORITY[lastPlayed.kind]
  ) {
    return false;
  }
  lastPlayedAtMs[kind] = atMs;
  lastPlayed = { kind, atMs };
  playSound(kind);
  return true;
}

// ---- Event to sound mapping ------------------------------------------------

/**
 * The sound a lead event asks for, or null. `finished` is the lead's own
 * "turn done"; `approval_request` / `user_question` (and its `host_tool_call`
 * variant) mean the lead is blocked on the human; a non-retrying `error` is
 * the stopped-with-errors case. Process lifecycle events (`exited`,
 * `parked`) and retried errors stay silent.
 */
export function leadEventSound(
  event: { kind?: unknown; retrying?: unknown } | null | undefined,
): SoundKind | null {
  const kind = typeof event?.kind === "string" ? event.kind : "";
  switch (kind) {
    case "finished":
      return "complete";
    case "approval_request":
    case "user_question":
      return "input_needed";
    case "error":
      return event?.retrying === true ? null : "stopped_errors";
    default:
      return null;
  }
}

/**
 * The sound a worker's status asks for, or null: `completed` is a finished
 * turn, `needs_you` a pending approval or question, `failed` stopped with
 * errors. Human-stopped states (`stopped`, `interrupted`, `cancelled`) stay
 * silent.
 */
export function workerStatusSound(status: string | null | undefined): SoundKind | null {
  switch (status) {
    case "completed":
      return "complete";
    case "needs_you":
      return "input_needed";
    case "failed":
      return "stopped_errors";
    default:
      return null;
  }
}

/**
 * The sound for a worker status *transition* — a snapshot that merely
 * restates a status (or a worker first seen in a terminal state) is silent,
 * so opening a project never replays old completions.
 */
export function workerTransitionSound(
  previousStatus: string | null | undefined,
  status: string | null | undefined,
): SoundKind | null {
  if (previousStatus === null || previousStatus === undefined) return null;
  if (previousStatus === status) return null;
  return workerStatusSound(status);
}

// ── Transport ───────────────────────────────────────────────────────────────

/** The Tauri API surface the transport needs, injected for tests and previews. */
export interface AppearanceTransportBindings {
  invoke: typeof invoke;
}

/** The IPC surface the Appearance page needs. */
export interface AppearanceTransport {
  /** The stored toggles plus the stored theme word. */
  get(): Promise<AppearanceSettings>;
  /** Save the supplied toggles (omitted ones keep their value). */
  save(patch: AppearancePatch): Promise<AppearanceSettings>;
}

/**
 * Fill defaults for any field an older host leaves out, so the page and the
 * store never see an undefined toggle.
 */
export function normalizeAppearance(
  value: Partial<AppearanceSettings> | null | undefined,
): AppearanceSettings {
  return {
    sound_complete: value?.sound_complete !== false,
    sound_input_needed: value?.sound_input_needed !== false,
    sound_stopped_errors: value?.sound_stopped_errors !== false,
    theme: typeof value?.theme === "string" && value.theme !== "" ? value.theme : "dark",
  };
}

/** The real IPC transport over the Tauri bindings. */
export function createTauriAppearanceTransport(
  bindings: AppearanceTransportBindings,
): AppearanceTransport {
  return {
    get: () =>
      bindings.invoke<AppearanceSettings>("appearance_settings_get").then(normalizeAppearance),
    save: (patch) =>
      bindings
        .invoke<AppearanceSettings>("appearance_settings_save", { settings: patch })
        .then(normalizeAppearance),
  };
}

// ---- Preview seam (mirrors lib/mcpSettings.ts) -----------------------------

/** The dev-only transport `src/test/previewAppearance.ts` wires up in a browser. */
let previewAppearanceTransport: AppearanceTransport | null = null;

/** Install the preview transport (dev-only; no Tauri host in a browser). */
export function setPreviewAppearanceTransport(transport: AppearanceTransport | null): void {
  previewAppearanceTransport = transport;
}

/**
 * The Appearance page's transport: the Tauri one inside the desktop app, the
 * preview one when a test-side seed installed it, null in a plain browser.
 */
export function createAppearanceTransport(
  bindings?: AppearanceTransportBindings,
): AppearanceTransport | null {
  if (previewAppearanceTransport) return previewAppearanceTransport;
  if (!isTauriRuntime()) return null;
  return createTauriAppearanceTransport(bindings ?? { invoke });
}

/**
 * Read the stored toggles once at startup and cache them for `notifySound`.
 * Failures leave the defaults in place (all on); the page reads the same
 * command and will report a real error if the host is broken.
 */
export async function loadSoundToggles(
  transport: AppearanceTransport | null = createAppearanceTransport(),
): Promise<SoundToggles | null> {
  if (!transport) return null;
  try {
    const settings = await transport.get();
    return applySoundToggles(settings);
  } catch {
    return null;
  }
}

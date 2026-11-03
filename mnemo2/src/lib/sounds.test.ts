import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_SOUND_TOGGLES,
  SOUND_DEBOUNCE_MS,
  SOUND_ROWS,
  TONES,
  applySoundToggles,
  createAppearanceTransport,
  createTauriAppearanceTransport,
  currentSoundToggles,
  leadEventSound,
  loadSoundToggles,
  normalizeAppearance,
  notifySound,
  playSound,
  resetSoundState,
  setPreviewAppearanceTransport,
  soundEnabled,
  workerStatusSound,
  workerTransitionSound,
  type AppearanceSettings,
  type AppearanceTransport,
  type CloseBehavior,
  type SoundKind,
} from "./sounds";

beforeEach(() => {
  resetSoundState();
  setPreviewAppearanceTransport(null);
});

afterEach(() => {
  resetSoundState();
});

describe("tone table", () => {
  it("gives every sound its own short, quiet shape", () => {
    for (const kind of ["complete", "input_needed", "stopped_errors"] as const) {
      const notes = TONES[kind];
      expect(notes.length).toBeGreaterThan(0);
      for (const note of notes) {
        expect(note.durMs).toBeLessThanOrEqual(400);
        expect(note.gain).toBeGreaterThan(0);
        expect(note.gain).toBeLessThanOrEqual(0.15);
      }
      const last = Math.max(...notes.map((note) => note.startMs + note.durMs));
      expect(last).toBeLessThanOrEqual(600);
    }
  });

  it("makes complete ascending, needs-you a double ping, and errors descending", () => {
    expect(TONES.complete[1].freq).toBeGreaterThan(TONES.complete[0].freq);
    expect(TONES.input_needed[0].freq).toBe(TONES.input_needed[1].freq);
    const errors = TONES.stopped_errors;
    for (let index = 1; index < errors.length; index += 1) {
      expect(errors[index].freq).toBeLessThan(errors[index - 1].freq);
    }
    expect(errors.every((note) => note.filterHz !== undefined)).toBe(true);
  });
});

describe("soundEnabled", () => {
  it("gates each sound by exactly its own toggle", () => {
    const rows: Array<[SoundKind, keyof typeof DEFAULT_SOUND_TOGGLES]> = [
      ["complete", "sound_complete"],
      ["input_needed", "sound_input_needed"],
      ["stopped_errors", "sound_stopped_errors"],
    ];
    for (const [kind, flag] of rows) {
      expect(soundEnabled(kind, DEFAULT_SOUND_TOGGLES)).toBe(true);
      expect(soundEnabled(kind, { ...DEFAULT_SOUND_TOGGLES, [flag]: false })).toBe(false);
      // A different flag off never silences this sound.
      const other = rows.find(([, otherFlag]) => otherFlag !== flag)![1];
      expect(soundEnabled(kind, { ...DEFAULT_SOUND_TOGGLES, [other]: false })).toBe(true);
    }
  });

  it("lists one page row per sound, wired to its flag", () => {
    expect(SOUND_ROWS.map((row) => row.kind)).toEqual([
      "complete",
      "input_needed",
      "stopped_errors",
    ]);
    expect(SOUND_ROWS.map((row) => row.flag)).toEqual([
      "sound_complete",
      "sound_input_needed",
      "sound_stopped_errors",
    ]);
  });
});

describe("leadEventSound", () => {
  it("maps a finished turn to the completion chime", () => {
    expect(leadEventSound({ kind: "finished" })).toBe("complete");
  });

  it("maps an approval or a question to the needs-you ping", () => {
    expect(leadEventSound({ kind: "approval_request" })).toBe("input_needed");
    expect(leadEventSound({ kind: "user_question" })).toBe("input_needed");
  });

  it("maps a final error to the stopped-with-errors tone, a retry to nothing", () => {
    expect(leadEventSound({ kind: "error" })).toBe("stopped_errors");
    expect(leadEventSound({ kind: "error", retrying: false })).toBe("stopped_errors");
    expect(leadEventSound({ kind: "error", retrying: true })).toBeNull();
  });

  it("stays silent for lifecycle and unknown events", () => {
    for (const kind of ["exited", "parked", "started", "message", ""]) {
      expect(leadEventSound({ kind })).toBeNull();
    }
    expect(leadEventSound(null)).toBeNull();
    expect(leadEventSound(undefined)).toBeNull();
    expect(leadEventSound({})).toBeNull();
  });
});

describe("worker sounds", () => {
  it("maps completed, needs_you and failed statuses", () => {
    expect(workerStatusSound("completed")).toBe("complete");
    expect(workerStatusSound("needs_you")).toBe("input_needed");
    expect(workerStatusSound("failed")).toBe("stopped_errors");
  });

  it("stays silent for working or human-stopped statuses", () => {
    for (const status of ["dispatching", "working", "idle", "stopped", "interrupted", "cancelled", "merged"]) {
      expect(workerStatusSound(status)).toBeNull();
    }
  });

  it("only fires on a real status change, never on a restated terminal state", () => {
    expect(workerTransitionSound("working", "completed")).toBe("complete");
    expect(workerTransitionSound("working", "needs_you")).toBe("input_needed");
    expect(workerTransitionSound("working", "failed")).toBe("stopped_errors");
    // Same status re-delivered by a later snapshot.
    expect(workerTransitionSound("completed", "completed")).toBeNull();
    // A worker first seen terminal (opening a project) never replays.
    expect(workerTransitionSound(undefined, "completed")).toBeNull();
    expect(workerTransitionSound(null, "needs_you")).toBeNull();
    // A transition into a silent status.
    expect(workerTransitionSound("working", "idle")).toBeNull();
  });
});

describe("notifySound", () => {
  it("plays when the toggle is on and swallows a burst of the same sound", () => {
    expect(notifySound("complete", 10_000)).toBe(true);
    // A team of workers finishing together: one chime.
    expect(notifySound("complete", 10_100)).toBe(false);
    // Past its own window the same sound is heard again.
    expect(notifySound("complete", 10_000 + SOUND_DEBOUNCE_MS)).toBe(true);
  });

  it("lets a needs-you ping through a completion chime's burst window", () => {
    // Regression (review finding needsYouSwallowedByDebounce): the single
    // shared window used to swallow this — the sound that most needs the
    // human's attention — whenever anything else played in the previous
    // 1.5 s.
    expect(notifySound("complete", 10_000)).toBe(true);
    expect(notifySound("input_needed", 10_500)).toBe(true);
    // The ping now owns the moment; a repeat of it is still one burst.
    expect(notifySound("input_needed", 10_600)).toBe(false);
  });

  it("plays a more urgent sound inside a less urgent one's window", () => {
    expect(notifySound("complete", 1_000)).toBe(true);
    expect(notifySound("stopped_errors", 1_400)).toBe(true);
    expect(notifySound("input_needed", 1_800)).toBe(true);
  });

  it("does not let a completion chime follow a needs-you ping inside its window", () => {
    expect(notifySound("complete", 1_000)).toBe(true);
    expect(notifySound("input_needed", 1_200)).toBe(true);
    // Outside the chime's own window (1 000 + 1 500), but the ping still owns
    // the moment: the least urgent sound waits.
    expect(notifySound("complete", 2_600)).toBe(false);
    // Once the ping's window has passed, the chime is heard again.
    expect(notifySound("complete", 2_800)).toBe(true);
  });

  it("honours a toggle flipped at runtime", () => {
    applySoundToggles({ sound_complete: false });
    expect(notifySound("complete", 5_000)).toBe(false);
    // The other sounds are untouched.
    expect(notifySound("input_needed", 5_000)).toBe(true);
    applySoundToggles({ sound_complete: true });
    expect(notifySound("complete", 20_000)).toBe(true);
  });

  it("does not let a silenced burst consume the debounce window", () => {
    applySoundToggles({ sound_stopped_errors: false });
    expect(notifySound("stopped_errors", 1_000)).toBe(false);
    expect(notifySound("complete", 1_100)).toBe(true);
  });
});

describe("toggle cache", () => {
  it("starts with every sound on and merges partial updates", () => {
    expect(currentSoundToggles()).toEqual(DEFAULT_SOUND_TOGGLES);
    applySoundToggles({ sound_input_needed: false });
    expect(currentSoundToggles()).toEqual({
      sound_complete: true,
      sound_input_needed: false,
      sound_stopped_errors: true,
    });
    applySoundToggles({ sound_stopped_errors: false });
    expect(currentSoundToggles().sound_input_needed).toBe(false);
  });

  it("loads the stored toggles through the transport", async () => {
    const transport: AppearanceTransport = {
      get: async () => ({
        sound_complete: false,
        sound_input_needed: true,
        sound_stopped_errors: false,
        theme: "light",
        close_behavior: "auto",
      }),
      save: async () => {
        throw new Error("not used");
      },
    };
    expect(await loadSoundToggles(transport)).toEqual({
      sound_complete: false,
      sound_input_needed: true,
      sound_stopped_errors: false,
    });
    expect(currentSoundToggles().sound_complete).toBe(false);
    expect(notifySound("complete", 1_000)).toBe(false);
  });

  it("keeps the defaults when the read fails", async () => {
    const transport: AppearanceTransport = {
      get: async () => {
        throw new Error("ipc down");
      },
      save: async () => {
        throw new Error("not used");
      },
    };
    expect(await loadSoundToggles(transport)).toBeNull();
    expect(currentSoundToggles()).toEqual(DEFAULT_SOUND_TOGGLES);
  });

  it("is a no-op without a transport (plain browser)", async () => {
    expect(createAppearanceTransport()).toBeNull();
    expect(await loadSoundToggles(null)).toBeNull();
  });
});

describe("wire normalization", () => {
  it("fills defaults for fields an older host leaves out", () => {
    expect(normalizeAppearance(null)).toEqual({
      ...DEFAULT_SOUND_TOGGLES,
      theme: "dark",
      close_behavior: "auto",
    });
    expect(normalizeAppearance({ sound_complete: false })).toEqual({
      sound_complete: false,
      sound_input_needed: true,
      sound_stopped_errors: true,
      theme: "dark",
      close_behavior: "auto",
    });
    expect(normalizeAppearance({ theme: "" }).theme).toBe("dark");
    expect(normalizeAppearance({ theme: "system" }).theme).toBe("system");
    expect(normalizeAppearance({ close_behavior: "tray" }).close_behavior).toBe("tray");
    expect(
      normalizeAppearance({ close_behavior: "bogus" as CloseBehavior }).close_behavior,
    ).toBe("auto");
  });

  it("talks to the two appearance commands through bindings", async () => {
    const calls: Array<{ command: string; args: unknown }> = [];
    const invoke = (async (command: string, args?: unknown) => {
      calls.push({ command, args });
      const settings: AppearanceSettings = {
        sound_complete: true,
        sound_input_needed: false,
        sound_stopped_errors: true,
        theme: "dark",
        close_behavior: "quit",
      };
      return settings;
    }) as unknown as typeof import("@tauri-apps/api/core").invoke;
    const transport = createTauriAppearanceTransport({ invoke });
    const loaded = await transport.get();
    expect(loaded.sound_input_needed).toBe(false);
    expect(loaded.close_behavior).toBe("quit");
    await transport.save({ sound_input_needed: true });
    expect(calls.map((call) => call.command)).toEqual([
      "appearance_settings_get",
      "appearance_settings_save",
    ]);
    expect(calls[1].args).toEqual({ settings: { sound_input_needed: true } });
  });
});

describe("playSound", () => {
  it("never throws where the platform has no audio (headless tests)", () => {
    expect(() => playSound("complete")).not.toThrow();
    expect(() => playSound("input_needed")).not.toThrow();
    expect(() => playSound("stopped_errors")).not.toThrow();
  });
});

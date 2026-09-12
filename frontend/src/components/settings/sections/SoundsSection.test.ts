// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * SoundsSection contract tests — the section is the Settings surface for the
 * three notification-sound toggles. No React DOM test infra (vitest runs in
 * `node`), so the wiring is pinned statically in the style of
 * `src/lib/ipc-contract.test.ts` (Vite `?raw` import): the save handle must
 * persist ALL THREE flags (store setters + the config.toml patch keys) and
 * the preview buttons must call `playSound` directly (auditioning is not
 * gated by the enable flags).
 */

import { describe, expect, it } from "vitest";
import source from "./SoundsSection.tsx?raw";

describe("SoundsSection save contract", () => {
  it("persists all three toggles through the store setters", () => {
    expect(source).toContain("s.setSoundComplete(draft.soundComplete)");
    expect(source).toContain("s.setSoundInput(draft.soundInput)");
    expect(source).toContain("s.setSoundDoom(draft.soundDoom)");
  });

  it("persists all three toggles through the config.toml [ui] patch", () => {
    expect(source).toContain("sound_complete: draft.soundComplete");
    expect(source).toContain("sound_input_needed: draft.soundInput");
    expect(source).toContain("sound_stopped_errors: draft.soundDoom");
  });

  it("previews play the real sounds regardless of the enable flags", () => {
    // One preview per sound, calling playSound with its kind — the click is
    // the user's gesture (and the AudioContext unlock).
    expect(source).toContain('playSound(kind)');
    expect(source).toContain('"complete"');
    expect(source).toContain('"input"');
    expect(source).toContain('"doom"');
  });

  it("follows the ChatSection draft model (snapshot + serializeSound)", () => {
    expect(source).toContain("serializeSound");
    // Discarding is a no-op: the draft writes config only on save.
    expect(source).toContain("await saveSettings(");
  });
});

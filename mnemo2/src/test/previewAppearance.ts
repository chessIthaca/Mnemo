/**
 * Dev-only seed for the `settings-appearance` preview scene: an in-memory
 * Appearance transport over a seeded snapshot, so the page's sound toggles,
 * theme and close choice visibly save (the Test buttons stay clickable) in
 * a plain browser (no Tauri host).
 *
 * Test-side code like the rest of this folder: `index.html` never reaches
 * it, so `vite build` bundles none of it and `npm run check:no-mock` keeps
 * passing.
 */

import {
  setPreviewAppearanceTransport,
  type AppearancePatch,
  type AppearanceSettings,
  type AppearanceTransport,
} from "../lib/sounds";

/** The snapshot the scene shows: one toggle off, the other two on, and a
 * non-default close choice. */
export function previewAppearanceSettings(): AppearanceSettings {
  return {
    sound_complete: true,
    sound_input_needed: true,
    sound_stopped_errors: false,
    theme: "dark",
    close_behavior: "tray",
  };
}

/** A fake transport over the seeded snapshot; saves mutate it like the real one. */
export function createPreviewAppearanceTransport(): AppearanceTransport {
  let snapshot = previewAppearanceSettings();
  return {
    get: async () => ({ ...snapshot }),
    save: async (patch: AppearancePatch) => {
      snapshot = { ...snapshot, ...patch };
      return { ...snapshot };
    },
  };
}

/** Wire the preview transport for the `settings-appearance` scene; returns the restore. */
export function seedPreviewAppearance(): () => void {
  setPreviewAppearanceTransport(createPreviewAppearanceTransport());
  return () => setPreviewAppearanceTransport(null);
}

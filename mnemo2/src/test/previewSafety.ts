/**
 * Dev-only seed for the `settings-safety` preview scene: an in-memory
 * Safety transport over a seeded snapshot, so the page's Save buttons
 * visibly take effect in a plain browser (no Tauri host).
 *
 * Test-side code like the rest of this folder: `index.html` never reaches
 * it, so `vite build` bundles none of it and `npm run check:no-mock` keeps
 * passing.
 */

import {
  setPreviewSafetyTransport,
  type SafetyModeId,
  type SafetyRule,
  type SafetySettings,
  type SafetyTransport,
} from "../lib/safetySettings";

/** The snapshot the scene shows: a project-scoped mode and two rules. */
export function previewSafetySettings(): SafetySettings {
  return {
    mode: "auto-approve-project",
    rules: [
      { tool: "shell", pattern: "cargo test", kind: "command_class" },
      { tool: "file_write", pattern: "^file_write:.*\\.md$", kind: "literal" },
    ],
    project_open: true,
    live_lead: true,
  };
}

/** A copy of the seeded snapshot (the page never mutates the transport's own state). */
function clone(snapshot: SafetySettings): SafetySettings {
  return { ...snapshot, rules: snapshot.rules.map((rule) => ({ ...rule })) };
}

/** A fake transport over the seeded snapshot; saves mutate it like the real ones would. */
export function createPreviewSafetyTransport(): SafetyTransport {
  let snapshot = previewSafetySettings();
  return {
    get: async () => clone(snapshot),
    saveMode: async (mode: SafetyModeId) => {
      snapshot = { ...snapshot, mode };
      return clone(snapshot);
    },
    saveRules: async (rules: SafetyRule[]) => {
      snapshot = { ...snapshot, rules: rules.map((rule) => ({ ...rule })) };
      return clone(snapshot);
    },
  };
}

/** Wire the preview transport for the `settings-safety` scene; returns the restore. */
export function seedPreviewSafety(): () => void {
  setPreviewSafetyTransport(createPreviewSafetyTransport());
  return () => setPreviewSafetyTransport(null);
}

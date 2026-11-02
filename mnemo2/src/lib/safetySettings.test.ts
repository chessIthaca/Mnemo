import { describe, expect, it, vi } from "vitest";
import {
  SAFETY_MODES,
  SAFETY_RULE_KINDS,
  createSafetyTransport,
  createTauriSafetyTransport,
  emptyRule,
  normalizeSafetySettings,
  ruleDraft,
  rulesDiffer,
  safetyModeMeta,
  setPreviewSafetyTransport,
  type SafetyRule,
  type SafetyTransport,
} from "./safetySettings";

/** A wire snapshot with one literal rule whose `kind` was omitted (serde skips the default). */
const WIRE = {
  mode: "auto-approve-project",
  rules: [
    { tool: "file_write", pattern: "^file_write:.*" },
    { tool: "shell", pattern: "cargo test", kind: "command_class" as const },
  ],
  project_open: true,
  live_lead: true,
};

/** The four words the backend's `parse_safety_mode` accepts. */
const MODE_WORDS = ["approve-each-action", "auto-read-approve-writes", "auto-approve-project", "autonomous"];

describe("safety mode metadata", () => {
  it("lists exactly the four [general] safety modes, least to most permissive", () => {
    expect(SAFETY_MODES.map((mode) => mode.id)).toEqual(MODE_WORDS);
    expect(new Set(SAFETY_MODES.map((mode) => mode.id)).size).toBe(4);
  });

  it("gives every mode a label and a one-line explanation", () => {
    for (const mode of SAFETY_MODES) {
      expect(mode.label.trim(), mode.id).not.toBe("");
      expect(mode.explanation.trim(), mode.id).not.toBe("");
      expect(mode.explanation.split("\n"), mode.id).toHaveLength(1);
    }
  });

  it("marks the no-prompt mode risky and every asking mode safe", () => {
    expect(SAFETY_MODES.filter((mode) => mode.risky).map((mode) => mode.id)).toEqual(["autonomous"]);
    expect(safetyModeMeta("autonomous").risky).toBe(true);
    expect(safetyModeMeta("approve-each-action").risky).toBe(false);
  });

  it("falls back to the default mode's metadata for an unknown word", () => {
    // The wire is ours, but a config written by a newer build must not crash the page.
    expect(safetyModeMeta("no-such-mode" as never).id).toBe("approve-each-action");
  });
});

describe("safety settings normalization", () => {
  it("defaults an omitted rule kind to literal and keeps file order", () => {
    expect(normalizeSafetySettings(WIRE)).toEqual({
      mode: "auto-approve-project",
      rules: [
        { tool: "file_write", pattern: "^file_write:.*", kind: "literal" },
        { tool: "shell", pattern: "cargo test", kind: "command_class" },
      ],
      project_open: true,
      live_lead: true,
    });
  });

  it("names both rule kinds with a blurb", () => {
    expect(SAFETY_RULE_KINDS.map((kind) => kind.value)).toEqual(["literal", "command_class"]);
    for (const kind of SAFETY_RULE_KINDS) expect(kind.blurb.trim()).not.toBe("");
  });
});

describe("rule drafts", () => {
  it("starts a blank row on the literal kind", () => {
    expect(emptyRule()).toEqual({ tool: "", pattern: "", kind: "literal" });
  });

  it("trims the tool but keeps the pattern byte-for-byte", () => {
    const parsed = ruleDraft({ tool: "  file_write ", pattern: "^file_write: .*$", kind: "literal" });
    expect(parsed).toEqual({
      rule: { tool: "file_write", pattern: "^file_write: .*$", kind: "literal" },
    });
  });

  it("rejects a blank tool or pattern with a plain message", () => {
    expect(ruleDraft({ tool: "  ", pattern: ".*", kind: "literal" })).toEqual({
      error: "Enter the tool the rule applies to.",
    });
    expect(ruleDraft({ tool: "shell", pattern: " ", kind: "command_class" })).toEqual({
      error: "Enter the pattern the rule matches.",
    });
  });

  it("compares rule lists for the editor's dirty check", () => {
    const a: SafetyRule[] = [{ tool: "shell", pattern: "cargo test", kind: "command_class" }];
    expect(rulesDiffer(a, [{ tool: "shell", pattern: "cargo test", kind: "command_class" }])).toBe(false);
    expect(rulesDiffer(a, [{ tool: "shell", pattern: "cargo build", kind: "command_class" }])).toBe(true);
    expect(rulesDiffer(a, [])).toBe(true);
  });
});

describe("safety transport", () => {
  it("maps every method to its command and normalizes the reply", async () => {
    const invoked: { command: string; args?: Record<string, unknown> }[] = [];
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      invoked.push({ command, args });
      return WIRE;
    });
    const transport = createTauriSafetyTransport({
      invoke: invoke as unknown as typeof import("@tauri-apps/api/core").invoke,
    });
    const rules: SafetyRule[] = [{ tool: "file_write", pattern: "^file_write:.*", kind: "literal" }];

    await expect(transport.get()).resolves.toEqual(normalizeSafetySettings(WIRE));
    await expect(transport.saveMode("autonomous")).resolves.toEqual(normalizeSafetySettings(WIRE));
    await expect(transport.saveRules(rules)).resolves.toEqual(normalizeSafetySettings(WIRE));

    expect(invoked).toEqual([
      { command: "safety_settings_get", args: undefined },
      { command: "save_safety_mode", args: { mode: "autonomous" } },
      { command: "save_safety_rules", args: { rules } },
    ]);
  });

  it("prefers the preview transport and falls back to null in a plain browser", async () => {
    const planted: SafetyTransport = {
      get: async () => normalizeSafetySettings(WIRE),
      saveMode: async () => normalizeSafetySettings(WIRE),
      saveRules: async () => normalizeSafetySettings(WIRE),
    };
    expect(createSafetyTransport()).toBeNull();
    setPreviewSafetyTransport(planted);
    expect(createSafetyTransport()).toBe(planted);
    await expect(createSafetyTransport()?.get()).resolves.toMatchObject({ mode: "auto-approve-project" });
    setPreviewSafetyTransport(null);
    expect(createSafetyTransport()).toBeNull();
  });
});

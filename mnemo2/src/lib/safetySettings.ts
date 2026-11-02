import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/**
 * Safety settings for the Safety page: the transport to
 * `safety_settings_get` / `save_safety_mode` / `save_safety_rules`, the
 * four `[general] safety` modes with their one-line explanations, and the
 * rule shapes the editor saves. The wire shapes mirror
 * `SafetySettingsWire` in `src-tauri/src/mnemo2/safety_settings.rs` — the
 * JSON keys are snake_case, like every other Mnemo2 DTO.
 */

// ── Wire shapes (the `safety_settings_get` snapshot) ────────────────────────

/** The four `[general] safety` modes, least to most permissive. */
export type SafetyModeId =
  | "approve-each-action"
  | "auto-read-approve-writes"
  | "auto-approve-project"
  | "autonomous";

/** How a rule's `pattern` is interpreted. */
export type SafetyRuleKind = "literal" | "command_class";

/** One `[[rule]]` in the project's `safety.toml`. */
export interface SafetyRule {
  tool: string;
  pattern: string;
  kind: SafetyRuleKind;
}

/** The Safety page snapshot. */
export interface SafetySettings {
  /** The saved mode word. */
  mode: SafetyModeId;
  /** `<project>/.coding/safety.toml`, in file order. */
  rules: SafetyRule[];
  /** A project is open — the rules editor is on. */
  project_open: boolean;
  /** An open lead is live — a mode save applies to its next action. */
  live_lead: boolean;
}

/** The raw wire snapshot: a literal rule's `kind` is omitted (serde skips the default). */
interface SafetySettingsWire extends Omit<SafetySettings, "mode" | "rules"> {
  mode: string;
  rules: { tool: string; pattern: string; kind?: SafetyRuleKind }[];
}

// ── Mode metadata ──────────────────────────────────────────────────────────

export interface SafetyModeMeta {
  id: SafetyModeId;
  label: string;
  /** One plain line: what the lead may do without asking. */
  explanation: string;
  /** Marked as risky in the UI (`autonomous` runs with no prompts at all). */
  risky: boolean;
}

export const SAFETY_MODES: readonly SafetyModeMeta[] = [
  {
    id: "approve-each-action",
    label: "Ask for every action",
    explanation: "Each file write, shell command and other change waits for your approval.",
    risky: false,
  },
  {
    id: "auto-read-approve-writes",
    label: "Auto-approve reads",
    explanation: "Reading files and searching run without asking; writes and commands still ask.",
    risky: false,
  },
  {
    id: "auto-approve-project",
    label: "Auto-approve inside the project",
    explanation: "Changes that only touch this project's files run without asking; shell commands still ask.",
    risky: false,
  },
  {
    id: "autonomous",
    label: "Run without asking",
    explanation: "Every action runs without a prompt, so the agent can do anything its tools allow.",
    risky: true,
  },
];

/** The metadata for a mode word; unknown words read as the default mode. */
export function safetyModeMeta(mode: SafetyModeId): SafetyModeMeta {
  return SAFETY_MODES.find((entry) => entry.id === mode) ?? SAFETY_MODES[0];
}

/** The rule-kind choices the editor offers, with a one-line blurb each. */
export const SAFETY_RULE_KINDS: readonly { value: SafetyRuleKind; label: string; blurb: string }[] = [
  {
    value: "literal",
    label: "Regex",
    blurb: "Matches the tool call's signature, e.g. \"^file_write:.*\".",
  },
  {
    value: "command_class",
    label: "Command class",
    blurb: "Matches a normalized shell class, e.g. \"cargo test\".",
  },
];

// ── Helpers ─────────────────────────────────────────────────────────────────

/** Default a wire snapshot into the page's shape (a literal kind is omitted on the wire). */
export function normalizeSafetySettings(wire: SafetySettingsWire): SafetySettings {
  return {
    mode: wire.mode as SafetyModeId,
    rules: wire.rules.map((rule) => ({ tool: rule.tool, pattern: rule.pattern, kind: rule.kind ?? "literal" })),
    project_open: wire.project_open,
    live_lead: wire.live_lead,
  };
}

/** A blank rule row for the editor's Add button. */
export function emptyRule(): SafetyRule {
  return { tool: "", pattern: "", kind: "literal" };
}

/**
 * Check a rule row before saving: both fields need text; the regex's
 * syntax is validated by the backend (`Rule::new`), whose error lands in
 * the page's alert. Returns the trimmed rule or a plain message.
 */
export function ruleDraft(rule: SafetyRule): { rule: SafetyRule } | { error: string } {
  const tool = rule.tool.trim();
  if (tool === "") return { error: "Enter the tool the rule applies to." };
  if (rule.pattern.trim() === "") return { error: "Enter the pattern the rule matches." };
  // A tool name never carries surrounding spaces; the pattern is kept
  // byte-for-byte (a literal regex may anchor on one).
  return { rule: { tool, pattern: rule.pattern, kind: rule.kind } };
}

/** Whether two rule lists differ (the editor's dirty check). */
export function rulesDiffer(a: SafetyRule[], b: SafetyRule[]): boolean {
  return JSON.stringify(a) !== JSON.stringify(b);
}

// ── Transport ───────────────────────────────────────────────────────────────

/** The Tauri API surface the transport needs, injected for tests and previews. */
export interface SafetyTransportBindings {
  invoke: typeof invoke;
}

/** The IPC surface the Safety page needs. */
export interface SafetyTransport {
  /** The full snapshot: the saved mode and (with a project open) its rules. */
  get(): Promise<SafetySettings>;
  /** Save `[general] safety`; applies to the lead's next action. */
  saveMode(mode: SafetyModeId): Promise<SafetySettings>;
  /** Replace the open project's rules (the whole list). */
  saveRules(rules: SafetyRule[]): Promise<SafetySettings>;
}

/** The real IPC transport over the Tauri bindings. */
export function createTauriSafetyTransport(bindings: SafetyTransportBindings): SafetyTransport {
  return {
    get: () => bindings.invoke<SafetySettingsWire>("safety_settings_get").then(normalizeSafetySettings),
    saveMode: (mode) =>
      bindings.invoke<SafetySettingsWire>("save_safety_mode", { mode }).then(normalizeSafetySettings),
    saveRules: (rules) =>
      bindings.invoke<SafetySettingsWire>("save_safety_rules", { rules }).then(normalizeSafetySettings),
  };
}

// ── Preview seam (mirrors lib/layaSettings.ts) ──────────────────────────────

/** The dev-only transport `src/test/previewSafety.ts` wires up in a browser. */
let previewSafetyTransport: SafetyTransport | null = null;

/** Install the preview transport (dev-only; no Tauri host in a browser). */
export function setPreviewSafetyTransport(transport: SafetyTransport | null): void {
  previewSafetyTransport = transport;
}

/**
 * The Safety page's transport: the Tauri one inside the desktop app, the
 * preview one when a test-side seed installed it, null in a plain browser
 * (the dialog shows its empty state).
 */
export function createSafetyTransport(bindings?: SafetyTransportBindings): SafetyTransport | null {
  if (previewSafetyTransport) return previewSafetyTransport;
  if (!isTauriRuntime()) return null;
  return createTauriSafetyTransport(bindings ?? { invoke });
}

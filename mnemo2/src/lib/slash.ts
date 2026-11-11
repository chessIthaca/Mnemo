// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Slash commands for the lead composer.
//
// A draft is a command only when its first token is "/" plus a known command
// name; anything else (for example "/usr/bin is missing") is an ordinary
// message for the lead. Everything here is pure so the parsing, the menu filter
// and the model-name resolution stay unit-testable without the store.

import type { LeadModel } from "./types";

/**
 * One command parsed out of the composer draft.
 *
 * `new` covers both `/new` and `/clear` (one action, two names), and `invalid`
 * carries the message the composer toasts verbatim while keeping the draft.
 */
export type SlashCommand =
  | { kind: "new" } // /new and /clear
  | { kind: "compact" }
  | { kind: "stop" }
  | { kind: "help" }
  | { kind: "model"; name: string; effort: string | null }
  | { kind: "invalid"; message: string };

/** One entry of the command menu and of the `/help` sheet. */
export interface SlashCommandInfo {
  name: string;
  usage: string;
  description: string;
  /** Whether the command takes text after its name (`/model …`). */
  takesArgument: boolean;
}

/** Every command the lead composer understands, in menu order. */
export const SLASH_COMMANDS: SlashCommandInfo[] = [
  { name: "new", usage: "/new", description: "Start a fresh conversation (clears the lead's history)", takesArgument: false },
  { name: "clear", usage: "/clear", description: "Same as /new", takesArgument: false },
  { name: "compact", usage: "/compact", description: "Summarize older messages to free context", takesArgument: false },
  { name: "model", usage: "/model <name> [effort]", description: "Switch the lead's model", takesArgument: true },
  { name: "stop", usage: "/stop", description: "Stop the lead's current turn", takesArgument: false },
  { name: "help", usage: "/help", description: "Show these commands", takesArgument: false },
];

/** The command names a draft may start with. */
const COMMAND_NAMES = new Set(SLASH_COMMANDS.map((command) => command.name));

/** The `Usage: …` line `/model` repeats when it is typed without a name. */
const MODEL_USAGE = "Usage: /model <name> [effort]";

/**
 * Parse a composer draft into a command, or `null` when it is not one.
 *
 * `null` means the draft is an ordinary message: it does not start with "/",
 * or its first token is not a known command name. A known command that carries
 * text it cannot use comes back as `{ kind: "invalid" }` so the composer can
 * explain instead of sending it.
 */
export function parseSlash(input: string): SlashCommand | null {
  const text = input.trim();
  if (!text.startsWith("/")) return null;
  const body = text.slice(1);
  const separator = body.search(/\s/);
  const token = (separator === -1 ? body : body.slice(0, separator)).toLowerCase();
  const rest = separator === -1 ? "" : body.slice(separator).trim();
  if (!COMMAND_NAMES.has(token)) return null;

  if (token === "model") {
    const parts = rest ? rest.split(/\s+/) : [];
    if (parts.length === 0) return { kind: "model", name: "", effort: null };
    if (parts.length > 2) return { kind: "invalid", message: MODEL_USAGE };
    return { kind: "model", name: parts[0], effort: parts[1] ?? null };
  }
  if (rest) return { kind: "invalid", message: `/${token} takes no arguments.` };
  if (token === "new" || token === "clear") return { kind: "new" };
  if (token === "compact") return { kind: "compact" };
  if (token === "stop") return { kind: "stop" };
  return { kind: "help" };
}

/**
 * The commands to suggest while the draft is still the command name.
 *
 * Empty unless the draft starts with "/" and holds no whitespace yet: once the
 * user types a space they are writing arguments, so the menu steps aside.
 */
export function filterSlashCommands(input: string): SlashCommandInfo[] {
  if (!input.startsWith("/") || /\s/.test(input)) return [];
  const token = input.slice(1).toLowerCase();
  if (!token) return SLASH_COMMANDS;
  return SLASH_COMMANDS.filter((command) => command.name.startsWith(token));
}

/** One configured model resolved from a `/model` name, or why it did not. */
export type ResolvedModel = { spec: string } | { error: string };

/**
 * Resolve a `/model` argument to a configured model spec.
 *
 * Case-insensitive, most specific first: the exact `endpoint/model` spec, then
 * the bare model id (the part after the endpoint), then a unique substring of
 * the spec. A miss, or a name matching several models, returns the message the
 * composer toasts verbatim.
 */
export function resolveModel(name: string, models: LeadModel[]): ResolvedModel {
  if (models.length === 0) return { error: "No models are configured." };
  const wanted = name.toLowerCase();
  const specs = models.map((model) => model.spec);

  const exact = models.find((model) => model.spec.toLowerCase() === wanted);
  if (exact) return { spec: exact.spec };

  const byId = models.filter((model) => model.model.toLowerCase() === wanted);
  if (byId.length === 1) return { spec: byId[0].spec };
  if (byId.length > 1) {
    return {
      error: `"${name}" matches several models: ${byId.map((model) => model.spec).join(", ")} — type more of the name.`,
    };
  }

  const bySubstring = models.filter((model) => model.spec.toLowerCase().includes(wanted));
  if (bySubstring.length === 1) return { spec: bySubstring[0].spec };
  if (bySubstring.length > 1) {
    return {
      error: `"${name}" matches several models: ${bySubstring.map((model) => model.spec).join(", ")} — type more of the name.`,
    };
  }

  return { error: `No model matches "${name}". Models: ${specs.join(", ")}` };
}

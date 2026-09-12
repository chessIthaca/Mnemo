// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Slash command parser — mirrors the Rust slash module.

export type SlashCommand =
  | { type: "clear" }
  | { type: "compact" }
  | { type: "new" }
  | { type: "model"; name: string }
  | { type: "provider"; name: string }
  | { type: "help" }
  | { type: "save"; path: string | null }
  | { type: "load"; path: string }
  | { type: "panel" }
  | { type: "unknown"; raw: string };

export function parseSlash(input: string): SlashCommand | null {
  const trimmed = input.trim();
  if (!trimmed.startsWith("/")) return null;
  const body = trimmed.slice(1);
  const parts = body.split(/\s+/);
  const cmd = parts[0] || "";
  const arg = parts.slice(1).join(" ");

  switch (cmd) {
    case "clear":
      return { type: "clear" };
    case "compact":
      return { type: "compact" };
    case "new":
      return { type: "new" };
    case "model":
      return { type: "model", name: arg };
    case "provider":
      return { type: "provider", name: arg };
    case "help":
      return { type: "help" };
    case "save":
      return { type: "save", path: arg || null };
    case "load":
      return { type: "load", path: arg };
    case "panel":
      return { type: "panel" };
    default:
      return { type: "unknown", raw: trimmed };
  }
}

export const HELP_TEXT = `Slash commands:
  /clear              Clear the conversation
  /compact            Compact the context (summarize old messages; mid-task work resumes afterwards)
  /new                Start a fresh conversation (clear all context)
  /model <name>       Switch the model (restart to take effect)
  /provider <name>    Switch the provider (restart to take effect)
  /save [path]        Save the conversation to disk
  /load <path>        Load a conversation from disk
  /panel              Toggle the right panel
  /help               Show this help

Commands execute in every agent state — even while the agent is working (they never steer).`;

// ── Command metadata (for the autocomplete menu) ────────────────────────────

/** One slash command as shown in the autocomplete menu. */
export interface SlashCommandInfo {
  /** Command name without the leading slash (e.g. "model"). */
  name: string;
  /** Usage string (e.g. "/model <name>"). */
  usage: string;
  /** One-line description shown next to the usage. */
  description: string;
  /** Optional hint shown inline (e.g. a restart or path requirement). */
  hint?: string;
  /**
   * True when the command expects an argument after the name. Drives Enter
   * behavior: argument-less commands run immediately once fully typed
   * (resolveArglessCommand); argument-taking commands complete to "/name "
   * for argument entry.
   */
  takesArgument: boolean;
}

/**
 * Build the default save path (project-relative): the backend resolves an
 * empty path to `.coding/conversations/<timestamp>.json` under the project
 * root, so the frontend mirrors that when pre-filling `/save`.
 */
export function defaultSavePath(): string {
  return `.coding/conversations/${Date.now()}.json`;
}

/** All slash commands, in menu display order. */
export const SLASH_COMMANDS: SlashCommandInfo[] = [
  {
    name: "clear",
    usage: "/clear",
    description: "Clear the conversation",
    takesArgument: false,
  },
  {
    name: "compact",
    usage: "/compact",
    description: "Compact the context (summarize old messages)",
    hint: "announces start + result; mid-task work resumes after",
    takesArgument: false,
  },
  {
    name: "new",
    usage: "/new",
    description: "Start a fresh conversation (clear all context)",
    hint: "clears the entire history",
    takesArgument: false,
  },
  {
    name: "model",
    usage: "/model <name>",
    description: "Switch the model",
    hint: "takes effect on restart",
    takesArgument: true,
  },
  {
    name: "provider",
    usage: "/provider <name>",
    description: "Switch the provider",
    hint: "takes effect on restart",
    takesArgument: true,
  },
  {
    name: "save",
    usage: "/save [path]",
    description: "Save the conversation to disk",
    hint: "no path → saves to .coding/conversations/<timestamp>.json",
    takesArgument: true,
  },
  {
    name: "load",
    usage: "/load <path>",
    description: "Load a conversation from disk",
    hint: "path required (project-relative)",
    takesArgument: true,
  },
  {
    name: "panel",
    usage: "/panel",
    description: "Toggle the right panel",
    takesArgument: false,
  },
  {
    name: "help",
    usage: "/help",
    description: "Show help",
    takesArgument: false,
  },
];

/**
 * Filter the command list against a partial input that starts with `/`.
 * Matches on the command-name prefix (the first whitespace-delimited token).
 * Returns all commands for a bare `/`.
 */
export function filterSlashCommands(input: string): SlashCommandInfo[] {
  if (!input.startsWith("/")) return [];
  const token = input.slice(1).split(/\s+/)[0]?.toLowerCase() ?? "";
  if (!token) return SLASH_COMMANDS;
  return SLASH_COMMANDS.filter((c) => c.name.startsWith(token));
}

/**
 * Resolve a fully-typed, argument-less slash command ("/compact",
 * "/compact " — surrounding whitespace tolerated). Returns the command's
 * info, or null when the input is a partial name ("/c"), an
 * argument-taking command ("/model"), carries arguments ("/compact x"),
 * or is not a slash input. Drives the InputBar's Enter key: a resolved
 * command runs immediately instead of being re-completed (the pre-fix
 * loop: completion appended a trailing space, the has-args check trimmed
 * it away, Enter re-completed forever). Case-sensitive, mirroring
 * parseSlash's switch.
 */
export function resolveArglessCommand(input: string): SlashCommandInfo | null {
  const trimmed = input.trim();
  if (!trimmed.startsWith("/")) return null;
  const body = trimmed.slice(1);
  if (!body || /\s/.test(body)) return null;
  return SLASH_COMMANDS.find((c) => c.name === body && !c.takesArgument) ?? null;
}

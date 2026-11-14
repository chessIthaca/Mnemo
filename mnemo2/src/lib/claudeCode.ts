/**
 * The Settings → Claude Code page's data: the plugin, helper and CLI state the
 * desktop shell reports (`claude_code_status`), the Connect action
 * (`claude_code_connect`), the two commands a user can run by hand, and the
 * rule that keeps a developer's own `mnemo` marketplace (a source checkout)
 * out of Connect's way.
 */

import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

export type MarketplaceState = "missing" | "mnemo2" | "other";

/** The Settings → Claude Code snapshot (Rust `ClaudeCodeStatus`). */
export interface ClaudeCodeStatus {
  helper_path: string | null;
  plugin_dir: string;
  cli_path: string | null;
  marketplace: MarketplaceState;
  marketplace_path: string | null;
  plugin_installed: boolean;
  plugin_version: string | null;
  error: string | null;
}

/** One `claude` command Connect ran. */
export interface ConnectStep {
  command: string;
  ok: boolean;
  output: string;
}

/** What Connect did. */
export interface ClaudeConnectResult {
  ok: boolean;
  message: string;
  steps: ConnectStep[];
}

/** What the page needs from the desktop shell. */
export interface ClaudeCodeTransport {
  status(): Promise<ClaudeCodeStatus>;
  connect(): Promise<ClaudeConnectResult>;
}

/** The shell transport; null in a plain browser. */
export function createClaudeCodeTransport(): ClaudeCodeTransport | null {
  if (!isTauriRuntime()) return null;
  return {
    status: () => invoke<ClaudeCodeStatus>("claude_code_status"),
    connect: () => invoke<ClaudeConnectResult>("claude_code_connect"),
  };
}

/** The two commands that register the plugin by hand. */
export function manualCommands(pluginDir: string): string[] {
  return [`claude plugin marketplace add "${pluginDir}"`, "claude plugin install mnemo@mnemo"];
}

/** Connect needs the helper, the CLI, and no other mnemo marketplace. */
export function canConnect(status: ClaudeCodeStatus): boolean {
  return status.helper_path !== null && status.cli_path !== null && status.marketplace !== "other";
}

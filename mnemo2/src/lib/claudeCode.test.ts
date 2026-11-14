import { describe, expect, it } from "vitest";
import { canConnect, manualCommands, type ClaudeCodeStatus } from "./claudeCode";

/** The snapshot a healthy install reports before the plugin is registered. */
function status(overrides: Partial<ClaudeCodeStatus> = {}): ClaudeCodeStatus {
  return {
    helper_path: "C:\\Users\\me\\.mnemo\\bin\\mnemo2-mcp.exe",
    plugin_dir: "C:\\Users\\me\\.mnemo\\claude-plugin",
    cli_path: "C:\\Users\\me\\AppData\\Local\\Programs\\claude\\claude.exe",
    marketplace: "missing",
    marketplace_path: null,
    plugin_installed: false,
    plugin_version: null,
    error: null,
    ...overrides,
  };
}

describe("manualCommands", () => {
  it("quotes the plugin folder in the marketplace add command and installs the plugin second", () => {
    const commands = manualCommands("C:\\Users\\me\\.mnemo\\claude-plugin");

    expect(commands[0]).toBe('claude plugin marketplace add "C:\\Users\\me\\.mnemo\\claude-plugin"');
    expect(commands[1]).toBe("claude plugin install mnemo@mnemo");
    expect(commands).toHaveLength(2);
  });
});

describe("canConnect", () => {
  it("allows Connect with the helper, the CLI and no marketplace", () => {
    expect(canConnect(status())).toBe(true);
  });

  it("allows Connect when Mnemo2's own marketplace is already registered", () => {
    expect(canConnect(status({ marketplace: "mnemo2" }))).toBe(true);
  });

  it("refuses Connect without the helper", () => {
    expect(canConnect(status({ helper_path: null }))).toBe(false);
  });

  it("refuses Connect without the CLI", () => {
    expect(canConnect(status({ cli_path: null }))).toBe(false);
  });

  it("refuses Connect when another mnemo marketplace is registered", () => {
    expect(canConnect(status({ marketplace: "other", marketplace_path: "C:\\Mnemo" }))).toBe(false);
  });
});

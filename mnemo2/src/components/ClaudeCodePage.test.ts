import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import ClaudeCodePage from "./ClaudeCodePage.svelte";
import type { ClaudeCodeStatus, ClaudeCodeTransport, ClaudeConnectResult } from "../lib/claudeCode";

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

/** A fake transport over one snapshot, with spies on both calls. */
function makeTransport(overrides: Partial<ClaudeCodeStatus> = {}) {
  const readStatus = vi.fn(async () => status(overrides));
  const connect = vi.fn(
    async (): Promise<ClaudeConnectResult> => ({ ok: true, message: "The mnemo plugin is ready.", steps: [] }),
  );
  const transport = { status: readStatus, connect } as unknown as ClaudeCodeTransport;
  return { transport, status: readStatus, connect };
}

async function page(overrides: Partial<ClaudeCodeStatus> = {}) {
  const made = makeTransport(overrides);
  render(ClaudeCodePage, { props: { transport: made.transport } });
  await screen.findByRole("region", { name: "Claude Code" });
  return made;
}

function connectButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Connect Claude Code" }) as HTMLButtonElement;
}

describe("ClaudeCodePage", () => {
  it("shows the helper, CLI and plugin status", async () => {
    await page();
    expect(screen.getByText("Installed: C:\\Users\\me\\.mnemo\\bin\\mnemo2-mcp.exe")).toBeTruthy();
    expect(screen.getByText("Found: C:\\Users\\me\\AppData\\Local\\Programs\\claude\\claude.exe")).toBeTruthy();
    expect(screen.getByText("Not installed")).toBeTruthy();
    expect(connectButton().disabled).toBe(false);
  });

  it("disables Connect without the CLI and shows the manual path", async () => {
    await page({ cli_path: null });
    expect(connectButton().disabled).toBe(true);
    expect(screen.getByText(/Not found\./)).toBeTruthy();
    expect(screen.getByText("C:\\Users\\me\\.mnemo\\claude-plugin")).toBeTruthy();
    expect(screen.getByText(/claude plugin install mnemo@mnemo/)).toBeTruthy();
  });

  it("leaves a source-checkout marketplace alone", async () => {
    await page({ marketplace: "other", marketplace_path: "C:\\Mnemo" });
    expect(
      screen.getByText(/Claude Code uses the mnemo plugin from C:\\Mnemo \(a source checkout\)/),
    ).toBeTruthy();
    expect(connectButton().disabled).toBe(true);
  });

  it("connects, lists the steps and re-reads the status", async () => {
    const made = await page();
    made.connect.mockResolvedValueOnce({
      ok: true,
      message: "Claude Code now leads through the mnemo plugin.",
      steps: [
        {
          command: 'claude plugin marketplace add "C:\\Users\\me\\.mnemo\\claude-plugin"',
          ok: true,
          output: "Added marketplace mnemo",
        },
        { command: "claude plugin install mnemo@mnemo", ok: true, output: "Installed mnemo" },
      ],
    });

    await fireEvent.click(connectButton());

    await waitFor(() => expect(made.connect).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(made.status).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("status").textContent).toContain("Claude Code now leads through the mnemo plugin.");
    expect(screen.getByText('claude plugin marketplace add "C:\\Users\\me\\.mnemo\\claude-plugin"')).toBeTruthy();
    expect(screen.getByText("claude plugin install mnemo@mnemo")).toBeTruthy();
  });

  it("shows a failed connect as an alert", async () => {
    const made = await page();
    made.connect.mockResolvedValueOnce({
      ok: false,
      message: "The mnemo plugin install failed.",
      steps: [
        { command: "claude plugin install mnemo@mnemo", ok: false, output: "error: marketplace not found" },
      ],
    });

    await fireEvent.click(connectButton());

    await waitFor(() => expect(made.connect).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("alert").textContent).toContain("The mnemo plugin install failed.");
  });

  it("shows a status that could not be read", async () => {
    const made = makeTransport();
    made.status.mockRejectedValueOnce(new Error("claude_code_status: the shell is gone"));
    render(ClaudeCodePage, { props: { transport: made.transport } });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("claude_code_status: the shell is gone");
    expect(screen.queryByRole("button", { name: "Connect Claude Code" })).toBeNull();
  });
});

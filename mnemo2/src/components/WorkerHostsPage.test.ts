import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WORKER_HOSTS_REFRESH_MS, type WorkerHost, type WorkerHostsList, type WorkerHostsTransport } from "../lib/workerHosts";
import WorkerHostsPage from "./WorkerHostsPage.svelte";

function host(over: Partial<WorkerHost> = {}): WorkerHost {
  return {
    name: "studio",
    node_id: "n1",
    os: "windows",
    connected: true,
    slots: 4,
    busy: 1,
    catalog_ready: true,
    checks: "passed",
    last_seen_ms: Date.now(),
    ssh: null,
    claude_home: null,
    ...over,
  };
}

function listOf(hosts: WorkerHost[]): WorkerHostsList {
  return { hub: { listening: true, address: "127.0.0.1:7000" }, hosts };
}

function fake(hosts: WorkerHost[] = [host()]) {
  const transport = {
    list: vi.fn(async () => listOf(hosts)),
    pair: vi.fn(async () => ({
      code: "ABCD-1234",
      expires_in_s: 90,
      hub: { listening: true, address: "127.0.0.1:7000" },
      commands: ["mnemo2-node pair --code ABCD-1234", "mnemo2-node run"],
      note: "Run both commands.",
    })),
    revoke: vi.fn(async () => ({})),
    link: vi.fn(async () => ({})),
    unlink: vi.fn(async () => ({})),
  };
  return transport;
}

function show(transport: WorkerHostsTransport) {
  return render(WorkerHostsPage, { props: { transport } });
}

beforeEach(() => {
  vi.useRealTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("WorkerHostsPage", () => {
  it("lists hosts with OS, state, checks and busy/slots", async () => {
    show(fake([host({ claude_home: true })]));
    expect(await screen.findByText("studio")).toBeTruthy();
    expect(screen.getByText("Windows")).toBeTruthy();
    expect(screen.getByText("connected")).toBeTruthy();
    expect(screen.getByText("checks passed")).toBeTruthy();
    expect(screen.getByText("1/4 busy")).toBeTruthy();
    expect(screen.getByText("Claude Code set up")).toBeTruthy();
  });

  it("shows the empty list text", async () => {
    show(fake([]));
    expect(await screen.findByText("No worker hosts yet.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Get pairing code" })).toBeTruthy();
  });

  it("asks to open a project when there is no open lead", async () => {
    const transport = fake();
    transport.list.mockRejectedValue("Mnemo2 has no open lead; call lead_open first");
    show(transport);
    expect(await screen.findByText("Open a project to manage worker hosts.")).toBeTruthy();
  });

  it("links a host then refreshes", async () => {
    const transport = fake();
    show(transport);
    await screen.findByText("studio");
    const input = screen.getByLabelText("SSH connection name") as HTMLInputElement;
    const button = screen.getByRole("button", { name: "Link" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    await fireEvent.input(input, { target: { value: "studio-ssh" } });
    expect(button.disabled).toBe(false);
    const before = transport.list.mock.calls.length;
    await fireEvent.click(button);
    await waitFor(() => expect(transport.link).toHaveBeenCalledWith("studio", "studio-ssh"));
    await waitFor(() => expect(transport.list.mock.calls.length).toBeGreaterThan(before));
  });

  it("unlinks a linked host", async () => {
    const transport = fake([host({ ssh: "studio-ssh" })]);
    show(transport);
    expect(await screen.findByText("Linked to studio-ssh")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Unlink" }));
    await waitFor(() => expect(transport.unlink).toHaveBeenCalledWith("studio"));
  });

  it("revokes only after the second click", async () => {
    const transport = fake();
    show(transport);
    await screen.findByText("studio");
    await fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    expect(transport.revoke).not.toHaveBeenCalled();
    expect(screen.getByText(/Revoke studio\? Its running workers stop/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Confirm revoke" }));
    await waitFor(() => expect(transport.revoke).toHaveBeenCalledWith("studio"));
  });

  it("shows an action error inline", async () => {
    const transport = fake();
    transport.link.mockRejectedValue("no such connection");
    show(transport);
    await screen.findByText("studio");
    await fireEvent.input(screen.getByLabelText("SSH connection name"), { target: { value: "x" } });
    await fireEvent.click(screen.getByRole("button", { name: "Link" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("no such connection");
  });

  it("shows the pairing code, both commands and a countdown that expires", async () => {
    const transport = fake([]);
    show(transport);
    await screen.findByText("No worker hosts yet.");
    vi.useFakeTimers({ shouldAdvanceTime: false });
    await fireEvent.click(screen.getByRole("button", { name: "Get pairing code" }));
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByText("ABCD-1234")).toBeTruthy();
    expect(screen.getByText("mnemo2-node pair --code ABCD-1234")).toBeTruthy();
    expect(screen.getByText("mnemo2-node run")).toBeTruthy();
    expect(screen.getByText("01:30")).toBeTruthy();
    await vi.advanceTimersByTimeAsync(5000);
    expect(screen.getByText("01:25")).toBeTruthy();
    await vi.advanceTimersByTimeAsync(86_000);
    expect(screen.getByText("expired")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Get a new code" })).toBeTruthy();
  });

  it("does not overlap list calls", async () => {
    vi.useFakeTimers();
    const transport = fake();
    let resolve: (value: WorkerHostsList) => void = () => {};
    transport.list.mockImplementation(() => new Promise<WorkerHostsList>((r) => (resolve = r)));
    show(transport);
    await vi.advanceTimersByTimeAsync(WORKER_HOSTS_REFRESH_MS * 2);
    expect(transport.list).toHaveBeenCalledTimes(1);
    resolve(listOf([]));
    await vi.advanceTimersByTimeAsync(WORKER_HOSTS_REFRESH_MS);
    expect(transport.list).toHaveBeenCalledTimes(2);
  });
});

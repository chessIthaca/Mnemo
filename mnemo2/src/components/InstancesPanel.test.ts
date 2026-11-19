import { fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { INSTANCES_REFRESH_MS, type InstanceRow, type InstancesTransport } from "../lib/instances";
import InstancesPanel from "./InstancesPanel.svelte";

/** A self window, another live window, and one that stopped answering. */
const rows: InstanceRow[] = [
  {
    pid: 11,
    project: "C:\\Mnemo",
    version: "2.6.0",
    started_ms: 1,
    is_self: true,
    answered: true,
    workers_running: 2,
    workers_waiting: 1,
    cost_today_micros: 1_234_567,
  },
  {
    pid: 12,
    project: "C:\\FactoryGame",
    version: "2.6.0",
    started_ms: 2,
    is_self: false,
    answered: true,
    workers_running: 0,
    workers_waiting: 3,
    cost_today_micros: 0,
  },
  {
    pid: 13,
    project: "C:\\OldProject",
    version: "2.5.0",
    started_ms: 3,
    is_self: false,
    answered: false,
    workers_running: 0,
    workers_waiting: 0,
    cost_today_micros: 0,
  },
];

/** A transport that answers with `answer` (the rows above by default). */
function transportOf(
  answer: () => Promise<InstanceRow[]> = async () => rows,
): InstancesTransport & { list: ReturnType<typeof vi.fn>; show: ReturnType<typeof vi.fn> } {
  return {
    list: vi.fn(answer),
    show: vi.fn(async () => undefined),
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("InstancesPanel", () => {
  it("lists every instance with its project, workers and today's cost", async () => {
    render(InstancesPanel, { transport: transportOf() });

    expect(await screen.findByTitle("C:\\Mnemo")).toBeTruthy();
    expect(screen.getByTitle("C:\\FactoryGame")).toBeTruthy();
    expect(screen.getByTitle("C:\\OldProject")).toBeTruthy();

    // The window asking is marked as such.
    expect(screen.getByText("this window")).toBeTruthy();
    // Running and waiting workers, and the Cost-tab day figure.
    expect(screen.getByText("2 running")).toBeTruthy();
    expect(screen.getByText("1 waiting")).toBeTruthy();
    expect(screen.getByText("$1.23 today")).toBeTruthy();
    expect(screen.getByText("3 waiting")).toBeTruthy();
    // An instance that stopped answering keeps its project and says so.
    expect(screen.getByText("not answering")).toBeTruthy();
    expect(screen.getByText("OldProject")).toBeTruthy();
    expect(screen.getByText("v2.5.0")).toBeTruthy();
  });

  it("renders nothing when the backend lists no instance", async () => {
    render(InstancesPanel, { transport: transportOf(async () => []) });
    await Promise.resolve();
    expect(screen.queryByRole("region", { name: /instances/i })).toBeNull();
  });

  it("asks another instance to show its window", async () => {
    const transport = transportOf();
    render(InstancesPanel, { transport });

    await fireEvent.click(await screen.findByRole("button", { name: /FactoryGame/ }));

    expect(transport.show).toHaveBeenCalledWith(12);
  });

  it("does not make this window clickable", async () => {
    render(InstancesPanel, { transport: transportOf() });
    expect(await screen.findByText("this window")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Mnemo/ })).toBeNull();
  });

  it("shows an error when the window cannot be brought forward", async () => {
    const transport = transportOf();
    transport.show.mockRejectedValue(new Error("instance 12 is not running"));
    render(InstancesPanel, { transport });

    await fireEvent.click(await screen.findByRole("button", { name: /FactoryGame/ }));

    expect(transport.show).toHaveBeenCalledWith(12);
  });

  it("refreshes every 10 seconds while it is mounted", async () => {
    vi.useFakeTimers();
    const transport = transportOf();
    const { unmount } = render(InstancesPanel, { transport });

    await vi.advanceTimersByTimeAsync(0);
    expect(transport.list).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(INSTANCES_REFRESH_MS);
    expect(transport.list).toHaveBeenCalledTimes(2);

    unmount();
    await vi.advanceTimersByTimeAsync(INSTANCES_REFRESH_MS * 2);
    expect(transport.list).toHaveBeenCalledTimes(2);
  });
});

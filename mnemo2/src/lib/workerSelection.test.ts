import { describe, expect, it, vi } from "vitest";
import {
  createOpenWorkerTransport,
  createTauriOpenWorkerTransport,
  OPEN_WORKER_CHANNEL,
  openWorkerNotice,
  openWorkerOutcome,
  type OpenWorkerRequest,
} from "./workerSelection";

const request = (root: string, workerId: string): OpenWorkerRequest => ({
  root,
  worker_id: workerId,
});

describe("open-worker requests", () => {
  const project = { path: "C:/AgenticCoder/AgenticCoder", name: "Mnemo" };

  it("selects the named worker for the open project, whatever the path spelling", () => {
    const outcome = openWorkerOutcome(
      request("\\\\?\\c:\\agenticcoder\\agenticcoder\\", "kai"),
      [{ id: "kai" }, { id: "sol" }],
      project,
    );
    expect(outcome).toEqual({ kind: "select", workerId: "kai" });
  });

  it("reports a worker the project does not have", () => {
    expect(openWorkerOutcome(request("C:/AgenticCoder/AgenticCoder", "nope"), [{ id: "kai" }], project))
      .toEqual({ kind: "missing", workerId: "nope", projectName: "Mnemo" });
  });

  it("ignores a request that names another project", () => {
    expect(openWorkerOutcome(request("C:/Games/Garden", "kai"), [{ id: "kai" }], project)).toEqual({
      kind: "foreign",
      root: "C:/Games/Garden",
    });
  });

  it("reports a request that arrives with no project open", () => {
    expect(openWorkerOutcome(request("C:/AgenticCoder/AgenticCoder", "kai"), [], null)).toEqual({
      kind: "closed",
    });
  });

  it("explains every miss in a short notice", () => {
    expect(openWorkerNotice({ kind: "missing", workerId: "nope", projectName: "Mnemo" }))
      .toContain("worker nope");
    expect(openWorkerNotice({ kind: "missing", workerId: "nope", projectName: "Mnemo" }))
      .toContain("Mnemo");
    expect(openWorkerNotice({ kind: "foreign", root: "C:/Games/Garden" }))
      .toContain("C:/Games/Garden");
    expect(openWorkerNotice({ kind: "closed" })).toContain("no project is open");
  });
});

describe("open-worker transport", () => {
  it("listens on the native channel and forwards its payload", async () => {
    const unlisten = vi.fn();
    const listen = vi.fn(
      async (_channel: string, _handler: (event: { payload: unknown }) => void) => unlisten,
    );
    const transport = createTauriOpenWorkerTransport({ listen: listen as never });

    const shown = vi.fn();
    const stop = await transport.onOpenWorker(shown);
    expect(listen).toHaveBeenCalledWith(OPEN_WORKER_CHANNEL, expect.any(Function));

    listen.mock.calls[0][1]({ payload: { root: "C:/Mnemo", worker_id: "kai" } });
    expect(shown).toHaveBeenCalledWith({ root: "C:/Mnemo", worker_id: "kai" });

    stop();
    expect(unlisten).toHaveBeenCalled();
  });

  it("has no native transport in a browser-only session", () => {
    expect(createOpenWorkerTransport()).toBeNull();
  });
});
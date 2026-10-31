import { describe, expect, it, vi } from "vitest";
import {
  createLeadTransport,
  createTauriLeadTransport,
  LEAD_EVENT_CHANNEL,
  type LeadTransportBindings,
} from "./leadEngine";
import type { LeadEvent, LeadSnapshot } from "./types";

const snapshot: LeadSnapshot = {
  generation: 2,
  model: "test/scripted-a",
  model_id: "scripted-a",
  models: [
    {
      endpoint: "test",
      model: "scripted-a",
      spec: "test/scripted-a",
      is_default: true,
    },
  ],
  default_model: "test/scripted-a",
  transcript: { messages: [] },
  used_tokens: 10,
  max_tokens: 1000,
  pending: null,
  busy: false,
};

describe("native lead transport", () => {
  it("maps every command to its Tauri command and argument names", async () => {
    const invokeMock = vi.fn(async () => snapshot);
    const listenMock = vi.fn(
      async (
        _channel: string,
        _handler: (event: { payload: LeadEvent }) => void,
      ) => () => {},
    );
    const transport = createTauriLeadTransport({
      invoke: invokeMock as unknown as LeadTransportBindings["invoke"],
      listen: listenMock as unknown as LeadTransportBindings["listen"],
    });

    await transport.open({ workspaceRoot: "C:/project", model: "test/scripted-a" });
    await transport.snapshot();
    await transport.prompt("hello");
    await transport.interrupt();
    await transport.setModel("test/scripted-b");
    await transport.approve("req-1", "deny");
    await transport.answer("q-1", { kind: "choice", index: 1 });
    await transport.prompt("with an image", ["data:image/png;base64,one"]);
    await transport.close();

    expect(invokeMock).toHaveBeenNthCalledWith(1, "lead_open", {
      workspaceRoot: "C:/project",
      configDir: null,
      model: "test/scripted-a",
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "lead_snapshot");
    expect(invokeMock).toHaveBeenNthCalledWith(3, "lead_prompt", { text: "hello" });
    expect(invokeMock).toHaveBeenNthCalledWith(4, "lead_interrupt");
    expect(invokeMock).toHaveBeenNthCalledWith(5, "lead_set_model", {
      spec: "test/scripted-b",
    });
    expect(invokeMock).toHaveBeenNthCalledWith(6, "lead_approve", {
      requestId: "req-1",
      decision: "deny",
    });
    expect(invokeMock).toHaveBeenNthCalledWith(7, "lead_answer", {
      questionId: "q-1",
      answer: { kind: "choice", index: 1 },
    });
    expect(invokeMock).toHaveBeenNthCalledWith(9, "lead_close");
    expect(invokeMock).toHaveBeenCalledWith("lead_prompt", {
      text: "with an image",
      images: ["data:image/png;base64,one"],
    });
  });

  it("probes the Mnemo2 host handshake before native mode", async () => {
    const invokeMock = vi.fn(async () => "pong");
    const transport = createTauriLeadTransport({
      invoke: invokeMock as unknown as LeadTransportBindings["invoke"],
      listen: vi.fn() as unknown as LeadTransportBindings["listen"],
    });
    await expect(transport.probe()).resolves.toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("mnemo2_host_ping");
  });

  it("reports an unavailable host when the handshake fails", async () => {
    const invokeMock = vi.fn(async () => {
      throw new Error("command not found");
    });
    const transport = createTauriLeadTransport({
      invoke: invokeMock as unknown as LeadTransportBindings["invoke"],
      listen: vi.fn() as unknown as LeadTransportBindings["listen"],
    });
    await expect(transport.probe()).resolves.toBe(false);
  });

  it("subscribes to the one namespaced event channel and forwards payloads", async () => {
    const capture: {
      handler: ((event: { payload: LeadEvent }) => void) | null;
    } = { handler: null };
    const listenMock = vi.fn(
      async (
        _channel: string,
        handler: (event: { payload: LeadEvent }) => void,
      ) => {
        capture.handler = handler;
        return () => {};
      },
    );
    const transport = createTauriLeadTransport({
      invoke: vi.fn() as unknown as LeadTransportBindings["invoke"],
      listen: listenMock as unknown as LeadTransportBindings["listen"],
    });
    const seen: LeadEvent[] = [];
    await transport.onEvent((event) => seen.push(event));
    expect(listenMock).toHaveBeenCalledWith(LEAD_EVENT_CHANNEL, expect.any(Function));

    const event: LeadEvent = {
      generation: 4,
      event: { agent: 1, event: { kind: "text_delta", text: "hi" } },
    };
    capture.handler?.({ payload: event });
    expect(seen).toEqual([event]);
  });

  it("falls back to the browser mock when not running inside Tauri", () => {
    expect(createLeadTransport()).toBeNull();
  });
});

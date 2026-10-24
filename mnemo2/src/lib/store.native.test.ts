import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LeadEvent, LeadSnapshot } from "./types";
import type { LeadTransport } from "./leadEngine";

const mockState = vi.hoisted(() => ({
  transport: null as LeadTransport | null,
}));

vi.mock("./leadEngine", () => ({
  createLeadTransport: () => mockState.transport,
}));

import { actions, app } from "./store";

function snapshot(generation = 1): LeadSnapshot {
  return {
    generation,
    model: "test/scripted-a",
    model_id: "scripted-a",
    models: [
      {
        endpoint: "test",
        model: "scripted-a",
        spec: "test/scripted-a",
        is_default: true,
      },
      {
        endpoint: "test",
        model: "scripted-b",
        spec: "test/scripted-b",
        is_default: false,
      },
    ],
    default_model: "test/scripted-a",
    transcript: { messages: [] },
    used_tokens: 0,
    max_tokens: 1000,
    pending: null,
    busy: false,
  };
}

function makeTransport(initial: LeadSnapshot) {
  let current = initial;
  const capture: { handler: ((event: LeadEvent) => void) | null } = {
    handler: null,
  };
  const transport: LeadTransport = {
    probe: vi.fn(async () => true),
    open: vi.fn(async () => current),
    snapshot: vi.fn(async () => current),
    prompt: vi.fn(async () => {}),
    interrupt: vi.fn(async () => {}),
    setModel: vi.fn(async (spec: string) => {
      const modelId = spec.split("/").at(-1) ?? spec;
      current = {
        ...current,
        generation: current.generation + 1,
        model: spec,
        model_id: modelId,
      };
      return current;
    }),
    approve: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    onEvent: vi.fn(async (handler) => {
      capture.handler = handler;
      return () => {};
    }),
  };
  return {
    transport,
    emit: (event: LeadEvent) => capture.handler?.(event),
  };
}

describe("native lead store path", () => {
  beforeEach(() => {
    mockState.transport = null;
    actions.resetDemo();
  });

  it("opens, streams, interrupts, and changes models through the transport", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;

    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));
    expect(harness.transport.open).toHaveBeenCalledWith({
      workspaceRoot: "C:/AgenticCoder/AgenticCoder",
    });
    expect(get(app).leadRuntime.generation).toBe(1);

    actions.sendLeadMessage("hello");
    await vi.waitFor(() =>
      expect(harness.transport.prompt).toHaveBeenCalledWith("hello"),
    );
    expect(get(app).leadRuntime.busy).toBe(true);

    harness.emit({
      generation: 1,
      event: { agent: 1, event: { kind: "text_delta", text: "Hi" } },
    });
    expect(get(app).leadEntries.at(-1)).toMatchObject({
      kind: "message",
      author: "lead",
      body: "Hi",
    });

    actions.interruptLead();
    expect(harness.transport.interrupt).toHaveBeenCalled();
    harness.emit({
      generation: 1,
      event: { agent: 1, event: { kind: "finished" } },
    });
    expect(get(app).leadRuntime.busy).toBe(false);

    await new Promise((resolve) => setTimeout(resolve, 40));
    actions.setLeadModel("test/scripted-b");
    await vi.waitFor(() =>
      expect(harness.transport.setModel).toHaveBeenCalledWith("test/scripted-b"),
    );
    await vi.waitFor(() => expect(get(app).leadRuntime.modelId).toBe("scripted-b"));
    expect(get(app).leadRuntime.generation).toBe(2);
  });

  it("keeps the browser mock when no native transport exists", async () => {
    actions.openProject("mnemo");
    await Promise.resolve();
    expect(get(app).leadRuntime.mode).toBe("mock");
    const before = get(app).leadEntries.length;
    actions.sendLeadMessage("hello");
    expect(get(app).leadEntries.length).toBe(before + 2);
  });

  it("stays in mock mode when another Tauri host does not answer the handshake", async () => {
    const harness = makeTransport(snapshot());
    harness.transport.probe = vi.fn(async () => false);
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(harness.transport.probe).toHaveBeenCalled());
    expect(get(app).leadRuntime.mode).toBe("mock");
    expect(harness.transport.open).not.toHaveBeenCalled();
    expect(harness.transport.onEvent).not.toHaveBeenCalled();
  });
});

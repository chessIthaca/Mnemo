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
import { fixtureState } from "../test/fixtures";

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
    compact: vi.fn(async () => {}),
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
    app.set(fixtureState());
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
      expect(harness.transport.prompt).toHaveBeenCalledWith("hello", []),
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
      expect(harness.transport.setModel).toHaveBeenCalledWith("test/scripted-b", null),
    );
    await vi.waitFor(() => expect(get(app).leadRuntime.modelId).toBe("scripted-b"));
    expect(get(app).leadRuntime.generation).toBe(2);
  });

  it("forwards an image-only submission to the transport", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    const image = "data:image/png;base64,AAAA";
    actions.sendLeadMessage("", [image]);
    await vi.waitFor(() =>
      expect(harness.transport.prompt).toHaveBeenCalledWith("", [image]),
    );
    expect(get(app).leadEntries.at(-1)).toMatchObject({
      kind: "message",
      author: "user",
      body: "",
      images: [image],
    });
  });

  it("steers a busy lead with the submission's text and images", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    actions.sendLeadMessage("start the turn");
    await vi.waitFor(() =>
      expect(harness.transport.prompt).toHaveBeenCalledWith("start the turn", []),
    );
    expect(get(app).leadRuntime.busy).toBe(true);

    // Pin the turn clock so a steer's (in)decision to reset it is observable.
    app.update((state) => ({
      ...state,
      leadRuntime: { ...state.leadRuntime, turnStartedAtMs: 1_234 },
    }));
    const image = "data:image/jpeg;base64,BBBB";
    actions.sendLeadMessage("  adjust the plan  ", [image]);
    await vi.waitFor(() =>
      expect(harness.transport.prompt).toHaveBeenCalledWith("adjust the plan", [image]),
    );

    expect(harness.transport.prompt).toHaveBeenCalledTimes(2);
    expect(get(app).leadRuntime.busy).toBe(true);
    // A steer joins the running turn — the elapsed clock must not restart.
    expect(get(app).leadRuntime.turnStartedAtMs).toBe(1_234);
    expect(get(app).leadEntries.at(-1)).toMatchObject({
      kind: "message",
      author: "user",
      body: "adjust the plan",
      images: [image],
    });
  });

  it("keeps the running turn and the draft when a steer is rejected", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    await actions.sendLeadMessage("start the turn");
    expect(get(app).leadRuntime.busy).toBe(true);

    const image = "data:image/png;base64,one";
    app.update((state) => ({
      ...state,
      leadDraft: { text: "please steer", images: [image] },
      leadRuntime: {
        ...state.leadRuntime,
        turnStartedAtMs: 1_234,
        streamingEntryId: "lead-1",
      },
    }));
    harness.transport.prompt = vi.fn(async () => {
      throw new Error("steer rejected");
    });

    const accepted = await actions.sendLeadMessage("please steer", [image]);
    expect(accepted).toBe(false);
    // The engine may still be working: never fabricate idle after a rejected
    // steer, and keep the clock/streaming state it owns.
    expect(get(app).leadRuntime.busy).toBe(true);
    expect(get(app).leadRuntime.status).toBe("ready");
    expect(get(app).leadRuntime.turnStartedAtMs).toBe(1_234);
    expect(get(app).leadRuntime.streamingEntryId).toBe("lead-1");
    expect(get(app).toast?.message).toContain("steer rejected");
    // The caller owns the draft; the store never clears it.
    expect(get(app).leadDraft).toEqual({ text: "please steer", images: [image] });
    // The rejected steer is not left in the transcript as if delivered.
    expect(
      get(app).leadEntries.some(
        (entry) => entry.kind === "message" && entry.body === "please steer",
      ),
    ).toBe(false);
  });

  it("reports an idle prompt failure without losing the draft", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    const image = "data:image/png;base64,one";
    app.update((state) => ({
      ...state,
      leadDraft: { text: "hello", images: [image] },
    }));
    harness.transport.prompt = vi.fn(async () => {
      throw new Error("ipc offline");
    });

    const accepted = await actions.sendLeadMessage("hello", [image]);
    expect(accepted).toBe(false);
    expect(get(app).leadRuntime.busy).toBe(false);
    expect(get(app).leadRuntime.status).toBe("error");
    expect(get(app).leadRuntime.error).toContain("ipc offline");
    expect(get(app).toast?.message).toContain("ipc offline");
    expect(get(app).leadDraft).toEqual({ text: "hello", images: [image] });
    expect(
      get(app).leadEntries.some(
        (entry) => entry.kind === "message" && entry.body === "hello",
      ),
    ).toBe(false);
  });

  it("refuses a submission that races a pending question and keeps the draft", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    const image = "data:image/png;base64,one";
    app.update((state) => ({
      ...state,
      leadDraft: { text: "queued text", images: [image] },
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-1",
          question: "Which?",
          options: [],
        },
      },
    }));

    const accepted = await actions.sendLeadMessage("queued text", [image]);
    expect(accepted).toBe(false);
    expect(harness.transport.prompt).not.toHaveBeenCalled();
    expect(get(app).leadRuntime.busy).toBe(false);
    expect(get(app).leadDraft).toEqual({ text: "queued text", images: [image] });
    expect(get(app).toast?.message).toMatch(/answer the lead's question/i);
  });

  it("surfaces interruption failures instead of hiding them", async () => {
    const harness = makeTransport(snapshot());
    harness.transport.interrupt = vi.fn(async () => {
      throw new Error("stop failed");
    });
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));

    await expect(actions.interruptLead()).rejects.toThrow("stop failed");
    expect(get(app).toast?.message).toContain("stop failed");
  });

  it("compacts the lead context through the transport and toasts failures", async () => {
    const harness = makeTransport(snapshot());
    mockState.transport = harness.transport;
    actions.openProject("mnemo");
    await vi.waitFor(() => expect(get(app).leadRuntime.status).toBe("ready"));
    const toastBefore = get(app).toast?.message ?? null;

    await actions.compactLead();
    expect(harness.transport.compact).toHaveBeenCalledTimes(1);
    expect(get(app).toast?.message ?? null).toBe(toastBefore);

    harness.transport.compact = vi.fn(async () => {
      throw new Error("compact failed");
    });
    await actions.compactLead();
    expect(harness.transport.compact).toHaveBeenCalledTimes(1);
    expect(get(app).toast?.message).toContain("compact failed");
  });

  it("toasts instead of throwing when no native transport can compact", async () => {
    actions.openProject("mnemo");
    await Promise.resolve();
    expect(get(app).leadRuntime.mode).toBe("mock");
    await actions.compactLead();
    expect(get(app).toast?.message).toMatch(/native lead is not connected/i);
  });

  it("never fabricates a lead reply when no native transport exists", async () => {
    actions.openProject("mnemo");
    await Promise.resolve();
    expect(get(app).leadRuntime.mode).toBe("mock");
    const before = get(app).leadEntries.length;
    actions.sendLeadMessage("hello");
    // The user's message is kept; no simulated lead answer is invented.
    expect(get(app).leadEntries.length).toBe(before + 1);
    expect(get(app).leadEntries.at(-1)).toMatchObject({ author: "user", body: "hello" });
    expect(get(app).toast?.message).toMatch(/native shell/i);
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

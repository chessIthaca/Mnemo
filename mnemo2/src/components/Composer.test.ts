import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import { initialLeadRuntime } from "../lib/leadState";
import Composer from "./Composer.svelte";

const attach = vi.hoisted(() => ({ fileToAttachedDataUrl: vi.fn() }));

vi.mock("../../../frontend/src/lib/attachImages", () => ({
  fileToAttachedDataUrl: attach.fileToAttachedDataUrl,
}));

function setLead(busy: boolean, status: "ready" | "opening" = "ready") {
  app.update((state) => ({
    ...state,
    projectId: "mnemo",
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status,
      generation: 1,
      busy,
      model: "test/scripted-a",
      modelId: "scripted-a",
      models: [
        {
          endpoint: "test",
          model: "scripted-a",
          spec: "test/scripted-a",
          is_default: true,
        },
      ],
    },
  }));
}

const draft = () => get(app).leadDraft;
const textbox = () =>
  screen.getByRole("textbox", { name: "Message the lead" }) as HTMLTextAreaElement;

describe("Composer", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("keeps the textarea editable with steer and stop available while working", async () => {
    setLead(true);
    const stop = vi.spyOn(actions, "interruptLead").mockResolvedValue(undefined);
    render(Composer);

    expect(textbox().disabled).toBe(false);
    expect(screen.queryByRole("button", { name: "Send message" })).toBeNull();
    expect(screen.getByRole("button", { name: "Steer the lead" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    expect(stop).toHaveBeenCalled();
  });

  it("disables send until a native lead is ready", () => {
    setLead(false, "opening");
    render(Composer);
    const input = screen.getByRole("textbox", { name: "Message the lead" });
    expect(input).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Send message" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("keeps stop available while an interaction is pending", async () => {
    setLead(false);
    const stop = vi.spyOn(actions, "interruptLead").mockResolvedValue(undefined);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: { kind: "approval", request_id: "req-1" },
      },
    }));
    render(Composer);
    expect(screen.queryByRole("button", { name: "Send message" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    expect(stop).toHaveBeenCalled();
  });

  it("keeps Stopping visible until the engine confirms completion", async () => {
    setLead(true);
    let finish!: () => void;
    vi.spyOn(actions, "interruptLead").mockImplementation(
      () => new Promise<void>((resolve) => { finish = resolve; }),
    );
    render(Composer);
    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    const stopping = screen.getByRole("button", { name: "Stopping the lead" }) as HTMLButtonElement;
    expect(stopping.disabled).toBe(true);

    finish();
    await vi.waitFor(() => expect(stopping.disabled).toBe(true));
    app.update((state) => ({
      ...state,
      leadRuntime: { ...state.leadRuntime, busy: false },
    }));
    expect(await screen.findByRole("button", { name: "Send message" })).toBeTruthy();
  });

  it("steers a busy lead with the current text and images", async () => {
    setLead(true);
    const image = "data:image/png;base64,one";
    actions.setLeadDraftText("tighten the brief");
    actions.addLeadDraftImage(image);
    const send = vi.spyOn(actions, "sendLeadMessage");
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Steer the lead" }));
    expect(send).toHaveBeenCalledWith("tighten the brief", [image]);
  });

  it("sends an image-only draft", async () => {
    setLead(false);
    const image = "data:image/png;base64,one";
    actions.addLeadDraftImage(image);
    const send = vi.spyOn(actions, "sendLeadMessage");
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(send).toHaveBeenCalledWith("", [image]);
  });

  it("clears the draft after a steer", async () => {
    setLead(true);
    actions.setLeadDraftText("ship it");
    const send = vi.spyOn(actions, "sendLeadMessage");
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Steer the lead" }));
    expect(send).toHaveBeenCalledWith("ship it", []);
    expect(draft()).toEqual({ text: "", images: [] });
  });

  it("leaves the unsent draft untouched when stopping", async () => {
    setLead(true);
    const image = "data:image/png;base64,one";
    actions.setLeadDraftText("do not lose me");
    actions.addLeadDraftImage(image);
    vi.spyOn(actions, "interruptLead").mockResolvedValue(undefined);
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Stop the lead" }));
    expect(draft()).toEqual({ text: "do not lose me", images: [image] });
  });

  it("writes typing into the store draft so it survives unmounting", async () => {
    setLead(false);
    const { unmount } = render(Composer);
    await fireEvent.input(textbox(), { target: { value: "draft survives" } });
    expect(draft().text).toBe("draft survives");

    unmount();
    render(Composer);
    expect(textbox().value).toBe("draft survives");
  });

  it("pastes clipboard images through the shared attachment pipeline", async () => {
    setLead(false);
    attach.fileToAttachedDataUrl.mockResolvedValue("data:image/png;base64,downscaled");
    render(Composer);

    const file = new File(["binary"], "shot.png", { type: "image/png" });
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(paste, "clipboardData", {
      value: {
        items: [{ kind: "file", type: "image/png", getAsFile: () => file }],
      },
    });
    await fireEvent(textbox(), paste);

    expect(attach.fileToAttachedDataUrl).toHaveBeenCalledWith(file);
    await vi.waitFor(() =>
      expect(draft().images).toEqual(["data:image/png;base64,downscaled"]),
    );
    expect(
      (await screen.findByAltText("Attachment 1")).getAttribute("src"),
    ).toBe("data:image/png;base64,downscaled");
  });

  it("removes an attached image without touching the text", async () => {
    setLead(false);
    const image = "data:image/png;base64,one";
    actions.setLeadDraftText("keep text");
    actions.addLeadDraftImage(image);
    render(Composer);

    await fireEvent.click(
      screen.getByRole("button", { name: "Remove attachment 1" }),
    );
    expect(draft()).toEqual({ text: "keep text", images: [] });
    expect(screen.queryByAltText("Attachment 1")).toBeNull();
  });
});

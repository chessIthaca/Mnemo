import { fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import { initialLeadRuntime } from "../lib/leadState";
import Composer from "./Composer.svelte";

const attach = vi.hoisted(() => ({ fileToAttachedDataUrl: vi.fn() }));

vi.mock("../lib/attachImages", () => ({
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

  it("answers a pending question with a numeric choice", async () => {
    setLead(false);
    const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(true);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-1",
          question: "Which approach?",
          options: [
            { label: "Fast" },
            { label: "Thorough" },
            { label: "Ask again" },
          ],
        },
      },
    }));
    actions.setLeadDraftText("2)");
    render(Composer);

    await fireEvent.click(
      screen.getByRole("button", { name: "Answer question" }),
    );
    expect(answer).toHaveBeenCalledWith("q-1", { kind: "choice", index: 1 });
    expect(draft()).toEqual({ text: "", images: [] });
  });

  it("answers a pending question with freeform text", async () => {
    setLead(false);
    const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(true);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-2",
          question: "When should this land?",
          options: [{ label: "Now" }, { label: "Later" }],
        },
      },
    }));
    actions.setLeadDraftText("Run it on Friday instead");
    render(Composer);

    await fireEvent.click(
      screen.getByRole("button", { name: "Answer question" }),
    );
    expect(answer).toHaveBeenCalledWith("q-2", {
      kind: "freeform",
      text: "Run it on Friday instead",
    });
    expect(draft()).toEqual({ text: "", images: [] });
  });

  it("opens freeform mode instead of submitting the let's-talk number", async () => {
    setLead(false);
    const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(true);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-3",
          question: "Which approach?",
          options: [{ label: "Fast" }, { label: "Thorough" }, { label: "Ask again" }],
        },
      },
    }));
    actions.setLeadDraftText("4)");
    render(Composer);

    // `4)` is the "let's talk about it" sentinel (3 options + 1): it must open
    // freeform mode, never submit the sentinel as the answer text.
    await fireEvent.click(
      screen.getByRole("button", { name: "Answer question" }),
    );
    expect(answer).not.toHaveBeenCalled();
    expect(draft().text).toBe("");
    expect(screen.getByText(/freeform answer/i)).toBeTruthy();

    await fireEvent.input(textbox(), { target: { value: "Let's spike it" } });
    await fireEvent.click(
      screen.getByRole("button", { name: "Answer question" }),
    );
    expect(answer).toHaveBeenCalledWith("q-3", {
      kind: "freeform",
      text: "Let's spike it",
    });
  });

  it("keeps the draft when the store rejects a send", async () => {
    setLead(false);
    const image = "data:image/png;base64,keep";
    actions.setLeadDraftText("keep me");
    actions.addLeadDraftImage(image);
    const send = vi.spyOn(actions, "sendLeadMessage").mockResolvedValue(false);
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await vi.waitFor(() =>
      expect(send).toHaveBeenCalledWith("keep me", [image]),
    );
    expect(draft()).toEqual({ text: "keep me", images: [image] });
  });

  it("clears the draft only after the store confirms acceptance", async () => {
    setLead(false);
    actions.setLeadDraftText("send me");
    let accept!: (value: boolean) => void;
    vi.spyOn(actions, "sendLeadMessage").mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          accept = resolve;
        }),
    );
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(draft().text).toBe("send me");

    accept(true);
    await vi.waitFor(() => expect(draft().text).toBe(""));
  });

  it("toasts rejected pasted image conversions", async () => {
    setLead(false);
    attach.fileToAttachedDataUrl.mockRejectedValue(new Error("decode failed"));
    const toast = vi.spyOn(actions, "showToast");
    render(Composer);

    const file = new File(["binary"], "broken.png", { type: "image/png" });
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(paste, "clipboardData", {
      value: {
        items: [{ kind: "file", type: "image/png", getAsFile: () => file }],
      },
    });
    await fireEvent(textbox(), paste);

    await vi.waitFor(() =>
      expect(toast).toHaveBeenCalledWith("Could not attach the pasted image.", "error"),
    );

    expect(draft().images).toEqual([]);
  });

  it("keeps the typed answer when the host rejects it", async () => {
    setLead(false);
    const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(false);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-4",
          question: "Which approach?",
          options: [{ label: "Fast" }, { label: "Thorough" }],
        },
      },
    }));
    actions.setLeadDraftText("2)");
    render(Composer);

    await fireEvent.click(
      screen.getByRole("button", { name: "Answer question" }),
    );
    await vi.waitFor(() =>
      expect(answer).toHaveBeenCalledWith("q-4", { kind: "choice", index: 1 }),
    );
    // The question is still pending and the rejected answer must survive.
    expect(draft().text).toBe("2)");
  });

  it("preserves attached images after a text-only answer", async () => {
    setLead(false);
    const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(true);
    const image = "data:image/png;base64,kept";
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-5",
          question: "Which approach?",
          options: [{ label: "Fast" }, { label: "Thorough" }],
        },
      },
    }));
    actions.setLeadDraftText("2)");
    actions.addLeadDraftImage(image);
    render(Composer);

    await fireEvent.click(
      screen.getByRole("button", { name: "Answer question" }),
    );
    await vi.waitFor(() =>
      expect(answer).toHaveBeenCalledWith("q-5", { kind: "choice", index: 1 }),
    );
    // An answer is text-only: the command clears and the attachment stays.
    await vi.waitFor(() =>
      expect(draft()).toEqual({ text: "", images: [image] }),
    );
  });

  it("submits a pending-question answer only once on a double click", async () => {
    setLead(false);
    let accept!: (value: boolean) => void;
    const answer = vi.spyOn(actions, "answerLead").mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          accept = resolve;
        }),
    );
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: {
          kind: "question",
          question_id: "q-6",
          question: "Which approach?",
          options: [{ label: "Fast" }, { label: "Thorough" }],
        },
      },
    }));
    actions.setLeadDraftText("2)");
    render(Composer);

    const button = screen.getByRole("button", { name: "Answer question" });
    await fireEvent.click(button);
    await fireEvent.click(button);
    expect(answer).toHaveBeenCalledTimes(1);

    accept(true);
    await vi.waitFor(() => expect(draft().text).toBe(""));
  });

  it("keeps pending approvals on their own buttons, not the composer", () => {
    setLead(false);
    app.update((state) => ({
      ...state,
      leadRuntime: {
        ...state.leadRuntime,
        pending: { kind: "approval", request_id: "req-1" },
      },
    }));
    render(Composer);
    expect(screen.queryByRole("button", { name: "Answer question" })).toBeNull();
    expect(screen.getByRole("button", { name: "Stop the lead" })).toBeTruthy();
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
    const send = vi.spyOn(actions, "sendLeadMessage").mockResolvedValue(true);
    render(Composer);

    await fireEvent.click(screen.getByRole("button", { name: "Steer the lead" }));
    expect(send).toHaveBeenCalledWith("ship it", []);
    await vi.waitFor(() => expect(draft()).toEqual({ text: "", images: [] }));
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

  it("a real pasted PNG renders its thumbnail before sending", async () => {
    setLead(false);
    // No converter mock for this test: wire the REAL shared pipeline into the
    // module the component imports, so the paste exercises the true
    // clipboard File -> FileReader -> data URL -> store -> <img> path.
    const actual =
      await vi.importActual<
        typeof import("../lib/attachImages")
      >("../lib/attachImages");
    attach.fileToAttachedDataUrl.mockImplementation(actual.fileToAttachedDataUrl);
    render(Composer);

    // A genuine 1x1 transparent PNG (68 bytes, zlib-verified), not mock bytes.
    const pngBase64 =
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=";
    const file = new File(
      [Uint8Array.from(atob(pngBase64), (c) => c.charCodeAt(0))],
      "shot.png",
      { type: "image/png" },
    );
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(paste, "clipboardData", {
      value: {
        items: [{ kind: "file", type: "image/png", getAsFile: () => file }],
      },
    });
    await fireEvent(textbox(), paste);

    // The real converter must have read the same bytes the file carries.
    const expected = `data:image/png;base64,${pngBase64}`;
    await vi.waitFor(() => expect(draft().images).toEqual([expected]));
    expect(
      (await screen.findByAltText("Attachment 1")).getAttribute("src"),
    ).toBe(expected);

    // Ordinary text pastes stay untouched: no image is attached and the
    // default paste behavior (no preventDefault) is preserved.
    const textPaste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(textPaste, "clipboardData", {
      value: {
        items: [{ kind: "string", type: "text/plain", getAsFile: () => null }],
      },
    });
    await fireEvent(textbox(), textPaste);
    expect(textPaste.defaultPrevented).toBe(false);
    expect(draft().images).toEqual([expected]);
  });

  async function explorerCopiedPngRendersThumbnailBeforeSending() {
    setLead(false);
    // Real converter, same as the direct paste regression above: the paste
    // must run the true clipboard File -> FileReader -> data URL -> store
    // -> <img> path, not a mocked conversion.
    const actual =
      await vi.importActual<
        typeof import("../lib/attachImages")
      >("../lib/attachImages");
    attach.fileToAttachedDataUrl.mockImplementation(actual.fileToAttachedDataUrl);
    render(Composer);

    // A genuine 1x1 transparent PNG (68 bytes, zlib-verified), not mock bytes.
    const pngBase64 =
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=";
    const file = new File(
      [Uint8Array.from(atob(pngBase64), (c) => c.charCodeAt(0))],
      "shot.png",
      { type: "image/png" },
    );
    // What an Explorer-copied image FILE pastes like: the file-kind item
    // carries an EMPTY type (never image/*), while the actual image File
    // only surfaces in clipboardData.files.
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(paste, "clipboardData", {
      value: {
        items: [{ kind: "file", type: "", getAsFile: () => file }],
        files: [file],
      },
    });
    await fireEvent(textbox(), paste);

    const expected = `data:image/png;base64,${pngBase64}`;
    await vi.waitFor(() => expect(draft().images).toEqual([expected]));
    expect(
      (await screen.findByAltText("Attachment 1")).getAttribute("src"),
    ).toBe(expected);
  }

  it(
    "an Explorer-copied PNG file renders its thumbnail before sending",
    explorerCopiedPngRendersThumbnailBeforeSending,
  );

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

  describe("with Claude Code leading the project", () => {
    const attachExternalLead = () =>
      app.update((state) => ({
        ...state,
        externalLead: { attached: true, root: "C:/Mnemo", sinceMs: 1_000 },
      }));
    const confirmation = () => screen.queryByRole("dialog", { name: "Send to the built-in lead?" });

    it("asks before sending to the built-in lead and keeps the draft on cancel", async () => {
      setLead(false);
      attachExternalLead();
      actions.setLeadDraftText("redirect the team");
      const send = vi.spyOn(actions, "sendLeadMessage").mockResolvedValue(true);
      render(Composer);

      await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
      expect(confirmation()).toBeTruthy();
      expect(
        screen.getByText("Claude Code is leading this project. Send this to the built-in lead anyway?"),
      ).toBeTruthy();
      expect(send).not.toHaveBeenCalled();

      await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(confirmation()).toBeNull();
      expect(send).not.toHaveBeenCalled();
      expect(draft().text).toBe("redirect the team");
    });

    it("sends to the built-in lead once confirmed", async () => {
      setLead(false);
      attachExternalLead();
      actions.setLeadDraftText("redirect the team");
      const send = vi.spyOn(actions, "sendLeadMessage").mockResolvedValue(true);
      render(Composer);

      await fireEvent.keyDown(textbox(), { key: "Enter" });
      expect(confirmation()).toBeTruthy();
      expect(send).not.toHaveBeenCalled();

      await fireEvent.click(screen.getByRole("button", { name: "Send anyway" }));
      await vi.waitFor(() => expect(send).toHaveBeenCalledWith("redirect the team", []));
      expect(send).toHaveBeenCalledTimes(1);
      expect(confirmation()).toBeNull();
      await vi.waitFor(() => expect(draft().text).toBe(""));
    });

    it("drops the confirmation when Claude Code detaches", async () => {
      setLead(false);
      attachExternalLead();
      actions.setLeadDraftText("redirect the team");
      render(Composer);

      await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
      expect(confirmation()).toBeTruthy();

      app.update((state) => ({
        ...state,
        externalLead: { attached: false, root: null, sinceMs: null },
      }));
      await vi.waitFor(() => expect(confirmation()).toBeNull());
      expect(draft().text).toBe("redirect the team");
    });

    it("never turns a confirmed prompt into an answer to a question that arrived meanwhile", async () => {
      setLead(false);
      attachExternalLead();
      actions.setLeadDraftText("redirect the team");
      const send = vi.spyOn(actions, "sendLeadMessage").mockResolvedValue(true);
      const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(true);
      render(Composer);

      await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
      expect(confirmation()).toBeTruthy();
      app.update((state) => ({
        ...state,
        leadRuntime: {
          ...state.leadRuntime,
          pending: {
            kind: "question",
            question_id: "q-late",
            question: "Which approach?",
            options: [{ label: "Fast" }, { label: "Thorough" }],
          },
        },
      }));

      // The question now owns the turn, so the prompt waits; the draft is
      // neither sent as the answer nor lost.
      await fireEvent.click(screen.getByRole("button", { name: "Send anyway" }));
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(answer).not.toHaveBeenCalled();
      expect(send).not.toHaveBeenCalled();
      expect(draft().text).toBe("redirect the team");
    });

    it("does not ask when answering a question the built-in lead asked", async () => {
      setLead(false);
      attachExternalLead();
      const answer = vi.spyOn(actions, "answerLead").mockResolvedValue(true);
      app.update((state) => ({
        ...state,
        leadRuntime: {
          ...state.leadRuntime,
          pending: {
            kind: "question",
            question_id: "q-ext",
            question: "Which approach?",
            options: [{ label: "Fast" }, { label: "Thorough" }],
          },
        },
      }));
      actions.setLeadDraftText("2)");
      render(Composer);

      await fireEvent.click(screen.getByRole("button", { name: "Answer question" }));
      expect(confirmation()).toBeNull();
      await vi.waitFor(() =>
        expect(answer).toHaveBeenCalledWith("q-ext", { kind: "choice", index: 1 }),
      );
    });

    it("sends without asking when no external lead is attached", async () => {
      setLead(false);
      actions.setLeadDraftText("plan the work");
      const send = vi.spyOn(actions, "sendLeadMessage").mockResolvedValue(true);
      render(Composer);

      await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
      await vi.waitFor(() => expect(send).toHaveBeenCalledWith("plan the work", []));
      expect(confirmation()).toBeNull();
    });
  });
});

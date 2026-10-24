import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { actions, app } from "../lib/store";
import { initialLeadRuntime } from "../lib/leadState";
import LeadChat from "./LeadChat.svelte";

function setRuntime(patch: Partial<ReturnType<typeof initialLeadRuntime>>) {
  app.update((state) => ({
    ...state,
    leadRuntime: {
      ...initialLeadRuntime(),
      mode: "native",
      status: "ready",
      generation: 1,
      ...patch,
    },
  }));
}

describe("LeadChat", () => {
  beforeEach(() => {
    actions.resetDemo();
    actions.openProject("mnemo");
  });

  it("renders no global run strip", () => {
    render(LeadChat);
    expect(screen.queryByText(/Run 04/)).toBeNull();
  });

  it("shows the configured lead model", () => {
    render(LeadChat);
    expect(screen.getAllByText("claude-opus-5-5").length).toBeGreaterThan(0);
    expect(screen.queryByText("ATLAS CORE")).toBeNull();
  });

  it("sends a message from the composer into the lead conversation", async () => {
    render(LeadChat);
    const input = screen.getByRole("textbox", { name: "Message the lead" });
    await fireEvent.input(input, { target: { value: "Keep the review flow explicit." } });
    await fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByText("Keep the review flow explicit.")).toBeTruthy();
    expect((input as HTMLTextAreaElement).value).toBe("");
  });

  it("shows an inline error with retry when the native lead fails", () => {
    setRuntime({ status: "error", error: "Engine refused the request." });
    render(LeadChat);
    expect(screen.getByRole("alert").textContent).toContain("Engine refused the request.");
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
  });

  it("shows approval and question interactions inline", () => {
    setRuntime({
      pending: {
        kind: "approval",
        request_id: "req-1",
        tool_name: "write_file",
      },
    });
    render(LeadChat);
    expect(screen.getByText("APPROVAL REQUIRED")).toBeTruthy();
    const approve = screen.getByRole("button", { name: "Approve" }) as HTMLButtonElement;
    expect(approve.disabled).toBe(false);
    expect(screen.getByRole("button", { name: "Deny" })).toBeTruthy();
  });

  it("shows question options inline", () => {
    setRuntime({
      pending: {
        kind: "question",
        question_id: "q-1",
        question: "Which branch should I target?",
        options: [{ label: "main" }, { label: "wt/mnemo" }],
      },
    });
    render(LeadChat);
    expect(screen.getByText("Which branch should I target?")).toBeTruthy();
    const main = screen.getByRole("button", { name: "main" }) as HTMLButtonElement;
    expect(main.disabled).toBe(false);
    expect(screen.getByRole("button", { name: "wt/mnemo" })).toBeTruthy();
  });
});

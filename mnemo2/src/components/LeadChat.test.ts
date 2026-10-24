import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { actions } from "../lib/store";
import LeadChat from "./LeadChat.svelte";

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
});

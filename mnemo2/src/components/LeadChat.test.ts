import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { actions } from "../lib/store";
import LeadChat from "./LeadChat.svelte";

describe("LeadChat", () => {
  beforeEach(() => {
    actions.resetDemo();
    actions.openProject("mnemo");
  });

  it("expands the connected run summary into readable steps", async () => {
    render(LeadChat);
    await fireEvent.click(screen.getByRole("button", { name: /Run 04/ }));
    expect(await screen.findByText("Ana has 2 open findings")).toBeTruthy();
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

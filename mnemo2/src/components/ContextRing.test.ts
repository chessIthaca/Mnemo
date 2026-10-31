import { fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyLeadEvent } from "../lib/leadState";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import type { LeadEvent, LeadRuntimeInfo } from "../lib/types";
import ContextRing from "./ContextRing.svelte";

/** Put a live native lead session into the shared store. */
function openLeadSession(patch: Partial<LeadRuntimeInfo> = {}): void {
  app.update((state) => ({
    ...state,
    leadRuntime: {
      ...state.leadRuntime,
      mode: "native",
      status: "ready",
      generation: 3,
      ...patch,
    },
  }));
}

/** Feed a live lead event through the same reducer the store's listener uses. */
function emitLeadEvent(event: LeadEvent): void {
  app.update((state) => applyLeadEvent(state, event));
}

const ringButton = () => screen.getByRole("button", { name: /Lead context/ });
const ringLevel = () => document.querySelector(".context-chip")?.className ?? "";
const tip = () => document.querySelector(".context-tip") as HTMLElement;
const compactButton = () =>
  screen.getByRole("button", { name: /Compact now|Compacting…/ }) as HTMLButtonElement;

describe("TopBar lead context ring", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders nothing without a native lead session", () => {
    const { container } = render(ContextRing);
    expect(container.querySelector(".context-chip")).toBeNull();
  });

  it("shows the ring's fill percent and token totals in its accessible label", () => {
    openLeadSession({ usedTokens: 50_000, maxTokens: 200_000 });
    render(ContextRing);

    expect(ringButton().getAttribute("aria-label")).toBe(
      "Lead context 25% used (50,000 of 200,000 tokens)",
    );
    expect(ringButton().textContent).toContain("25%");
    expect(ringLevel()).toContain("level-ok");
  });

  it("updates the ring from a live context_usage event without a snapshot reload", async () => {
    openLeadSession({ usedTokens: 50_000, maxTokens: 200_000 });
    render(ContextRing);

    emitLeadEvent({
      generation: 3,
      event: {
        agent: 1,
        event: {
          kind: "context_usage",
          used: 150_000,
          max: 200_000,
          breakdown: { system: 20_000, user: 5_000, assistant: 25_000, tool: 100_000 },
        },
      },
    });
    await tick();

    expect(ringButton().getAttribute("aria-label")).toBe(
      "Lead context 75% used (150,000 of 200,000 tokens)",
    );
    expect(ringLevel()).toContain("level-warn");

    // The pinned popup lists all four per-role breakdown values.
    await fireEvent.click(ringButton());
    for (const text of ["System", "20,000", "User", "5,000", "Assistant", "25,000", "Tool", "100,000"]) {
      expect(tip().textContent).toContain(text);
    }
    expect(tip().querySelectorAll(".role-bar")).toHaveLength(4);
  });

  it("opens on hover and focus like the usage chip", async () => {
    openLeadSession({ usedTokens: 50_000, maxTokens: 200_000 });
    render(ContextRing);

    await fireEvent.mouseEnter(ringButton());
    expect(tip().classList).toContain("open");
    await fireEvent.mouseLeave(ringButton());
    expect(tip().classList).not.toContain("open");

    await fireEvent.focus(ringButton());
    expect(tip().classList).toContain("open");
    await fireEvent.blur(ringButton());
    expect(tip().classList).not.toContain("open");
  });

  it("pins the popup on click so its Compact now button sends the compact command", async () => {
    const compactLead = vi.spyOn(actions, "compactLead");
    openLeadSession({ usedTokens: 150_000, maxTokens: 200_000 });
    render(ContextRing);

    await fireEvent.click(ringButton());
    expect(tip().classList).toContain("open");
    expect(ringButton().getAttribute("aria-expanded")).toBe("true");

    // Moving the pointer away does not close a pinned popup, so its button
    // stays reachable.
    await fireEvent.mouseLeave(ringButton());
    expect(tip().classList).toContain("open");

    await fireEvent.click(screen.getByRole("button", { name: "Compact now" }));
    expect(compactLead).toHaveBeenCalledTimes(1);
  });

  it("disables the compact button while the lead is busy or compacting", async () => {
    openLeadSession({ usedTokens: 150_000, maxTokens: 200_000 });
    render(ContextRing);
    await fireEvent.click(ringButton());

    expect(compactButton().disabled).toBe(false);

    app.update((state) => ({ ...state, leadRuntime: { ...state.leadRuntime, busy: true } }));
    await tick();
    expect(compactButton().disabled).toBe(true);

    app.update((state) => ({
      ...state,
      leadRuntime: { ...state.leadRuntime, busy: false, compacting: true },
    }));
    await tick();
    expect(compactButton().disabled).toBe(true);
    expect(compactButton().textContent).toContain("Compacting…");
  });

  it("shows Compacting… for compact_started and applies the compacted outcome", async () => {
    openLeadSession({ usedTokens: 150_000, maxTokens: 200_000 });
    render(ContextRing);
    await fireEvent.click(ringButton());

    emitLeadEvent({ generation: 3, event: { agent: 1, event: { kind: "compact_started" } } });
    await tick();
    expect(compactButton().disabled).toBe(true);
    expect(compactButton().textContent).toContain("Compacting…");

    emitLeadEvent({
      generation: 3,
      event: { agent: 1, event: { kind: "compacted", before: 150_000, after: 30_000 } },
    });
    await tick();
    expect(ringButton().getAttribute("aria-label")).toBe(
      "Lead context 15% used (30,000 of 200,000 tokens)",
    );
    expect(ringLevel()).toContain("level-ok");
    expect(compactButton().disabled).toBe(false);
    expect(compactButton().textContent).toContain("Compact now");
  });

  it("keeps the compact button unpressable while the session is opening", async () => {
    openLeadSession({ usedTokens: 1_000, maxTokens: 2_000, status: "opening" });
    render(ContextRing);

    // The ring renders during opening (the transport exists), but a disabled
    // button never dispatches click events, so compaction cannot start.
    await fireEvent.click(ringButton());
    expect(compactButton().disabled).toBe(true);
  });

  it("disables the compact button live when a ready session reopens", async () => {
    openLeadSession({ usedTokens: 1_000, maxTokens: 2_000 });
    render(ContextRing);
    await fireEvent.click(ringButton());
    expect(compactButton().disabled).toBe(false);

    app.update((state) => ({
      ...state,
      leadRuntime: { ...state.leadRuntime, status: "opening" },
    }));
    await tick();
    expect(compactButton().disabled).toBe(true);

    // Status "error" is ambiguous: a failed open has no session, but a failed
    // turn leaves a live session that may still compact. The UI cannot tell
    // them apart, so it stays available and the host gate rejects a
    // sessionless compact with "the lead session is not open" (pinned by the
    // Rust test lead_compact_summarizes_history_and_reports_the_outcome).
    app.update((state) => ({
      ...state,
      leadRuntime: { ...state.leadRuntime, status: "error" },
    }));
    await tick();
    expect(compactButton().disabled).toBe(false);
  });

  it("closes a pinned popup with Escape or an outside click", async () => {
    openLeadSession({ usedTokens: 1_000, maxTokens: 2_000 });
    render(ContextRing);

    await fireEvent.click(ringButton());
    expect(tip().classList).toContain("open");

    await fireEvent.keyDown(window, { key: "Escape" });
    expect(tip().classList).not.toContain("open");
    expect(ringButton().getAttribute("aria-expanded")).toBe("false");

    await fireEvent.click(ringButton());
    await new Promise((resolve) => setTimeout(resolve, 0));
    await fireEvent.pointerDown(document.body);
    expect(tip().classList).not.toContain("open");
    expect(ringButton().getAttribute("aria-expanded")).toBe("false");
  });

  it("stays neutral while the window size is unknown", async () => {
    openLeadSession({ usedTokens: 0, maxTokens: 0 });
    render(ContextRing);

    expect(ringButton().getAttribute("aria-label")).toBe(
      "Lead context usage unknown (window size unknown)",
    );
    expect(ringButton().textContent).toContain("–");
    expect(ringLevel()).toContain("level-unknown");

    await fireEvent.click(ringButton());
    expect(tip().textContent).toContain("The context window size is unknown for this session yet.");
  });
});
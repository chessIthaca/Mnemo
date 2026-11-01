import { render } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { app } from "../lib/store";
import type { InboxEntry, Worker } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerInbox from "./WorkerInbox.svelte";

/**
 * One entry per sender, oldest first, spanning all four delivery forms —
 * the texts are the delivered bodies, headers included.
 */
const inbox = (): InboxEntry[] => [
  {
    sender: "external_lead",
    kind: "prompt",
    text: "[From the lead (Claude Code)] Ship the **received-messages ledger**.",
    atMs: 1_787_400_000_000,
  },
  {
    sender: "lead",
    kind: "steer",
    text: "[From the built-in lead] Keep the section collapsed by default.",
    atMs: 1_787_400_060_000,
  },
  {
    sender: "user",
    kind: "brief",
    text: "[From the user in mission control] List who sent what and when.",
    atMs: 1_787_400_120_000,
  },
  {
    sender: "mnemo",
    kind: "fix",
    text: "[From Mnemo2] Fix the reported finding on the reviewed tip.",
    atMs: 1_787_400_180_000,
  },
];

const worker = (entries: InboxEntry[] = inbox()): Worker => ({ ...createWorkers()[0], inbox: entries });

describe("WorkerInbox", () => {
  beforeEach(() => app.set(fixtureState()));

  it("lists every sender's message with its label, oldest first, and tags only steer/fix", () => {
    const { container } = render(WorkerInbox, { worker: worker() });

    expect(container.querySelector(".inbox-heading")?.textContent).toContain("RECEIVED MESSAGES · 4");
    expect(
      [...container.querySelectorAll(".inbox-meta strong")].map((strong) => strong.textContent),
    ).toEqual(["Claude Code", "Built-in lead", "You", "Mnemo2"]);
    const tags = [...container.querySelectorAll(".kind-tag")];
    expect(tags.map((tag) => tag.textContent)).toEqual(["steer", "fix"]);
    expect(tags.map((tag) => tag.getAttribute("data-kind"))).toEqual(["steer", "fix"]);
    // Prompt and brief are the norm: their entries carry no kind tag.
    expect(container.querySelectorAll(".inbox-entry")).toHaveLength(4);
    expect(container.querySelectorAll(".inbox-meta time")).toHaveLength(4);
  });

  it("renders each delivered text as Markdown inside the ledger", () => {
    const { container } = render(WorkerInbox, { worker: worker() });

    const bodies = [...container.querySelectorAll(".inbox-body .markdown")];
    // markdown-it closes with a trailing newline; compare the trimmed text.
    const texts = bodies.map((body) => body.textContent?.trim());
    expect(bodies).toHaveLength(4);
    expect(bodies[0].querySelector("strong")?.textContent).toBe("received-messages ledger");
    expect(texts).toEqual([
      "[From the lead (Claude Code)] Ship the received-messages ledger.",
      "[From the built-in lead] Keep the section collapsed by default.",
      "[From the user in mission control] List who sent what and when.",
      "[From Mnemo2] Fix the reported finding on the reviewed tip.",
    ]);
  });

  it("stays hidden while the worker has received nothing", () => {
    const { container } = render(WorkerInbox, { worker: worker([]) });

    expect(container.querySelector(".inbox-panel")).toBeNull();
  });
});
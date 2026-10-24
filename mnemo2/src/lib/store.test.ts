import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import { actions, app } from "./store";

const state = () => get(app);
const worker = (id: string) => state().workers.find((item) => item.id === id)!;

describe("Mnemo2 mock workflow", () => {
  beforeEach(() => {
    actions.resetDemo();
    actions.openProject("mnemo");
  });

  it("starts with ten workers and configurable project worker slots", () => {
    expect(state().workers).toHaveLength(10);
    expect(state().workers.some((item) => item.name === "Lead")).toBe(false);

    actions.setWorkerSlots(12);
    expect(state().projects.find((project) => project.id === "mnemo")?.workerSlots).toBe(12);

    actions.openWorkbench("diff");
    actions.openTeam();
    expect(state().route).toBe("team");
    expect(state().workbenchOpen).toBe(false);
  });

  it("adds a user message and a concise lead response", () => {
    const count = state().leadEntries.length;
    actions.sendLeadMessage("Keep the review flow quiet and explicit.");
    const entries = state().leadEntries;
    expect(entries).toHaveLength(count + 2);
    expect(entries.at(-2)).toMatchObject({ kind: "message", author: "user" });
    expect(entries.at(-1)).toMatchObject({ kind: "message", author: "lead" });
    expect(state().activityExpanded).toBe(true);
  });

  it("blocks merge while Ana has an open finding", () => {
    const count = state().leadEntries.length;
    actions.mergeWorker("ana");
    expect(worker("ana").status).toBe("in_review");
    expect(worker("ana").merge).toBeUndefined();
    expect(state().leadEntries).toHaveLength(count);
    expect(state().toast?.message).toContain("open");
  });

  it("walks tiny fix, fixer delta, re-review, and merge record", () => {
    actions.fixFinding("ana", "ana-copy");
    expect(worker("ana").findings.find((finding) => finding.id === "ana-copy")?.status).toBe("fixed");

    actions.spawnFixer("ana", "ana-merge");
    expect(worker("ana").status).toBe("fixing");
    actions.mergeWorker("ana");
    expect(worker("ana").merge).toBeUndefined();

    actions.completeFixer("ana", "ana-merge");
    expect(worker("ana").status).toBe("in_review");
    expect(worker("ana").checks[0].status).toBe("passed");

    actions.mergeWorker("ana");
    expect(worker("ana").status).toBe("merged");
    expect(worker("ana").merge?.reviewSummary).toBe("2 findings · 2 fixed · 0 wontfix");
    expect(state().leadEntries.at(-1)).toMatchObject({ kind: "merge", workerId: "ana" });
  });

  it("requires justification for wontfix but then permits merge", () => {
    actions.fixFinding("ana", "ana-copy");
    actions.wontfixFinding("ana", "ana-merge", "   ");
    expect(worker("ana").findings.find((finding) => finding.id === "ana-merge")?.status).toBe("open");

    actions.wontfixFinding("ana", "ana-merge", "Tracked separately; this mock only demonstrates the guard.");
    actions.mergeWorker("ana");
    expect(worker("ana").status).toBe("merged");
    expect(worker("ana").merge?.reviewSummary).toContain("1 wontfix");
  });

  it("routes a conflicted worker back to work and supports stop", () => {
    const count = worker("theo").messages.length;
    actions.replyWorker("theo", "Keep the lead version and reapply only the adapter delta.");
    expect(worker("theo").status).toBe("working");
    expect(worker("theo").messages).toHaveLength(count + 2);

    actions.stopWorker("theo");
    expect(worker("theo").status).toBe("cancelled");
  });
});

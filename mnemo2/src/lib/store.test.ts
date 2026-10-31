import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import { actions, app, createInitialState } from "./store";
import { fixtureState } from "../test/fixtures";

const state = () => get(app);

describe("Mnemo2 store production state", () => {
  it("starts empty — no seeded projects, workers, or activity", () => {
    const initial = createInitialState();
    expect(initial.projects).toEqual([]);
    expect(initial.workers).toEqual([]);
    expect(initial.leadEntries).toEqual([]);
    expect(initial.planItems).toEqual([]);
    expect(initial.diffFiles).toEqual([]);
    expect(initial.selectedWorkerId).toBeNull();
  });

  it("never fabricates a lead reply without the native shell", async () => {
    app.set(fixtureState());
    await actions.openProject("mnemo");
    const before = state().leadEntries.length;
    actions.sendLeadMessage("Keep the review flow quiet and explicit.");
    expect(state().leadEntries).toHaveLength(before + 1);
    expect(state().leadEntries.at(-1)).toMatchObject({ author: "user" });
    expect(state().toast?.message).toMatch(/native shell/i);
  });

  it("refuses team actions when the native team runtime is absent", () => {
    app.set(fixtureState());
    const before = state().workers.length;
    actions.replyWorker("ana", "status?");
    expect(state().workers).toHaveLength(before);
    expect(state().toast?.message).toMatch(/team runtime/i);
  });

  it("requires the native runtime for every Phase 3 action without simulating success", async () => {
    app.set(fixtureState());
    await actions.openProject("mnemo");
    const before = state().workers.map((worker) => worker.status);

    await actions.reviewWorker("ana", "review", [], {
      unanchoredPass: true,
      intent: "review the current tip",
      execution: "the lead read the diff itself",
      integration: "no neighbouring contract changes",
      testQuality: "the reducer test covers the action path",
      residualRisk: "nothing beyond the reviewed tip",
    });
    await actions.recordWorkerCheck("ana", "workspace tests", "passed");
    await actions.fixFinding("ana", "ana-copy", "old", "new");
    await actions.spawnFixer("ana", "ana-copy");
    await actions.resolveFinding("ana", "ana-copy", "fixed", "verified");
    await actions.wontfixFinding("ana", "ana-merge", "separate ticket");
    await actions.mergeWorker("ana");
    await actions.revertMerge("ana");
    await actions.stopAll();

    expect(state().workers.map((worker) => worker.status)).toEqual(before);
    expect(state().workers.find((worker) => worker.id === "ana")?.merge).toBeUndefined();
    expect(state().toast?.message).toMatch(/team runtime/i);
    expect("completeFixer" in actions).toBe(false);
  });

  it("keeps the conversation-first navigation", async () => {
    app.set(fixtureState());
    await actions.openProject("mnemo");
    actions.openWorkbench("diff");
    actions.openTeam();
    expect(state().route).toBe("team");
    expect(state().workbenchOpen).toBe(false);
    expect(state().leadEntries).toEqual([]);
  });

  it("closes a project back to the empty start route", async () => {
    app.set(fixtureState());
    await actions.openProject("mnemo");
    await actions.closeProject();
    expect(state().route).toBe("start");
    expect(state().projectId).toBeNull();
    expect(state().workers).toEqual([]);
  });

  it("keeps the unsent draft in store state and resets it on a project switch", async () => {
    app.set(fixtureState());
    const first = "data:image/png;base64,one";
    const second = "data:image/png;base64,two";
    actions.setLeadDraftText("keep this draft");
    actions.addLeadDraftImage(first);
    actions.addLeadDraftImage(second);
    expect(state().leadDraft).toEqual({ text: "keep this draft", images: [first, second] });

    actions.removeLeadDraftImage(0);
    expect(state().leadDraft.images).toEqual([second]);

    await actions.openProject("garden-game");
    expect(state().projectId).toBe("garden-game");
    expect(state().leadDraft).toEqual({ text: "", images: [] });
  });

  it("accepts an image-only submission but ignores a genuinely empty one", () => {
    app.set(fixtureState());
    const before = state().leadEntries.length;
    actions.sendLeadMessage("   ");
    expect(state().leadEntries).toHaveLength(before);

    const image = "data:image/png;base64,one";
    actions.sendLeadMessage("", [image]);
    expect(state().leadEntries).toHaveLength(before + 1);
    expect(state().leadEntries.at(-1)).toMatchObject({
      author: "user",
      body: "",
      images: [image],
    });
    expect(state().toast?.message).toMatch(/native shell/i);
  });

  it("clears the whole draft when asked", () => {
    app.set(fixtureState());
    actions.setLeadDraftText("still here");
    actions.addLeadDraftImage("data:image/png;base64,one");
    actions.clearLeadDraft();
    expect(state().leadDraft).toEqual({ text: "", images: [] });
  });
});

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

  it("never fabricates a lead reply without the native shell", () => {
    app.set(fixtureState());
    actions.openProject("mnemo");
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

  it("does not simulate Phase 3 review, fixer, or merge work", () => {
    app.set(fixtureState());
    actions.openProject("mnemo");
    const before = state().workers.map((worker) => worker.status);

    actions.fixFinding("ana", "ana-copy");
    actions.spawnFixer("ana", "ana-copy");
    actions.completeFixer("ana", "ana-copy");
    actions.wontfixFinding("ana", "ana-merge", "separate ticket");
    actions.mergeWorker("ana");

    expect(state().workers.map((worker) => worker.status)).toEqual(before);
    expect(state().workers.find((worker) => worker.id === "ana")?.merge).toBeUndefined();
    expect(state().toast?.message).toMatch(/Phase 3/);
  });

  it("keeps the conversation-first navigation", () => {
    app.set(fixtureState());
    actions.openProject("mnemo");
    actions.openWorkbench("diff");
    actions.openTeam();
    expect(state().route).toBe("team");
    expect(state().workbenchOpen).toBe(false);
    expect(state().leadEntries.length).toBeGreaterThan(0);
  });

  it("closes a project back to the empty start route", () => {
    app.set(fixtureState());
    actions.openProject("mnemo");
    actions.closeProject();
    expect(state().route).toBe("start");
    expect(state().projectId).toBeNull();
    expect(state().workers).toEqual([]);
  });
});

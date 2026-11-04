import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import { actions, app, createInitialState } from "./store";

const state = () => get(app);

describe("Memory path hand-overs", () => {
  beforeEach(() => {
    // A project is open on the Memory tab, with the Workbench closed.
    app.set({
      ...createInitialState(),
      projectId: "mnemo",
      route: "memory",
      workbenchOpen: false,
      workbenchTab: "browser",
    });
  });

  it("opens a plan document on the Plans route with the id pending", () => {
    actions.openPlanDocument(" fc536488 ");

    expect(state().route).toBe("plans");
    expect(state().planDocumentId).toBe("fc536488");
    expect(state().workbenchOpen).toBe(false);
  });

  it("clears a consumed plan hand-over", () => {
    actions.openPlanDocument("fc536488");
    actions.clearPlanDocument();

    expect(state().planDocumentId).toBeNull();
  });

  it("ignores an empty plan id", () => {
    actions.openPlanDocument("   ");

    expect(state().route).toBe("memory");
    expect(state().planDocumentId).toBeNull();
  });

  it("opens a file in the Workbench's Files tab at its line", () => {
    actions.openFileInWorkbench("src/agent/turn.rs", 4352);

    expect(state().route).toBe("lead");
    expect(state().workbenchOpen).toBe(true);
    expect(state().workbenchTab).toBe("files");
    expect(state().workbenchFile).toEqual({ path: "src/agent/turn.rs", line: 4352 });
  });

  it("opens a file without a line at the top", () => {
    actions.openFileInWorkbench("mnemo2/src/lib/store.ts");

    expect(state().workbenchFile).toEqual({ path: "mnemo2/src/lib/store.ts", line: null });
  });

  it("ignores an empty path and an unusable line", () => {
    actions.openFileInWorkbench("  ", 3);
    expect(state().workbenchFile).toBeNull();
    expect(state().workbenchOpen).toBe(false);

    actions.openFileInWorkbench("src/main.rs", 0);
    expect(state().workbenchFile).toEqual({ path: "src/main.rs", line: null });
  });

  it("clears a consumed file hand-over", () => {
    actions.openFileInWorkbench("src/main.rs", 2);
    actions.clearWorkbenchFile();

    expect(state().workbenchFile).toBeNull();
  });

  it("opens a path ref in the Workbench's Files tab at its line", () => {
    actions.openRepoPath({ path: "src/a.rs", line: 42, kind: "file", planId: null });

    expect(state().route).toBe("lead");
    expect(state().workbenchOpen).toBe(true);
    expect(state().workbenchTab).toBe("files");
    expect(state().workbenchFile).toEqual({ path: "src/a.rs", line: 42 });
  });

  it("opens a plan ref on the Plans route", () => {
    actions.openRepoPath({
      path: ".coding/plans/abc12345.md",
      line: null,
      kind: "plan",
      planId: "abc12345",
    });

    expect(state().route).toBe("plans");
    expect(state().planDocumentId).toBe("abc12345");
    expect(state().workbenchFile).toBeNull();
  });

  it("falls back to the file for a plan ref that carries no id", () => {
    actions.openRepoPath({ path: ".coding/plans/orphan.md", line: null, kind: "plan", planId: null });

    expect(state().workbenchTab).toBe("files");
    expect(state().workbenchFile).toEqual({ path: ".coding/plans/orphan.md", line: null });
  });
});

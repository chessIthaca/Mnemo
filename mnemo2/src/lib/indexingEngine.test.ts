import { describe, expect, it } from "vitest";
import { runningPasses, type IndexingSnapshot } from "./indexingEngine";

function snapshot(patch: Partial<IndexingSnapshot>): IndexingSnapshot {
  return { revision: 1, firstRun: false, code: null, memory: null, ...patch };
}

function running(done: number, total: number) {
  return { state: "running" as const, done, total, detail: null };
}

describe("runningPasses", () => {
  it("lists the passes in display order, the memory re-embed last", () => {
    const kinds = runningPasses(snapshot({
      code: running(120, 450),
      memory: running(3, 4),
      laya: running(34, 207),
      embeddings: running(3200, 12444),
    })).map(({ kind }) => kind);
    expect(kinds).toEqual(["code", "memory", "laya", "embeddings"]);
  });

  it("shows a running memory re-embed on its own", () => {
    expect(runningPasses(snapshot({ embeddings: running(3200, 12444) }))).toEqual([
      { kind: "embeddings", pass: running(3200, 12444) },
    ]);
  });

  it("ignores a pass that already ended", () => {
    expect(runningPasses(snapshot({
      code: { state: "done", done: 450, total: 450, detail: null },
      embeddings: { state: "failed", done: 0, total: 0, detail: "store closed" },
    }))).toEqual([]);
  });

  it("reads a snapshot from an older host without the embeddings key", () => {
    const older: IndexingSnapshot = { revision: 1, firstRun: false, code: null, memory: running(1, 2) };
    expect(runningPasses(older).map(({ kind }) => kind)).toEqual(["memory"]);
    expect(runningPasses(null)).toEqual([]);
  });
});

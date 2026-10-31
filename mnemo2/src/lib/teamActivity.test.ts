import { describe, expect, it } from "vitest";
import { insertActivityCard, keepActivityCards, type ActivityCard } from "./teamActivity";
import type { LeadEntry } from "./types";

const message = (id: string, body: string): LeadEntry => ({
  id, kind: "message", author: "lead", body, time: "restored",
});
const tool = (id: string, toolName: string, workerId: string, extra: Partial<LeadEntry> = {}): LeadEntry => ({
  id, kind: "tool", time: "restored", toolCallId: `call-${id}`, toolName,
  arguments: JSON.stringify({ worker_id: workerId }), liveOutput: "",
  result: { success: true, output: "ok" }, ...extra,
} as LeadEntry);
const review = (workerId: string, atMs: number): ActivityCard => ({
  id: `review-${workerId}-${atMs}`, kind: "review", workerId, title: `Reviewed ${workerId}’s work`,
  body: "", time: "", atMs,
});
const merge = (workerId: string, atMs: number): ActivityCard => ({
  id: `merge-${workerId}`, kind: "merge", workerId, time: "", atMs,
  record: { id: `merge-${workerId}`, workerId } as Extract<LeadEntry, { kind: "merge" }>["record"],
});
const ids = (entries: LeadEntry[]) => entries.map((entry) => entry.id);

describe("lead-chat activity cards", () => {
  it("places a review and a landing right after the lead's own calls, not at the bottom", () => {
    const entries = [
      message("m1", "Reviewing a."),
      tool("t1", "team_review", "a"),
      tool("t2", "team_merge", "a"),
      message("m2", "Now b."),
      tool("t3", "team_review", "b"),
      message("m3", "Later work."),
    ];
    insertActivityCard(entries, merge("a", 200));
    insertActivityCard(entries, review("a", 100));
    insertActivityCard(entries, review("b", 300));
    expect(ids(entries)).toEqual([
      "m1", "t1", "review-a-100", "t2", "merge-a", "m2", "t3", "review-b-300", "m3",
    ]);
  });

  it("anchors to the latest successful call for that worker only", () => {
    const entries = [
      tool("t1", "team_merge", "a"),
      tool("t2", "team_merge", "b", { result: { success: false, output: "refused" } }),
      tool("t3", "team_merge", "b"),
      message("m1", "done"),
    ];
    insertActivityCard(entries, merge("b", 50));
    expect(ids(entries)).toEqual(["t1", "t2", "t3", "merge-b", "m1"]);
  });

  it("places a card with no call (a human merge) by time, else at the end", () => {
    const entries: LeadEntry[] = [
      tool("t1", "team_review", "a"),
      review("a", 100),
      message("m1", "between"),
      tool("t2", "team_review", "b"),
      review("b", 300),
    ];
    insertActivityCard(entries, merge("c", 200));
    expect(ids(entries)).toEqual(["t1", "review-a-100", "merge-c", "m1", "t2", "review-b-300"]);
    insertActivityCard(entries, merge("d", 400));
    expect(ids(entries).at(-1)).toBe("merge-d");
  });

  it("keeps cards where they were when the chat is rebuilt from the transcript", () => {
    const live: LeadEntry[] = [
      { ...message("lead-stream-1", "Reviewing a.") },
      tool("live-t1", "team_review", "a"),
      review("a", 100),
      { ...message("lead-stream-2", "Merging c by hand next.") },
      merge("c", 150),
      { ...message("lead-stream-3", "Done.") },
    ];
    const rebuilt = [
      message("history-0", "Reviewing a."),
      tool("history-tool-1-0", "team_review", "a", { toolCallId: "call-live-t1" } as Partial<LeadEntry>),
      message("history-2", "Merging c by hand next."),
      message("history-3", "Done."),
    ];
    expect(ids(keepActivityCards(rebuilt, live))).toEqual([
      "history-0", "history-tool-1-0", "review-a-100", "history-2", "merge-c", "history-3",
    ]);
  });

  it("places restored cards by their calls when nothing preceded them", () => {
    const restoredFirst: LeadEntry[] = [review("a", 100), merge("a", 120)];
    const rebuilt = [
      message("history-0", "Start."),
      tool("history-tool-1-0", "team_review", "a"),
      tool("history-tool-1-1", "team_merge", "a"),
      message("history-2", "Next task."),
      tool("history-tool-3-0", "read_files", "x"),
    ];
    expect(ids(keepActivityCards(rebuilt, restoredFirst))).toEqual([
      "history-0", "history-tool-1-0", "review-a-100", "history-tool-1-1", "merge-a",
      "history-2", "history-tool-3-0",
    ]);
  });
});

import { describe, expect, it } from "vitest";
import { ENTRY_WINDOW, earlierEntriesLabel, windowEntries } from "./entryWindow";

describe("windowEntries", () => {
  it("keeps the whole list when it fits the limit", () => {
    const entries = ["first", "second", "third"];
    const window = windowEntries(entries, ENTRY_WINDOW);
    expect(window.hidden).toBe(0);
    expect(window.visible).toBe(entries);
  });

  it("keeps the newest 200 of 250 entries", () => {
    const entries = Array.from({ length: 250 }, (_, index) => index);
    const window = windowEntries(entries, ENTRY_WINDOW);
    expect(window.hidden).toBe(50);
    expect(window.visible).toEqual(entries.slice(50));
    expect(window.visible[0]).toBe(50);
    expect(window.visible.at(-1)).toBe(249);
  });

  it("hides everything with a limit of zero", () => {
    const entries = ["first", "second"];
    expect(windowEntries(entries, 0)).toEqual({ hidden: 2, visible: [] });
  });

  it("floors a fractional limit and treats a negative one as zero", () => {
    const entries = ["first", "second", "third"];
    expect(windowEntries(entries, 2.9).visible).toEqual(["second", "third"]);
    expect(windowEntries(entries, -5)).toEqual({ hidden: 3, visible: [] });
  });
});

describe("earlierEntriesLabel", () => {
  it("names a single hidden message", () => {
    expect(earlierEntriesLabel(1)).toBe("Show 1 earlier message");
  });

  it("counts the hidden messages", () => {
    expect(earlierEntriesLabel(42)).toBe("Show earlier messages (42 hidden)");
  });
});

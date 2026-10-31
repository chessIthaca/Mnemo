import { act, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import type { LeadEntry } from "../lib/types";
import MergeCard from "./MergeCard.svelte";

describe("MergeCard", () => {
  beforeEach(() => app.set(fixtureState()));

  it("shows only the icon and headline until expanded, with no card box", async () => {
    const entry = fixtureState().leadEntries.find(
      (item): item is Extract<LeadEntry, { kind: "merge" }> => item.kind === "merge",
    );
    if (!entry) throw new Error("the fixture has a merge record");
    const { container } = render(MergeCard, { entry });
    const row = container.querySelector<HTMLDetailsElement>("details.event-row");
    expect(row?.querySelector("summary")?.textContent).toContain(`work landed in ${entry.record.target}`);
    expect(container.querySelector(".merge-card")).toBeNull();
    expect(screen.queryByRole("button", { name: /Revert merge/ })).toBeNull();

    row!.open = true;
    row!.dispatchEvent(new Event("toggle"));
    await act();
    expect(screen.getByText(entry.record.commit)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Revert merge/ })).toBeTruthy();
  });
});

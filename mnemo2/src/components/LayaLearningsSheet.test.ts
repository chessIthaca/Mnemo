import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";
import type { LayaLearningsTransport, LearningsOffer } from "../lib/layaLearnings";
import LayaLearningsSheet from "./LayaLearningsSheet.svelte";

/** One offer with only the fields under test spelled out. */
function offer(overrides: Partial<LearningsOffer> = {}): LearningsOffer {
  return {
    source_key: "project",
    source_label: "This project's repository",
    sha256: "abc123",
    new_rows: 2,
    local_only_rows: 1,
    ...overrides,
  };
}

/** An in-memory transport that answers like the native host, with spies. */
function fakeTransport(initial: LearningsOffer[]) {
  let pending = [...initial];
  let emit: ((offers: LearningsOffer[]) => void) | null = null;
  const offers = vi.fn(async () => pending);
  const decide = vi.fn(async (target: LearningsOffer, _accept: boolean) => {
    pending = pending.filter((candidate) => candidate.source_key !== target.source_key);
    return pending;
  });
  const onOffers = vi.fn(async (handler: (offers: LearningsOffer[]) => void) => {
    emit = handler;
    return () => {
      emit = null;
    };
  });
  const transport: LayaLearningsTransport = { offers, decide, onOffers };
  return {
    transport,
    offers,
    decide,
    onOffers,
    /** What the host's `mnemo2://laya-learnings` event carries. */
    broadcast: (next: LearningsOffer[]) => {
      pending = next;
      emit?.(next);
    },
  };
}

/** Wait for the sheet to be open with the given sentence on it. */
async function shownQuestion(): Promise<string> {
  const dialog = await screen.findByRole("dialog", { name: "Update Laya's learnings?" });
  return dialog.textContent ?? "";
}

describe("LayaLearningsSheet", () => {
  it("renders nothing outside Tauri", () => {
    render(LayaLearningsSheet);

    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("renders nothing when no copy is pending", async () => {
    const { transport, onOffers } = fakeTransport([]);
    render(LayaLearningsSheet, { props: { transport } });

    await waitFor(() => expect(onOffers).toHaveBeenCalled());
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("shows the question for a pending copy", async () => {
    const { transport } = fakeTransport([offer({ new_rows: 2, local_only_rows: 5 })]);
    render(LayaLearningsSheet, { props: { transport } });

    expect(await shownQuestion()).toContain(
      "This project's repository has 2 routing decisions this computer doesn't have, " +
        "and this computer has 5 of its own that the copy doesn't. " +
        "Updating adds the new ones and removes nothing.",
    );
  });

  it("updates when Update is clicked and closes when nothing is left", async () => {
    const { transport, decide } = fakeTransport([offer()]);
    render(LayaLearningsSheet, { props: { transport } });
    await shownQuestion();

    await fireEvent.click(screen.getByRole("button", { name: "Update" }));

    await waitFor(() => expect(decide).toHaveBeenCalledWith(offer(), true));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("declines when Not now is clicked", async () => {
    const { transport, decide } = fakeTransport([offer()]);
    render(LayaLearningsSheet, { props: { transport } });
    await shownQuestion();

    await fireEvent.click(screen.getByRole("button", { name: "Not now" }));

    await waitFor(() => expect(decide).toHaveBeenCalledWith(offer(), false));
  });

  it("declines when the sheet is closed with its X", async () => {
    const { transport, decide } = fakeTransport([offer()]);
    render(LayaLearningsSheet, { props: { transport } });
    await shownQuestion();

    await fireEvent.click(screen.getByRole("button", { name: "Close" }));

    await waitFor(() => expect(decide).toHaveBeenCalledWith(offer(), false));
  });

  it("shows two pending copies one after the other", async () => {
    const { transport, decide } = fakeTransport([
      offer({ source_key: "project", source_label: "This project's repository" }),
      offer({ source_key: "installer", source_label: "The Mnemo2 installer" }),
    ]);
    render(LayaLearningsSheet, { props: { transport } });

    expect(await shownQuestion()).toContain("This project's repository has");
    await fireEvent.click(screen.getByRole("button", { name: "Update" }));

    await waitFor(async () =>
      expect((await shownQuestion()).includes("The Mnemo2 installer has")).toBe(true),
    );
    expect(decide).toHaveBeenCalledTimes(1);
  });

  it("shows the alert and drops the copy when the host rejects the answer", async () => {
    const { transport, decide } = fakeTransport([
      offer({ source_key: "project", source_label: "This project's repository" }),
      offer({ source_key: "installer", source_label: "The Mnemo2 installer" }),
    ]);
    decide.mockRejectedValueOnce("that update is no longer pending");
    render(LayaLearningsSheet, { props: { transport } });
    await shownQuestion();

    await fireEvent.click(screen.getByRole("button", { name: "Update" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("that update is no longer pending");
    expect(await shownQuestion()).toContain("The Mnemo2 installer has");
  });

  it("opens when the host pushes a new offer", async () => {
    const { transport, onOffers, broadcast } = fakeTransport([]);
    render(LayaLearningsSheet, { props: { transport } });
    await waitFor(() => expect(onOffers).toHaveBeenCalled());
    expect(screen.queryByRole("dialog")).toBeNull();

    broadcast([offer()]);

    expect(await shownQuestion()).toContain("This project's repository has 2 routing decisions");
  });

  it("an empty event closes the sheet", async () => {
    const { transport, onOffers, broadcast } = fakeTransport([offer()]);
    render(LayaLearningsSheet, { props: { transport } });
    await shownQuestion();
    await waitFor(() => expect(onOffers).toHaveBeenCalled());

    broadcast([]);

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("answers once when Update is clicked twice", async () => {
    const { transport, decide } = fakeTransport([offer()]);
    decide.mockImplementationOnce(() => new Promise<LearningsOffer[]>(() => {}));
    render(LayaLearningsSheet, { props: { transport } });
    await shownQuestion();
    const update = screen.getByRole("button", { name: "Update" });

    await fireEvent.click(update);
    await fireEvent.click(update);

    expect(decide).toHaveBeenCalledTimes(1);
  });
});

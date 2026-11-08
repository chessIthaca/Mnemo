import { describe, expect, it } from "vitest";
import {
  LAYA_LEARNINGS_CHANNEL,
  createLayaLearningsTransport,
  createTauriLayaLearningsTransport,
  offerMessage,
  type LayaLearningsBindings,
  type LearningsOffer,
} from "./layaLearnings";

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

/** The Tauri bindings with every invoke and listen recorded. */
function recordingBindings() {
  const calls: { command: string; payload: Record<string, unknown> }[] = [];
  const listened: { channel: string; handler: (event: { payload: LearningsOffer[] }) => void }[] =
    [];
  const bindings = {
    invoke: (async (command: string, payload?: Record<string, unknown>) => {
      calls.push({ command, payload: payload ?? {} });
      return [offer()];
    }) as unknown as LayaLearningsBindings["invoke"],
    listen: (async (channel: string, handler: (event: { payload: LearningsOffer[] }) => void) => {
      listened.push({ channel, handler });
      return () => {};
    }) as unknown as LayaLearningsBindings["listen"],
  };
  return { calls, listened, bindings };
}

describe("layaLearnings", () => {
  it("is null in a browser-only session and native when bindings are given", () => {
    expect(createLayaLearningsTransport()).toBeNull();
    const { bindings } = recordingBindings();
    expect(createLayaLearningsTransport(bindings)).not.toBeNull();
  });

  it("asks the host for the pending offers", async () => {
    const { calls, bindings } = recordingBindings();
    const transport = createTauriLayaLearningsTransport(bindings);

    const pending = await transport.offers();

    expect(calls).toEqual([{ command: "laya_learnings_offers", payload: {} }]);
    expect(pending).toEqual([offer()]);
  });

  it("passes sourceKey, sha256 and accept to the decide command", async () => {
    const { calls, bindings } = recordingBindings();
    const transport = createTauriLayaLearningsTransport(bindings);

    await transport.decide(offer({ source_key: "installer", sha256: "deadbeef" }), true);
    await transport.decide(offer(), false);

    expect(calls).toEqual([
      {
        command: "laya_learnings_decide",
        payload: { sourceKey: "installer", sha256: "deadbeef", accept: true },
      },
      {
        command: "laya_learnings_decide",
        payload: { sourceKey: "project", sha256: "abc123", accept: false },
      },
    ]);
  });

  it("subscribes to the offers channel and forwards the event payload", async () => {
    const { listened, bindings } = recordingBindings();
    const transport = createTauriLayaLearningsTransport(bindings);
    const seen: LearningsOffer[][] = [];

    await transport.onOffers((next) => seen.push(next));
    expect(listened).toHaveLength(1);
    expect(listened[0].channel).toBe(LAYA_LEARNINGS_CHANNEL);
    listened[0].handler({ payload: [offer()] });

    expect(seen).toEqual([[offer()]]);
  });

  it("spells the question in the singular for one new row", () => {
    expect(offerMessage(offer({ new_rows: 1, local_only_rows: 4 }))).toBe(
      "This project's repository has 1 routing decision this computer doesn't have, " +
        "and this computer has 4 of its own that the copy doesn't. " +
        "Updating adds the new ones and removes nothing.",
    );
  });

  it("spells the question in the plural for several new rows", () => {
    expect(offerMessage(offer({ new_rows: 3, local_only_rows: 0 }))).toBe(
      "This project's repository has 3 routing decisions this computer doesn't have, " +
        "and this computer has 0 of its own that the copy doesn't. " +
        "Updating adds the new ones and removes nothing.",
    );
  });
});

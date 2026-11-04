import { beforeEach, describe, expect, it } from "vitest";
import {
  LAYA_FLAG_META,
  LAYA_STATUS_META,
  ROUTING_LANES,
  createLayaTransport,
  layaStatusHint,
  leavesPage,
  routingDraft,
  routingWire,
  setPreviewLayaTransport,
  type LayaFlagKey,
  type LayaStatusWord,
  type LayaTransport,
  type RoutingLanes,
} from "./layaSettings";

/** The seven wire flag keys, as `laya_status` reports them. */
const SEVEN_KEYS: LayaFlagKey[] = [
  "auto_type_memories",
  "failure_triage",
  "failure_triage_knn",
  "auto_finetune",
  "steer_tool_choice",
  "routing",
  "reflex",
];

const LANES: RoutingLanes = {
  cheap: "ep/tiny",
  routine: "ep/big",
  capable: "ep/big",
  lane_medium: null,
  lane_high: "ep/org/model-id",
  escalate: null,
  threshold: 0.8,
  enforce: false,
};

describe("layaSettings", () => {
  beforeEach(() => setPreviewLayaTransport(null));

  it("is null in a browser-only session until the preview seeds a transport", () => {
    expect(createLayaTransport()).toBeNull();
    const seeded = {} as LayaTransport;
    setPreviewLayaTransport(seeded);
    expect(createLayaTransport()).toBe(seeded);
    setPreviewLayaTransport(null);
    expect(createLayaTransport()).toBeNull();
  });

  it("describes exactly the seven wire flags — never the master enabled key", () => {
    expect(LAYA_FLAG_META.map((flag) => flag.key).sort()).toEqual([...SEVEN_KEYS].sort());
    // Every flag carries a plain one-line explanation.
    for (const flag of LAYA_FLAG_META) expect(flag.explanation.trim()).not.toBe("");
    // Only auto_finetune rides a base checkpoint; the five classifier tasks warn.
    for (const flag of LAYA_FLAG_META) {
      if (flag.key === "auto_finetune") {
        expect(flag.note).toContain("local");
        expect(flag.note).toContain("background");
      } else if (flag.key === "failure_triage_knn") {
        expect(flag.note).toBeTruthy();
      } else {
        expect(flag.note).toContain("fine-tuned checkpoint");
      }
    }
  });

  it("words every status and hints each one distinctly", () => {
    const words: LayaStatusWord[] = ["disabled", "not_installed", "installing", "starting", "ready", "failed"];
    expect(new Set(words).size).toBe(6);
    for (const word of words) {
      expect(LAYA_STATUS_META[word].trim()).not.toBe("");
      expect(layaStatusHint(word).trim()).not.toBe("");
    }
    expect(LAYA_STATUS_META.disabled).toBe("Off");
    expect(LAYA_STATUS_META.not_installed).toBe("Not installed");
  });

  it("seeds a routing draft from the wire lanes", () => {
    expect(routingDraft(LANES)).toEqual({
      cheap: "ep/tiny",
      capable: "ep/big",
      lane_medium: "",
      lane_high: "ep/org/model-id",
      routine: "ep/big",
      escalate: "",
      threshold: "0.8",
      enforce: false,
    });
  });

  it("parses lane refs into endpoint/model pairs, keeping slashes inside the model id", () => {
    const parsed = routingWire(routingDraft(LANES));
    expect(parsed.error).toBeUndefined();
    expect(parsed.value).toEqual({
      cheap: { endpoint: "ep", model: "tiny" },
      routine: { endpoint: "ep", model: "big" },
      capable: { endpoint: "ep", model: "big" },
      lane_medium: null,
      lane_high: { endpoint: "ep", model: "org/model-id" },
      escalate: null,
      threshold: 0.8,
      enforce: false,
    });
  });

  it("clears an empty lane to an explicit null and passes threshold and enforce through", () => {
    const draft = { ...routingDraft(LANES), capable: "  ", threshold: "0.95", enforce: true };
    const parsed = routingWire(draft);
    expect(parsed.error).toBeUndefined();
    expect(parsed.value?.capable).toBeNull();
    expect(parsed.value?.threshold).toBe(0.95);
    expect(parsed.value?.enforce).toBe(true);
  });

  it("rejects a malformed lane ref with the lane's label", () => {
    const parsed = routingWire({ ...routingDraft(LANES), lane_medium: "no-slash" });
    expect(parsed.value).toBeUndefined();
    expect(parsed.error).toBe('Medium lane model must be "endpoint/model"');
  });

  it("rejects a threshold that is not a number from 0 to 1", () => {
    for (const threshold of ["", "abc", "-0.1", "1.2", "9"]) {
      const parsed = routingWire({ ...routingDraft(LANES), threshold });
      expect(parsed.value).toBeUndefined();
      expect(parsed.error).toBe("Threshold must be a number from 0 to 1");
    }
  });

  it("lists the six routing lanes in display order", () => {
    expect(ROUTING_LANES.map((lane) => lane.key)).toEqual([
      "cheap",
      "routine",
      "capable",
      "lane_medium",
      "lane_high",
      "escalate",
    ]);
    for (const lane of ROUTING_LANES) expect(lane.blurb.trim()).not.toBe("");
  });

  it("marks Escape keydowns and outside button clicks as leaving the page", () => {
    const root = document.createElement("div");
    document.body.appendChild(root);
    const insideButton = document.createElement("button");
    root.appendChild(insideButton);
    const outsideButton = document.createElement("button");
    document.body.appendChild(outsideButton);
    const outsideText = document.createElement("p");
    document.body.appendChild(outsideText);

    const withRoot: boolean[] = [];
    const withoutRoot: boolean[] = [];
    const clickHandler = (event: Event): void => {
      withRoot.push(leavesPage(event, root));
      withoutRoot.push(leavesPage(event, null));
    };
    window.addEventListener("click", clickHandler, true);
    insideButton.click();
    outsideButton.click();
    outsideText.click();
    window.removeEventListener("click", clickHandler, true);
    // An in-page click keeps its own meaning; an outside button leaves.
    expect(withRoot).toEqual([false, true, false]);
    // Without a bound root, every button click counts as leaving.
    expect(withoutRoot).toEqual([true, true, false]);

    const keys: boolean[] = [];
    const keyHandler = (event: Event): void => {
      keys.push(leavesPage(event, root));
    };
    window.addEventListener("keydown", keyHandler, true);
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    window.removeEventListener("keydown", keyHandler, true);
    expect(keys).toEqual([true, false]);

    // beforeunload is handled by its own branch, never as a leaving click.
    expect(leavesPage(new Event("beforeunload"), root)).toBe(false);

    root.remove();
    outsideButton.remove();
    outsideText.remove();
  });
});
import { describe, expect, it } from "vitest";
import { createUnsavedRegistry } from "./unsavedEdits";

describe("unsaved edits registry", () => {
  it("registry reports unsaved only while a registered check is true", () => {
    const registry = createUnsavedRegistry();
    expect(registry.hasUnsaved()).toBe(false);

    let dirty = false;
    registry.register(() => dirty);
    expect(registry.hasUnsaved()).toBe(false);

    dirty = true;
    expect(registry.hasUnsaved()).toBe(true);

    dirty = false;
    expect(registry.hasUnsaved()).toBe(false);
  });

  it("an unregistered check no longer counts", () => {
    const registry = createUnsavedRegistry();
    const stop = registry.register(() => true);
    expect(registry.hasUnsaved()).toBe(true);

    stop();
    expect(registry.hasUnsaved()).toBe(false);
    // Removing it twice stays harmless.
    stop();
    expect(registry.hasUnsaved()).toBe(false);
  });
});

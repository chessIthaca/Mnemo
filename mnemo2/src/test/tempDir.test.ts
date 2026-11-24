import { expect, it } from "vitest";

import {
  checkoutRootOf,
  isInside,
  samePath,
  targetTmpOf,
  tmpdirNow,
} from "../../../scripts/test-tmp.mjs";

// The suite's temp folders must stay in this checkout's target/tmp (and on its
// drive), not the user's system temp folder; a leak there is plan 35105099's bug.
it("keeps os.tmpdir() inside the repo target/tmp", () => {
  const actual = tmpdirNow();
  expect(
    isInside(actual, checkoutRootOf(import.meta.url)),
    `os.tmpdir() ${actual} must lie inside the checkout`,
  ).toBe(true);
  expect(
    samePath(actual, targetTmpOf(import.meta.url)),
    `os.tmpdir() ${actual} must be the checkout's target/tmp`,
  ).toBe(true);
});

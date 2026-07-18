import { describe, expect, it } from "vitest";

import { isVisibleTerminal } from "./session-visibility";
import type { ProcessState } from "./types";

describe("isVisibleTerminal", () => {
  it.each<[ProcessState, boolean]>([
    ["starting", true],
    ["running", true],
    ["exited", false],
    ["killed", false],
  ])("maps %s to %s", (processState, expected) => {
    expect(isVisibleTerminal({ processState })).toBe(expected);
  });
});

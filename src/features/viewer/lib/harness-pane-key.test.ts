// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { harnessPaneKey } from "./harness-pane-key";

describe("harnessPaneKey", () => {
  it("keeps the bare workspace id for the first tab", () => {
    expect(harnessPaneKey("ws-a", 0, "tab-1")).toBe("ws-a");
  });

  it("gives every later tab its own key", () => {
    const keys = ["tab-1", "tab-2", "tab-3"].map((id, i) => harnessPaneKey("ws-a", i, id));
    expect(new Set(keys).size).toBe(3);
    expect(keys[1]).toBe("ws-a::tab-2");
  });
});

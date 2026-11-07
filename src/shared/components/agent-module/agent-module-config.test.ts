// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { shouldRegisterPaneChip } from "./agent-module-config";

/**
 * SW-GS-12 (§9) primitive, exercised as SW-GS-32's visibility knob (AC-4): whether a
 * connect adapter registers a born-minimized tray-chip workspace record. `"hidden"`
 * skips it (headless); everything else — including the default — registers it.
 */
describe("shouldRegisterPaneChip (SW-GS-32 AC-4 — the visibility knob)", () => {
  it('"hidden" → false (no tray chip; git-sync + notes default)', () => {
    expect(shouldRegisterPaneChip("hidden")).toBe(false);
  });

  it('"minimized" → true (always-visible chip)', () => {
    expect(shouldRegisterPaneChip("minimized")).toBe(true);
  });

  it("undefined (the default) → true", () => {
    expect(shouldRegisterPaneChip(undefined)).toBe(true);
    expect(shouldRegisterPaneChip()).toBe(true);
  });
});

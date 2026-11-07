// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  BACKGROUND_MODE_EXPLAINER,
  shouldShowBackgroundExplainer,
} from "./use-background-mode-notification";

describe("shouldShowBackgroundExplainer (SW-AU-34 AC-5 one-shot gate)", () => {
  it("shows on a fresh profile (undefined → true)", () => {
    expect(shouldShowBackgroundExplainer(undefined)).toBe(true);
  });

  it("shows when not yet seen (false → true)", () => {
    expect(shouldShowBackgroundExplainer(false)).toBe(true);
  });

  it("suppresses once seen (true → false)", () => {
    expect(shouldShowBackgroundExplainer(true)).toBe(false);
  });

  it("explains that the app is still running and how to reopen it", () => {
    expect(BACKGROUND_MODE_EXPLAINER).toMatch(/still running/i);
    expect(BACKGROUND_MODE_EXPLAINER).toMatch(/dock/i);
  });
});

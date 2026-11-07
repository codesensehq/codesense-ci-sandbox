// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import { startUpdateCheck } from "./update-check";

describe("startUpdateCheck (SW-PB-13 AC-2)", () => {
  it("in a beta channel never loads or runs the GitHub update check", () => {
    for (const channel of ["beta", "beta-staging"] as const) {
      const runUpdateCheck = vi.fn().mockResolvedValue(undefined);
      const load = vi.fn().mockResolvedValue({ runUpdateCheck });
      expect(startUpdateCheck({ channel, publicHost: "codesense.ai" }, load)).toBeNull();
      expect(load).not.toHaveBeenCalled();
      expect(runUpdateCheck).not.toHaveBeenCalled();
    }
  });

  it("in the internal channel still runs it", async () => {
    const runUpdateCheck = vi.fn().mockResolvedValue(undefined);
    const load = vi.fn().mockResolvedValue({ runUpdateCheck });
    await startUpdateCheck({ channel: "internal", publicHost: "codesense.ai" }, load);
    expect(runUpdateCheck).toHaveBeenCalledTimes(1);
  });

  it("reads the injected build channel by default", () => {
    window.__CODESENSE_CHANNEL__ = "beta";
    try {
      const load = vi.fn();
      expect(startUpdateCheck(undefined, load)).toBeNull();
      expect(load).not.toHaveBeenCalled();
    } finally {
      delete window.__CODESENSE_CHANNEL__;
    }
  });
});

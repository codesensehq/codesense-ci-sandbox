// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { formatAgo, formatIn, formatLastOutput, formatUpdatedAgo } from "./format-updated-ago";

describe("formatUpdatedAgo", () => {
  const now = 1_000_000_000_000;

  it("reads 'just now' under 5s", () => {
    expect(formatUpdatedAgo(now - 2_000, now)).toBe("updated just now");
  });

  it("renders seconds, minutes, hours, then days", () => {
    expect(formatUpdatedAgo(now - 30_000, now)).toBe("updated 30s ago");
    expect(formatUpdatedAgo(now - 5 * 60_000, now)).toBe("updated 5m ago");
    expect(formatUpdatedAgo(now - 3 * 3_600_000, now)).toBe("updated 3h ago");
    expect(formatUpdatedAgo(now - 2 * 86_400_000, now)).toBe("updated 2d ago");
  });

  it("keeps climbing so a stalled stream reads visibly stale", () => {
    // Same poll time, later now → a larger elapsed label (never resets to fresh).
    const early = formatUpdatedAgo(now - 60_000, now);
    const later = formatUpdatedAgo(now - 60_000, now + 120_000);
    expect(early).toBe("updated 1m ago");
    expect(later).toBe("updated 3m ago");
  });

  it("clamps a future timestamp to 'just now' rather than a negative label", () => {
    expect(formatUpdatedAgo(now + 10_000, now)).toBe("updated just now");
  });
});

describe("formatAgo (bare core shared with the RF-perf-24 worktree sub-line)", () => {
  const now = 1_000_000_000_000;

  it("renders the same cadence without the 'updated ' prefix", () => {
    expect(formatAgo(now - 2_000, now)).toBe("just now");
    expect(formatAgo(now - 30_000, now)).toBe("30s ago");
    expect(formatAgo(now - 2 * 60_000, now)).toBe("2m ago");
    expect(formatAgo(now - 3 * 3_600_000, now)).toBe("3h ago");
    expect(formatAgo(now - 5 * 86_400_000, now)).toBe("5d ago");
  });

  it("stays the single source of truth: formatUpdatedAgo is just a prefix of it", () => {
    expect(formatUpdatedAgo(now - 30_000, now)).toBe(`updated ${formatAgo(now - 30_000, now)}`);
  });
});

describe("formatIn (SW-AU-85D — future-facing 'next check' countdown)", () => {
  const now = 1_000_000_000_000;

  it("counts down in the same s/m/h/d vocabulary", () => {
    expect(formatIn(now + 45_000, now)).toBe("in 45s");
    expect(formatIn(now + 2 * 60_000, now)).toBe("in 2m");
    expect(formatIn(now + 3 * 3_600_000, now)).toBe("in 3h");
    expect(formatIn(now + 5 * 86_400_000, now)).toBe("in 5d");
  });

  it("reads 'due now' within 5s or once overdue (never a negative countdown)", () => {
    expect(formatIn(now + 3_000, now)).toBe("due now");
    expect(formatIn(now - 60_000, now)).toBe("due now");
  });
});

describe("formatLastOutput (SW-OR-19 — shared by the fleet row + the panel)", () => {
  const now = 1_000_000_000_000;

  it("prefixes formatAgo with 'last output ' at the same cadence", () => {
    expect(formatLastOutput(now - 2_000, now)).toBe("last output just now");
    expect(formatLastOutput(now - 12_000, now)).toBe("last output 12s ago");
    expect(formatLastOutput(now - 2 * 60_000, now)).toBe("last output 2m ago");
  });

  it("climbs while output stalls (never falsely resets to fresh)", () => {
    // Same last-output anchor, later clock → a larger label — how a wedged run reads.
    expect(formatLastOutput(now - 5_000, now)).toBe("last output 5s ago");
    expect(formatLastOutput(now - 5_000, now + 90_000)).toBe("last output 1m ago");
  });

  it("is exactly formatAgo with the prefix (one source of truth, panel==row)", () => {
    expect(formatLastOutput(now - 30_000, now)).toBe(`last output ${formatAgo(now - 30_000, now)}`);
  });
});

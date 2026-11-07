// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { validatedOutcomeChip } from "./run-outcome";

describe("validatedOutcomeChip — the SW-OR-13 terminal chip (AC-1)", () => {
  it("maps a validated completion to the celebrated completed chip", () => {
    expect(validatedOutcomeChip("completed")).toBe("completed");
  });

  // SW-OR-20: the 4→4 split — a hard failure gets its OWN red `failed` chip, distinct from
  // the orange `dropped` a needs-attention run still reads.
  it("maps a hard failure to the distinct failed chip", () => {
    expect(validatedOutcomeChip("failed")).toBe("failed");
  });

  it("maps a needs-attention run to the dropped chip", () => {
    expect(validatedOutcomeChip("needs-attention")).toBe("dropped");
  });
});

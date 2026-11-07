// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { comparisonLabel, effectiveBaseRef } from "./source-diff-base";

describe("effectiveBaseRef (SLW-DV-7 AC-2)", () => {
  it("branchBase passes the resolved baseRef through (undefined for fork-from-default)", () => {
    expect(effectiveBaseRef("branchBase", undefined)).toBeUndefined();
    expect(effectiveBaseRef("branchBase", "feat/SLW-DV-1")).toBe("feat/SLW-DV-1");
  });

  it("head resolves to the literal HEAD ref regardless of the resolved baseRef", () => {
    expect(effectiveBaseRef("head", undefined)).toBe("HEAD");
    expect(effectiveBaseRef("head", "feat/SLW-DV-1")).toBe("HEAD");
  });
});

describe("comparisonLabel (SLW-DV-7 AC-1)", () => {
  it("fork-from-default names the default start point, not a fabricated ref", () => {
    expect(comparisonLabel("branchBase", undefined, "main")).toBe("vs main (merge-base)");
    expect(comparisonLabel("branchBase", undefined, "develop")).toBe("vs develop (merge-base)");
  });

  it("stacked story names the resolved dependency branch", () => {
    expect(comparisonLabel("branchBase", "feat/SLW-DV-1", "main")).toBe(
      "vs feat/SLW-DV-1 (merge-base)",
    );
  });

  it("falls back to the default name for an empty resolved ref (never blank)", () => {
    expect(comparisonLabel("branchBase", "", "main")).toBe("vs main (merge-base)");
  });

  it("head reads as working-changes-only", () => {
    expect(comparisonLabel("head", undefined, "main")).toBe("vs HEAD (working changes only)");
    expect(comparisonLabel("head", "feat/SLW-DV-1", "main")).toBe("vs HEAD (working changes only)");
  });
});

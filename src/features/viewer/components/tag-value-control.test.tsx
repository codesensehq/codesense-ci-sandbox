// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { declaredInvalidFlag, isChoiceType, normalizeDeclaredType } from "./tag-value-control";

// The pure dispatch helpers are the drift-critical core shared by the drawer and the
// composer rule builder (AC-16). The rendered dispatch is exercised end-to-end by the
// metadata-band drawer suite, which now delegates to this control.

describe("normalizeDeclaredType", () => {
  it("trims, lowercases, and folds the yes_no alias", () => {
    expect(normalizeDeclaredType(" Choice ")).toBe("choice");
    expect(normalizeDeclaredType("DATE")).toBe("date");
    expect(normalizeDeclaredType("yes_no")).toBe("yes-no");
    expect(normalizeDeclaredType(undefined)).toBeUndefined();
  });
});

describe("isChoiceType", () => {
  it("treats team choice and org select/ordered/status as closed-set", () => {
    for (const t of ["choice", "select", "ordered", "status"]) {
      expect(isChoiceType(t)).toBe(true);
    }
    for (const t of ["text", "multi_select", "date", "number", undefined]) {
      expect(isChoiceType(t)).toBe(false);
    }
  });
});

describe("declaredInvalidFlag", () => {
  it("maps each declared type to its short flag", () => {
    expect(declaredInvalidFlag("choice")).toBe("not allowed");
    expect(declaredInvalidFlag("multi_select")).toBe("not allowed");
    expect(declaredInvalidFlag("number")).toBe("not a number");
    expect(declaredInvalidFlag("date")).toBe("use YYYY-MM-DD");
    expect(declaredInvalidFlag("yes-no")).toBe("use yes or no");
    expect(declaredInvalidFlag("text")).toBe("invalid");
  });
});

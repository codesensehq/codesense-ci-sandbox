// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { classifyHealth } from "./classify-health";

describe("classifyHealth", () => {
  it("zero forward docs → context-desert (built blind)", () => {
    expect(classifyHealth(0)).toBe("context-desert");
    expect(classifyHealth(-1)).toBe("context-desert"); // defensive
  });

  it("one forward doc (below default threshold 2) → thin", () => {
    expect(classifyHealth(1)).toBe("thin");
  });

  it("at or above the threshold → healthy", () => {
    expect(classifyHealth(2)).toBe("healthy");
    expect(classifyHealth(5)).toBe("healthy");
  });

  it("honors a custom threshold", () => {
    expect(classifyHealth(2, 3)).toBe("thin");
    expect(classifyHealth(3, 3)).toBe("healthy");
    expect(classifyHealth(1, 1)).toBe("healthy"); // threshold 1 → 1 is healthy
  });
});

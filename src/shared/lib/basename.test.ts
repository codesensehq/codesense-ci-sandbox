// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { basename } from "./basename";

describe("basename", () => {
  it("returns the last path segment", () => {
    expect(basename("a/b/c.md")).toBe("c.md");
  });

  it("returns the input when there is no slash", () => {
    expect(basename("file.md")).toBe("file.md");
  });

  it("tolerates trailing slashes", () => {
    expect(basename("a/b/")).toBe("b");
    expect(basename("a/b///")).toBe("b");
  });

  it("handles an empty string", () => {
    expect(basename("")).toBe("");
  });
});

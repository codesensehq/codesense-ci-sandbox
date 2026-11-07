// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { resolveRelativePath } from "./resolve-relative-path";

describe("resolveRelativePath", () => {
  it("resolves a same-dir sibling", () => {
    expect(resolveRelativePath("/a/b/epic", "sibling.md")).toBe("/a/b/epic/sibling.md");
  });

  it("resolves a `./`-prefixed ref (drops the `.`)", () => {
    expect(resolveRelativePath("/a/b/epic", "./doc.md")).toBe("/a/b/epic/doc.md");
  });

  it("climbs with `..` into a sibling sub-epic", () => {
    expect(resolveRelativePath("/a/b/epic", "../other/W-5-foo.md")).toBe("/a/b/other/W-5-foo.md");
  });

  it("climbs multiple levels", () => {
    expect(resolveRelativePath("/a/b/c/d", "../../x/y.md")).toBe("/a/b/x/y.md");
  });

  it("floors at root — `..` never pops above `/`", () => {
    expect(resolveRelativePath("/a", "../../../etc/passwd")).toBe("/etc/passwd");
  });

  it("preserves the leading slash for absolute bases and drops it for relative bases", () => {
    expect(resolveRelativePath("/root", "child")).toBe("/root/child");
    expect(resolveRelativePath("root", "child")).toBe("root/child");
  });

  it("collapses empty and `.` segments", () => {
    expect(resolveRelativePath("/a/b", ".//./c//d.md")).toBe("/a/b/c/d.md");
  });
});

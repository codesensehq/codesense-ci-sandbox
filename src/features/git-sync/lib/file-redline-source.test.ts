// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import { joinOrgPath } from "./group-changes-by-node";

const getFileRedlineMock = vi.fn();
vi.mock("@/tauri/commands", () => ({
  getFileRedline: (repoPath: string, filePath: string, oldPath?: string) =>
    getFileRedlineMock(repoPath, filePath, oldPath),
}));

import { getFileRedline, stripOrgRoot } from "./file-redline-source";

describe("stripOrgRoot", () => {
  it("is the inverse of joinOrgPath", () => {
    const orgRoot = "/Users/x/orgs/acme/context";
    const rel = "_team/default/content/_bmad-output/story.md";
    expect(stripOrgRoot(joinOrgPath(orgRoot, rel), orgRoot)).toBe(rel);
  });

  it("tolerates a trailing slash on the root", () => {
    expect(stripOrgRoot("/root/a/b.md", "/root/")).toBe("a/b.md");
  });

  it("returns the empty string when the path IS the root", () => {
    expect(stripOrgRoot("/root", "/root")).toBe("");
  });

  it("returns null for a path outside the root or a null root", () => {
    expect(stripOrgRoot("/other/a.md", "/root")).toBeNull();
    expect(stripOrgRoot("/root-sibling/a.md", "/root")).toBeNull(); // prefix-but-not-segment
    expect(stripOrgRoot("/root/a.md", null)).toBeNull();
  });
});

describe("getFileRedline seam", () => {
  beforeEach(() => getFileRedlineMock.mockReset());

  it("delegates to the tauri command with (orgRoot, relPath)", async () => {
    getFileRedlineMock.mockResolvedValue({
      status: "modified",
      binary: false,
      before: "a",
      after: "b",
    });
    const r = await getFileRedline("/root", "a.md");
    expect(getFileRedlineMock).toHaveBeenCalledWith("/root", "a.md", undefined);
    expect(r.after).toBe("b");
  });

  it("forwards the pre-rename old path when given (SW-GS-14)", async () => {
    getFileRedlineMock.mockResolvedValue({
      status: "modified",
      binary: false,
      before: "a",
      after: "b",
    });
    await getFileRedline("/root", "new.md", "old.md");
    expect(getFileRedlineMock).toHaveBeenCalledWith("/root", "new.md", "old.md");
  });
});

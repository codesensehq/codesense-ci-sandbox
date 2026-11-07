// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { partitionPinnedNav, normalizePinPath, type NavPinMeta } from "./nav-pins";

/** Build a resolvable map from [path, isDir, label] triples. */
function resolvable(...entries: [string, boolean, string][]): Map<string, NavPinMeta> {
  return new Map(entries.map(([path, isDir, label]) => [path, { isDir, label }]));
}

describe("normalizePinPath", () => {
  it("trims, forward-slashes, and strips leading/trailing slashes", () => {
    expect(normalizePinPath("  a/b/  ")).toBe("a/b");
    expect(normalizePinPath("/a/b")).toBe("a/b");
    expect(normalizePinPath("a\\b\\c")).toBe("a/b/c");
    expect(normalizePinPath("///")).toBe("");
  });
});

describe("partitionPinnedNav", () => {
  it("merges team + personal into one list, tagged by scope", () => {
    const items = partitionPinnedNav(
      ["_bmad/epic-a"],
      ["_bmad/notes.md"],
      resolvable(["_bmad/epic-a", true, "epic-a"], ["_bmad/notes.md", false, "notes"]),
    );
    expect(items.map((i) => [i.path, i.scope, i.isDir])).toEqual([
      ["_bmad/epic-a", "team", true],
      ["_bmad/notes.md", "personal", false],
    ]);
  });

  it("dedups by path with TEAM winning — a personal pin cannot hide a team pin (AC-5)", () => {
    // Same path pinned both ways, in either declaration order → one team row.
    const a = partitionPinnedNav(
      ["shared/dir"],
      ["shared/dir"],
      resolvable(["shared/dir", true, "dir"]),
    );
    expect(a).toHaveLength(1);
    expect(a[0].scope).toBe("team");

    // Personal declared but team also present → still team, still one row.
    const b = partitionPinnedNav(
      ["shared/dir"],
      ["shared/dir", "shared/dir"],
      resolvable(["shared/dir", true, "dir"]),
    );
    expect(b).toHaveLength(1);
    expect(b[0].scope).toBe("team");
  });

  it("dedups duplicate paths differing only by slash shape", () => {
    const items = partitionPinnedNav(["a/b/", "/a/b"], ["a\\b"], resolvable(["a/b", true, "b"]));
    expect(items).toHaveLength(1);
    expect(items[0].path).toBe("a/b");
    expect(items[0].scope).toBe("team");
  });

  it("flags a pin that no longer resolves as stale, with a derived label (AC-7)", () => {
    const items = partitionPinnedNav(
      ["gone/epic", "live/dir"],
      [],
      resolvable(["live/dir", true, "dir"]),
    );
    const gone = items.find((i) => i.path === "gone/epic")!;
    expect(gone.stale).toBe(true);
    expect(gone.label).toBe("epic"); // derived from the last path segment
    expect(items.find((i) => i.path === "live/dir")!.stale).toBe(false);
  });

  it("derives a stale FILE pin's label by stripping the viewer extension", () => {
    const [item] = partitionPinnedNav(["notes/readme.md"], [], new Map());
    expect(item.stale).toBe(true);
    expect(item.label).toBe("readme");
    expect(item.isDir).toBe(false); // last segment has an extension → file
  });

  it("infers a stale DIR pin's type from the path (no extension → dir icon)", () => {
    const [item] = partitionPinnedNav(["some/epic-dir"], [], new Map());
    expect(item.stale).toBe(true);
    expect(item.isDir).toBe(true);
  });

  it("orders natural-alphabetically by label, dirs and files interleaved (AC-3)", () => {
    const items = partitionPinnedNav(
      ["z/epic-10", "z/epic-2"],
      ["a/file.md", "m/mid-dir"],
      resolvable(
        ["z/epic-10", true, "epic-10"],
        ["z/epic-2", true, "epic-2"],
        ["a/file.md", false, "file"],
        ["m/mid-dir", true, "mid-dir"],
      ),
    );
    // natural sort: epic-2 before epic-10; file/mid-dir interleaved by label
    expect(items.map((i) => i.label)).toEqual(["epic-2", "epic-10", "file", "mid-dir"]);
  });

  it("ignores empty / whitespace pin entries", () => {
    const items = partitionPinnedNav(
      ["  ", "///"],
      ["real/dir"],
      resolvable(["real/dir", true, "dir"]),
    );
    expect(items.map((i) => i.path)).toEqual(["real/dir"]);
  });
});

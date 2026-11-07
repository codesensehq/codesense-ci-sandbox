// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { resolveEpicDocPath } from "./resolve-epic-doc-path";

const DOC = "/ctx/content/epics/epic-x/epic-x.md";
const TREE_DOC = "/ctx/content/epics/epic-x/from-tree.md";

describe("resolveEpicDocPath (SW-EW-15)", () => {
  it("prefers the workspace record's epicDocPath over the tree", () => {
    const records = [{ epicDirPath: "epic-x", epicDocPath: DOC }];
    const tree = new Map([["epic-x", TREE_DOC]]);
    expect(resolveEpicDocPath(records, tree, "epic-x")).toBe(DOC);
  });

  it("falls back to the tree index when no record carries the path", () => {
    const tree = new Map([["epic-x", TREE_DOC]]);
    expect(resolveEpicDocPath([], tree, "epic-x")).toBe(TREE_DOC);
  });

  it("falls back to the tree when a record matches the dir but has no doc path", () => {
    const records = [{ epicDirPath: "epic-x", epicDocPath: undefined }];
    const tree = new Map([["epic-x", TREE_DOC]]);
    expect(resolveEpicDocPath(records, tree, "epic-x")).toBe(TREE_DOC);
  });

  it("throws on a genuine miss (no record, no tree entry)", () => {
    expect(() => resolveEpicDocPath([], new Map(), "epic-x")).toThrow(
      'No epic doc found for "epic-x"',
    );
  });

  it("ignores records for a different epic dir", () => {
    const records = [{ epicDirPath: "other-epic", epicDocPath: DOC }];
    expect(() => resolveEpicDocPath(records, new Map(), "epic-x")).toThrow();
  });

  it("returns the FIRST matching record (callers pass the active workspace first)", () => {
    // Two epics share the leaf "foo"; whichever the caller lists first wins —
    // the view passes the active workspace first so the card resolves to it.
    const active = { epicDirPath: "foo", epicDocPath: "/ctx/active/foo/epic.md" };
    const other = { epicDirPath: "foo", epicDocPath: "/ctx/other/foo/epic.md" };
    expect(resolveEpicDocPath([active, other], new Map(), "foo")).toBe(active.epicDocPath);
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { findEnclosingEpic } from "./find-enclosing-epic";
import { buildTree } from "./test-fixtures/derived-tree";
import type { MainDoc } from "./types";

const mainDoc = (filePath: string): MainDoc => ({
  filePath,
  supportingDocs: null,
  purposeText: null,
});

describe("findEnclosingEpic (SW-SC-12)", () => {
  it("returns the deepest enclosing epic for a canvas nested in a sub-epic", () => {
    // root (epic) > shared-canvas (epic) > canvas
    const subEpic = buildTree("shared-canvas", {
      absoluteDirPath: "/ctx/epic/shared-canvas",
      mainDoc: mainDoc("/ctx/epic/shared-canvas/README.md"),
    });
    const root = buildTree("epic", {
      absoluteDirPath: "/ctx/epic",
      mainDoc: mainDoc("/ctx/epic/README.md"),
      subEpics: [subEpic],
    });

    const found = findEnclosingEpic(root, "/ctx/epic/shared-canvas/design.excalidraw");
    expect(found.absoluteDirPath).toBe("/ctx/epic/shared-canvas");
  });

  it("returns the nearest epic ancestor when the canvas dir is not itself an epic", () => {
    // root (epic) > notes (plain dir, no mainDoc) > canvas
    const plainDir = buildTree("notes", {
      absoluteDirPath: "/ctx/epic/notes",
      mainDoc: null,
    });
    const root = buildTree("epic", {
      absoluteDirPath: "/ctx/epic",
      mainDoc: mainDoc("/ctx/epic/README.md"),
      subEpics: [plainDir],
    });

    const found = findEnclosingEpic(root, "/ctx/epic/notes/design.excalidraw");
    expect(found.absoluteDirPath).toBe("/ctx/epic");
  });

  it("falls back to the tree when no enclosing epic exists (drilled below the epic)", () => {
    // tree root is a plain dir (drilled below the epic boundary).
    const root = buildTree("plain", {
      absoluteDirPath: "/ctx/plain",
      mainDoc: null,
    });

    const found = findEnclosingEpic(root, "/ctx/plain/design.excalidraw");
    expect(found).toBe(root);
  });

  it("falls back to the tree when the canvas sits outside the tree", () => {
    const root = buildTree("epic", {
      absoluteDirPath: "/ctx/epic",
      mainDoc: mainDoc("/ctx/epic/README.md"),
    });

    const found = findEnclosingEpic(root, "/somewhere/else/design.excalidraw");
    expect(found).toBe(root);
  });

  it("does not match a sibling whose path is a string-prefix (separator guard)", () => {
    // `/ctx/epic-foo` must not swallow a canvas under `/ctx/epic-foobar`.
    const epicFoo = buildTree("epic-foo", {
      absoluteDirPath: "/ctx/epic-foo",
      mainDoc: mainDoc("/ctx/epic-foo/README.md"),
    });
    const epicFoobar = buildTree("epic-foobar", {
      absoluteDirPath: "/ctx/epic-foobar",
      mainDoc: mainDoc("/ctx/epic-foobar/README.md"),
    });
    const root = buildTree("root", {
      absoluteDirPath: "/ctx",
      subEpics: [epicFoo, epicFoobar],
    });

    const found = findEnclosingEpic(root, "/ctx/epic-foobar/design.excalidraw");
    expect(found.absoluteDirPath).toBe("/ctx/epic-foobar");
  });

  it("resolves a canvas directly inside the root epic to the root", () => {
    const root = buildTree("epic", {
      absoluteDirPath: "/ctx/epic",
      mainDoc: mainDoc("/ctx/epic/README.md"),
    });

    const found = findEnclosingEpic(root, "/ctx/epic/design.excalidraw");
    expect(found).toBe(root);
  });
});

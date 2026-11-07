// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type { FileEntry } from "@/tauri/commands";
import type { StoryRef } from "@/shared/lib/pipeline-graph/types";

import type { FlatNavItem } from "./flatten-nav-tree";
import {
  dragSourcePath,
  relatableTargetPath,
  underRoot,
  isValidRelateDrop,
} from "./nav-relate-dnd";

const ROOT = "/ctx/content";

const storyItem = (filePath: string): FlatNavItem => ({
  type: "story",
  story: { filePath, fileName: filePath.split("/").pop() } as StoryRef,
  depth: 0,
  isReady: true,
  blockedBy: [],
});
const docItem = (filePath: string, installed = false): FlatNavItem => ({
  type: "doc",
  filePath,
  label: filePath.split("/").pop() ?? "",
  depth: 0,
  installed,
});
const mainDocItem = (filePath: string, installed = false): FlatNavItem => ({
  type: "main-doc",
  filePath,
  label: "README.md",
  depth: 0,
  installed,
});
const genericFileItem = (path: string): FlatNavItem => ({
  type: "generic-file",
  entry: { path, name: path.split("/").pop() ?? "", is_dir: false } as FileEntry,
  depth: 0,
});
const folderItem = (): FlatNavItem => ({
  type: "generic-dir",
  entry: { path: "/ctx/content/dir", name: "dir", is_dir: true } as FileEntry,
  depth: 0,
  expanded: false,
});

describe("dragSourcePath", () => {
  it("returns the path for every leaf file row (any file can be dragged)", () => {
    expect(dragSourcePath(storyItem("/ctx/content/a.md"))).toBe("/ctx/content/a.md");
    expect(dragSourcePath(docItem("/ctx/content/b.md"))).toBe("/ctx/content/b.md");
    expect(dragSourcePath(mainDocItem("/ctx/content/README.md"))).toBe("/ctx/content/README.md");
    expect(dragSourcePath(genericFileItem("/ctx/content/pic.png"))).toBe("/ctx/content/pic.png");
    // an installed (browse-only source) doc is still a valid drag SOURCE
    expect(dragSourcePath(docItem("/ctx/content/src.md", true))).toBe("/ctx/content/src.md");
  });
  it("returns null for folders (not draggable)", () => {
    expect(dragSourcePath(folderItem())).toBeNull();
  });
});

describe("relatableTargetPath", () => {
  it("returns the path for a story / non-installed doc / non-installed main-doc", () => {
    expect(relatableTargetPath(storyItem("/ctx/content/a.md"))).toBe("/ctx/content/a.md");
    expect(relatableTargetPath(docItem("/ctx/content/b.md"))).toBe("/ctx/content/b.md");
    expect(relatableTargetPath(mainDocItem("/ctx/content/README.md"))).toBe(
      "/ctx/content/README.md",
    );
  });
  it("returns null for an installed (browse-only) doc / main-doc — nowhere to write", () => {
    expect(relatableTargetPath(docItem("/ctx/content/src.md", true))).toBeNull();
    expect(relatableTargetPath(mainDocItem("/ctx/content/README.md", true))).toBeNull();
  });
  it("returns null for a generic file and folders (no own frontmatter)", () => {
    expect(relatableTargetPath(genericFileItem("/ctx/content/pic.png"))).toBeNull();
    expect(relatableTargetPath(folderItem())).toBeNull();
  });
});

describe("underRoot", () => {
  it("accepts a path under the root or equal to it", () => {
    expect(underRoot("/ctx/content/a.md", ROOT)).toBe(true);
    expect(underRoot(ROOT, ROOT)).toBe(true);
  });
  it("rejects a path outside the root and an empty root", () => {
    expect(underRoot("/other/x.md", ROOT)).toBe(false);
    expect(underRoot("/ctx/content-sibling/x.md", ROOT)).toBe(false); // prefix but not a child
    expect(underRoot("/ctx/content/a.md", "")).toBe(false);
  });
});

describe("isValidRelateDrop", () => {
  it("accepts a distinct same-root drag→target (AC-4)", () => {
    expect(isValidRelateDrop("/ctx/content/a.md", "/ctx/content/b.md", ROOT)).toBe(true);
  });
  it("rejects a self-drop (AC-5)", () => {
    expect(isValidRelateDrop("/ctx/content/a.md", "/ctx/content/a.md", ROOT)).toBe(false);
  });
  it("rejects a cross-root drag (source outside the team root) (AC-5)", () => {
    expect(isValidRelateDrop("/other/src.md", "/ctx/content/b.md", ROOT)).toBe(false);
  });
  it("rejects when either side is null/empty", () => {
    expect(isValidRelateDrop(null, "/ctx/content/b.md", ROOT)).toBe(false);
    expect(isValidRelateDrop("/ctx/content/a.md", null, ROOT)).toBe(false);
    expect(isValidRelateDrop(undefined, "/ctx/content/b.md", ROOT)).toBe(false);
  });
});

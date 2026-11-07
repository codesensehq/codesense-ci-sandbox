// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  navDropDirPath,
  isInternalMoveToDir,
  pickDropDir,
  type DropCandidate,
} from "./nav-file-drop";
import type { FlatNavItem } from "./flatten-nav-tree";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import type { FileEntry } from "@/tauri/commands";

// Minimal builders — only the fields the helpers read.
const folderHeader = (absoluteDirPath: string): FlatNavItem =>
  ({
    type: "folder-header",
    node: { absoluteDirPath } as DerivedPipelineTree,
    depth: 0,
    expanded: false,
    total: 0,
    done: 0,
  }) as FlatNavItem;

const genericDir = (path: string): FlatNavItem =>
  ({
    type: "generic-dir",
    entry: { path, name: path.split("/").pop() ?? "", is_dir: true } as FileEntry,
    depth: 0,
    expanded: false,
  }) as FlatNavItem;

const docRow = (filePath: string): FlatNavItem =>
  ({ type: "doc", filePath, label: "d", depth: 0, installed: false }) as FlatNavItem;

describe("navDropDirPath", () => {
  it("returns the absolute path for a folder-header dir row", () => {
    expect(navDropDirPath(folderHeader("/c/_config"))).toBe("/c/_config");
  });

  it("returns the entry path for a generic-dir row", () => {
    expect(navDropDirPath(genericDir("/c/scripts"))).toBe("/c/scripts");
  });

  it("excludes browse-only installed:// source dirs (not a drop target)", () => {
    expect(navDropDirPath(folderHeader("installed://owner/repo/skills"))).toBeNull();
    expect(navDropDirPath(genericDir("installed://owner/repo/x"))).toBeNull();
  });

  it("returns null for file/story rows (not directory drop targets)", () => {
    expect(navDropDirPath(docRow("/c/a.md"))).toBeNull();
  });
});

describe("isInternalMoveToDir", () => {
  it("accepts a file moving into a different directory", () => {
    expect(isInternalMoveToDir("/c/a/x.md", "/c/b")).toBe(true);
  });

  it("rejects a file already directly inside the target dir (no-op)", () => {
    expect(isInternalMoveToDir("/c/b/x.md", "/c/b")).toBe(false);
  });

  it("rejects dropping a dir onto itself", () => {
    expect(isInternalMoveToDir("/c/b", "/c/b")).toBe(false);
  });

  it("rejects when there is no in-flight internal path (an OS drag)", () => {
    expect(isInternalMoveToDir(null, "/c/b")).toBe(false);
  });

  it("rejects when there is no target dir", () => {
    expect(isInternalMoveToDir("/c/a/x.md", null)).toBe(false);
  });
});

describe("pickDropDir", () => {
  const rect = (top: number, bottom: number): DOMRect =>
    ({ left: 0, top, right: 200, bottom }) as DOMRect;
  const candidates: DropCandidate[] = [
    { dir: "/c/first", rect: rect(0, 24) },
    { dir: "/c/second", rect: rect(24, 48) },
  ];

  it("returns the dir whose row rect contains the position", () => {
    expect(pickDropDir({ x: 10, y: 12 }, candidates)).toBe("/c/first");
    expect(pickDropDir({ x: 10, y: 36 }, candidates)).toBe("/c/second");
  });

  it("returns null when the position is over no directory row (→ root zone)", () => {
    expect(pickDropDir({ x: 10, y: 400 }, candidates)).toBeNull();
  });

  it("is half-open on the row seam (a drop on y=24 resolves to the second row)", () => {
    expect(pickDropDir({ x: 10, y: 24 }, candidates)).toBe("/c/second");
  });

  it("compares position directly to row rects (no devicePixelRatio scaling)", () => {
    // A drop deep in a lower row must not be down-scaled into the top row.
    const rows: DropCandidate[] = [
      { dir: "/c/top", rect: { left: 0, top: 0, right: 400, bottom: 300 } as DOMRect },
      { dir: "/c/bottom", rect: { left: 0, top: 300, right: 400, bottom: 900 } as DOMRect },
    ];
    expect(pickDropDir({ x: 100, y: 666 }, rows)).toBe("/c/bottom");
  });
});

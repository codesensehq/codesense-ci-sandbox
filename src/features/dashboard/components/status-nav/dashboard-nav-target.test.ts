// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { dashboardNavTarget } from "./dashboard-nav-target";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
const story = buildStoryRef("story.md", { absolutePath: "/team/story.md" });
const node = buildTree("epic", {
  absoluteDirPath: "/team/epic",
  dirPath: "epic",
  mainDoc: { filePath: "/team/epic/README.md", supportingDocs: null, purposeText: null },
});
const items: FlatNavItem[] = [
  { type: "story", story, depth: 0, isReady: true, blockedBy: [] },
  { type: "folder-header", node, depth: 0, expanded: false, total: 1, done: 0 },
  { type: "doc", filePath: "/team/file.html", label: "File", depth: 0, installed: false },
];
it("resolves resident nav rows and preserves the native epic arguments", () => {
  expect(dashboardNavTarget({ kind: "story", path: "story.md" }, "/team", items)).toEqual({
    path: story.filePath,
  });
  expect(dashboardNavTarget({ kind: "epic", path: "epic" }, "/team", items)).toEqual({
    path: "/team/epic",
    epicDirPath: "epic",
    epicFilePath: "/team/epic/README.md",
    isEpic: true,
  });
  expect(dashboardNavTarget({ kind: "file", path: "file.html" }, "/team", items)).toEqual({
    path: "/team/file.html",
  });
  for (const path of ["../story.md", "other-team/story.md", "deleted.md"])
    expect(dashboardNavTarget({ kind: "story", path }, "/team", items)).toBeNull();
  expect(dashboardNavTarget({ kind: "story", path: "story.md" }, "/team", [])).toBeNull();
});

it("resolves a listed absolute path and own generic files without mounting their native rows", () => {
  expect(dashboardNavTarget({ kind: "story", path: "/team/story.md" }, "/team", items)).toEqual({
    path: story.filePath,
  });
  expect(dashboardNavTarget({ kind: "file", path: "/team/deep/image.png" }, "/team", [])).toEqual({
    path: "/team/deep/image.png",
  });
  expect(dashboardNavTarget({ kind: "file", path: "/other/secret.md" }, "/team", [])).toBeNull();
});

it("uses the native epic menu for an epic's listed main document", () => {
  expect(
    dashboardNavTarget({ kind: "epic", path: "/team/epic/README.md" }, "/team", items),
  ).toEqual({
    path: "/team/epic",
    epicDirPath: "epic",
    epicFilePath: "/team/epic/README.md",
    isEpic: true,
  });
});

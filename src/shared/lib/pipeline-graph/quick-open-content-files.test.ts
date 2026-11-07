// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { quickOpenContentFiles } from "./quick-open-content-files";
import { buildTree } from "./test-fixtures/derived-tree";
it("keeps real inherited paths, nearer/own winners and own non-nav code files", () => {
  const own = [
    { name: "config.yml", relativePath: "config.yml", fullPath: "/child/config.yml" },
    { name: "guide.md", relativePath: "docs/guide.md", fullPath: "/child/docs/guide.md" },
  ];
  const tree = buildTree("", {
    subEpics: [
      buildTree("docs", {
        dirPath: "docs",
        docs: [
          { fileName: "guide.md", filePath: "/child/docs/guide.md" },
          { fileName: "policy.md", filePath: "/parent/docs/policy.md", sourceTeam: "parent" },
        ],
      }),
    ],
  });
  expect(quickOpenContentFiles(own, tree).map((f) => f.fullPath)).toEqual([
    "/child/config.yml",
    "/child/docs/guide.md",
    "/parent/docs/policy.md",
  ]);
});

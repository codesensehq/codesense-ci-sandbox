// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { isReadOnlyLayeredDoc } from "./is-read-only-layered-doc";
import type { DerivedPipelineTree, DocRef } from "@/shared/lib/pipeline-graph/types";

function doc(filePath: string, extra: Partial<DocRef> = {}): DocRef {
  return { fileName: filePath.split("/").pop() ?? "", filePath, ...extra };
}

function tree(docs: DocRef[], subEpics: DerivedPipelineTree[] = []): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "/root",
    stories: [],
    docs,
    subEpics,
    orderError: null,
    mainDoc: null,
  };
}

describe("isReadOnlyLayeredDoc (AC-13)", () => {
  it("is read-only for an installed source-repo layer winner", () => {
    const t = tree([doc("/root/a.md", { installedSourceRepo: "acme/api" })]);
    expect(isReadOnlyLayeredDoc(t, "/root/a.md")).toBe(true);
  });

  it("is editable for a plain team-workspace doc", () => {
    const t = tree([doc("/root/a.md")]);
    expect(isReadOnlyLayeredDoc(t, "/root/a.md")).toBe(false);
  });

  it("recurses into sub-epics", () => {
    const t = tree([], [tree([doc("/root/sub/b.md", { installedSourceRepo: "acme/api" })])]);
    expect(isReadOnlyLayeredDoc(t, "/root/sub/b.md")).toBe(true);
  });

  it("returns false for unknown paths / null tree", () => {
    expect(isReadOnlyLayeredDoc(null, "/root/a.md")).toBe(false);
    expect(isReadOnlyLayeredDoc(tree([]), "/root/missing.md")).toBe(false);
  });
});

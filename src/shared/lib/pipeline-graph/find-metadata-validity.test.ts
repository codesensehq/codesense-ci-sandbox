// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { findMetadataValidity } from "./find-metadata-validity";
import type { DerivedPipelineTree, DocRef, StoryRef } from "./types";

function baseTree(over: Partial<DerivedPipelineTree> = {}): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "/root",
    stories: [],
    docs: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...over,
  };
}

function story(filePath: string, over: Partial<StoryRef> = {}): StoryRef {
  return {
    fileName: filePath.split("/").pop() ?? "",
    filePath,
    dependsOn: null,
    status: null,
    documentType: "story",
    supportingDocs: null,
    sourceRepo: null,
    team: null,
    ...over,
  };
}

function doc(filePath: string, over: Partial<DocRef> = {}): DocRef {
  return { fileName: filePath.split("/").pop() ?? "", filePath, ...over };
}

describe("findMetadataValidity", () => {
  it("reads an invalid story's flag + reason", () => {
    const t = baseTree({
      stories: [
        story("/root/s.md", {
          metadataValid: false,
          metadataInvalidReason: "source_repo 'acme/old' is not a configured repo",
        }),
      ],
    });
    expect(findMetadataValidity(t, "/root/s.md")).toEqual({
      metadataValid: false,
      metadataInvalidReason: "source_repo 'acme/old' is not a configured repo",
    });
  });

  it("defaults to valid when the flag is absent", () => {
    const t = baseTree({ stories: [story("/root/s.md")] });
    expect(findMetadataValidity(t, "/root/s.md")?.metadataValid).toBe(true);
  });

  it("reads a plain doc's flag (AC-16)", () => {
    const t = baseTree({ docs: [doc("/root/d.md", { metadataValid: false })] });
    expect(findMetadataValidity(t, "/root/d.md")?.metadataValid).toBe(false);
  });

  it("recurses into sub-epics and returns null for unknown paths", () => {
    const t = baseTree({
      subEpics: [baseTree({ stories: [story("/root/sub/x.md", { metadataValid: false })] })],
    });
    expect(findMetadataValidity(t, "/root/sub/x.md")?.metadataValid).toBe(false);
    expect(findMetadataValidity(t, "/root/nope.md")).toBeNull();
  });
});

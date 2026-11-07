// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  abbreviateStem,
  stripPipelineExtension,
  normalizeStatus,
  normalizeDocumentType,
  isActionableDocumentType,
  findScopedSubtree,
  stripDotSlash,
  resolveScopeRelativePath,
  scopeRelative,
  ACTIONABLE_DOCUMENT_TYPES,
  KNOWN_DOCUMENT_TYPES,
  docTypeFromFileName,
} from "./pipeline-utils";
import { mergePipelineTrees } from "./merge-pipeline-trees";
import { buildStoryRef, buildTree } from "./test-fixtures/derived-tree";

describe("abbreviateStem", () => {
  it("abbreviates standard story stems", () => {
    expect(abbreviateStem("W-5-source-repo")).toBe("W.5");
    expect(abbreviateStem("F-12-workspace-tabs")).toBe("F.12");
  });

  it("returns stem unchanged when no prefix match", () => {
    expect(abbreviateStem("no-prefix")).toBe("no-prefix");
  });

  it("returns empty string unchanged", () => {
    expect(abbreviateStem("")).toBe("");
  });

  it("handles multi-letter prefixes", () => {
    expect(abbreviateStem("ABC-99-something")).toBe("ABC.99");
  });
});

describe("stripPipelineExtension", () => {
  it("strips .md suffix", () => {
    expect(stripPipelineExtension("story.md")).toBe("story");
  });

  it("strips .MD suffix (case-insensitive)", () => {
    expect(stripPipelineExtension("story.MD")).toBe("story");
  });

  it("strips .Md suffix (mixed case)", () => {
    expect(stripPipelineExtension("story.Md")).toBe("story");
  });

  it("strips .html suffix", () => {
    expect(stripPipelineExtension("story.html")).toBe("story");
  });

  it("strips .htm suffix", () => {
    expect(stripPipelineExtension("story.htm")).toBe("story");
  });

  it("returns name unchanged when no pipeline extension", () => {
    expect(stripPipelineExtension("story")).toBe("story");
    expect(stripPipelineExtension("story.txt")).toBe("story.txt");
  });

  it("only strips trailing extension, not mid-string", () => {
    expect(stripPipelineExtension("my.md.file.md")).toBe("my.md.file");
  });

  it("handles empty string", () => {
    expect(stripPipelineExtension("")).toBe("");
  });
});

describe("normalizeStatus", () => {
  // SW-GT-3: null/empty/unknown now fall back to the taxonomy's first state by
  // `order` — for the default seed that is `planning` (order 0), not `todo`.
  it("returns the taxonomy default (planning) for null", () => {
    expect(normalizeStatus(null)).toBe("planning");
  });

  it("returns the taxonomy default (planning) for empty string", () => {
    expect(normalizeStatus("")).toBe("planning");
  });

  it("returns valid status as-is: in-progress", () => {
    expect(normalizeStatus("in-progress")).toBe("in-progress");
  });

  it("returns valid status as-is: done", () => {
    expect(normalizeStatus("done")).toBe("done");
  });

  it("returns valid status as-is: planning", () => {
    expect(normalizeStatus("planning")).toBe("planning");
  });

  it("returns valid status as-is: todo", () => {
    expect(normalizeStatus("todo")).toBe("todo");
  });

  it("returns valid status as-is: blocked", () => {
    expect(normalizeStatus("blocked")).toBe("blocked");
  });

  it("returns valid status as-is: review", () => {
    expect(normalizeStatus("review")).toBe("review");
  });

  it("returns valid status as-is: qa", () => {
    expect(normalizeStatus("qa")).toBe("qa");
  });

  it("returns the taxonomy default (planning) for unknown status string", () => {
    expect(normalizeStatus("invalid-status")).toBe("planning");
  });

  it("is case-sensitive (uppercase is unknown → taxonomy default)", () => {
    expect(normalizeStatus("Done")).toBe("planning");
  });
});

describe("KNOWN_DOCUMENT_TYPES", () => {
  it("contains all known document types (incl. SW-SC-2 canvas, SW-SC-7 image)", () => {
    expect(KNOWN_DOCUMENT_TYPES).toEqual(
      new Set(["story", "bug", "spike", "doc", "retro", "canvas", "image"]),
    );
  });
});

describe("docTypeFromFileName", () => {
  it("maps image extensions to 'image' (SW-SC-7)", () => {
    expect(docTypeFromFileName("shot.png")).toBe("image");
    expect(docTypeFromFileName("MOCKUP.JPG")).toBe("image");
    expect(docTypeFromFileName("diagram.svg")).toBe("image");
  });
  it("maps .excalidraw to 'canvas' (SW-SC-2)", () => {
    expect(docTypeFromFileName("design.excalidraw")).toBe("canvas");
  });
  it("defaults everything else to 'doc'", () => {
    expect(docTypeFromFileName("notes.md")).toBe("doc");
    expect(docTypeFromFileName("page.html")).toBe("doc");
    expect(docTypeFromFileName("noext")).toBe("doc");
  });
});

describe("ACTIONABLE_DOCUMENT_TYPES", () => {
  it("contains only actionable types", () => {
    expect(ACTIONABLE_DOCUMENT_TYPES).toEqual(new Set(["story", "bug", "spike"]));
  });

  it("excludes doc and retro", () => {
    expect(ACTIONABLE_DOCUMENT_TYPES.has("doc")).toBe(false);
    expect(ACTIONABLE_DOCUMENT_TYPES.has("retro")).toBe(false);
  });
});

describe("normalizeDocumentType", () => {
  it("returns 'story' for null", () => {
    expect(normalizeDocumentType(null)).toBe("story");
  });

  it("returns 'story' for empty string", () => {
    expect(normalizeDocumentType("")).toBe("story");
  });

  it("returns 'story' for whitespace-only string", () => {
    expect(normalizeDocumentType("   ")).toBe("story");
  });

  it("returns known types as-is (lowercase)", () => {
    expect(normalizeDocumentType("story")).toBe("story");
    expect(normalizeDocumentType("bug")).toBe("bug");
    expect(normalizeDocumentType("spike")).toBe("spike");
    expect(normalizeDocumentType("doc")).toBe("doc");
    expect(normalizeDocumentType("retro")).toBe("retro");
  });

  it("normalizes case to lowercase", () => {
    expect(normalizeDocumentType("Bug")).toBe("bug");
    expect(normalizeDocumentType("SPIKE")).toBe("spike");
    expect(normalizeDocumentType("Doc")).toBe("doc");
  });

  it("trims whitespace", () => {
    expect(normalizeDocumentType("  bug  ")).toBe("bug");
  });

  it("returns 'doc' for unknown types", () => {
    expect(normalizeDocumentType("meeting")).toBe("doc");
    expect(normalizeDocumentType("notes")).toBe("doc");
    expect(normalizeDocumentType("unknown")).toBe("doc");
  });
});

describe("isActionableDocumentType", () => {
  it("returns true for null (defaults to story)", () => {
    expect(isActionableDocumentType(null)).toBe(true);
  });

  it("returns true for empty string (defaults to story)", () => {
    expect(isActionableDocumentType("")).toBe(true);
  });

  it("returns true for actionable types", () => {
    expect(isActionableDocumentType("story")).toBe(true);
    expect(isActionableDocumentType("bug")).toBe(true);
    expect(isActionableDocumentType("spike")).toBe(true);
  });

  it("returns false for non-actionable types", () => {
    expect(isActionableDocumentType("doc")).toBe(false);
    expect(isActionableDocumentType("retro")).toBe(false);
  });

  it("returns false for unknown types (fallback to doc)", () => {
    expect(isActionableDocumentType("meeting")).toBe(false);
    expect(isActionableDocumentType("notes")).toBe(false);
  });

  it("handles case-insensitive input", () => {
    expect(isActionableDocumentType("Bug")).toBe(true);
    expect(isActionableDocumentType("DOC")).toBe(false);
  });
});

describe("findScopedSubtree (M.4 — merged tree)", () => {
  const merged = mergePipelineTrees([
    {
      repoSlug: "acme/api",
      tree: buildTree("", {
        subEpics: [
          buildTree("in-progress", {
            dirPath: "in-progress",
            stories: [buildStoryRef("s1.md")],
          }),
        ],
      }),
    },
    {
      repoSlug: "acme/web",
      tree: buildTree("", {
        stories: [buildStoryRef("s2.md")],
      }),
    },
  ]);

  it("returns the full merged tree for empty scope", () => {
    const result = findScopedSubtree(merged, "");
    expect(result).toBe(merged);
  });

  it("drills into a repo-level group", () => {
    const result = findScopedSubtree(merged, "acme/api");
    expect(result).not.toBeNull();
    expect(result!.dirPath).toBe("acme/api");
  });

  it("drills into a nested epic within a repo group", () => {
    const result = findScopedSubtree(merged, "acme/api/in-progress");
    expect(result).not.toBeNull();
    expect(result!.dirPath).toBe("acme/api/in-progress");
    expect(result!.stories).toHaveLength(1);
  });

  it("returns null for non-existent scope path", () => {
    const result = findScopedSubtree(merged, "acme/nonexistent");
    expect(result).toBeNull();
  });
});

describe("stripDotSlash", () => {
  it("strips leading ./", () => {
    expect(stripDotSlash("./bug.md")).toBe("bug.md");
  });

  it("leaves bare refs unchanged", () => {
    expect(stripDotSlash("bug.md")).toBe("bug.md");
  });

  it("leaves refs with / path unchanged", () => {
    expect(stripDotSlash("../epic/story.md")).toBe("../epic/story.md");
  });

  it("handles empty string", () => {
    expect(stripDotSlash("")).toBe("");
  });

  it("only strips leading ./, not mid-string ./", () => {
    expect(stripDotSlash("a/./b.md")).toBe("a/./b.md");
  });
});

describe("resolveScopeRelativePath", () => {
  it("resolves .. by popping one segment", () => {
    expect(resolveScopeRelativePath("epic-a/sub1", "..")).toBe("epic-a");
  });

  it("resolves . by skipping", () => {
    expect(resolveScopeRelativePath("epic-a", "./sub1")).toBe("epic-a/sub1");
  });

  it("resolves multiple .. pops", () => {
    expect(resolveScopeRelativePath("a/b/c", "../../d")).toBe("a/d");
  });

  it("returns null when .. over-pops past scope root", () => {
    expect(resolveScopeRelativePath("epic-a", "../..")).toBeNull();
  });

  it("returns null when .. over-pops from empty base", () => {
    expect(resolveScopeRelativePath("", "..")).toBeNull();
  });

  it("handles empty base with simple relative", () => {
    expect(resolveScopeRelativePath("", "auth")).toBe("auth");
  });

  it("handles pure relative path from empty base", () => {
    expect(resolveScopeRelativePath("", "a/b")).toBe("a/b");
  });

  it("appends simple relative to base", () => {
    expect(resolveScopeRelativePath("epic-a", "sub1")).toBe("epic-a/sub1");
  });
});

describe("scopeRelative", () => {
  it("returns dirPath when scope is empty", () => {
    expect(scopeRelative("nav", "")).toBe("nav");
  });

  it("returns empty string when dirPath equals scope", () => {
    expect(scopeRelative("in-progress", "in-progress")).toBe("");
  });

  it("strips scope prefix from descendant dirPath", () => {
    expect(scopeRelative("in-progress/nav", "in-progress")).toBe("nav");
  });

  it("handles nested descendant", () => {
    expect(scopeRelative("in-progress/nav/sub", "in-progress")).toBe("nav/sub");
  });
});

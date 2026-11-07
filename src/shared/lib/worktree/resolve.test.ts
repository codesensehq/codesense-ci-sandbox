// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  epicCodeFromTitle,
  extractFrontmatterBlock,
  isStoryFile,
  normalizeStoryFilePath,
  parseBranchNameFromFrontmatter,
  parseDependsOnFromFrontmatter,
  parseSourceRepoFromFrontmatter,
  parseStoryIdFromFrontmatter,
  parseStoryTitle,
  prettifyDirLeaf,
  resolveBranchName,
  resolveWorktreeParams,
  resolveWorktreePath,
  storyDisplayLabel,
  storyIdFromPath,
  workspaceDisplayLabel,
} from "./resolve";

vi.mock("@/tauri/commands", () => ({
  readFile: vi.fn(),
}));

import { readFile } from "@/tauri/commands";

const readFileMock = readFile as unknown as ReturnType<typeof vi.fn>;

beforeEach(() => {
  readFileMock.mockReset();
});

describe("storyIdFromPath", () => {
  it("strips the .md suffix and directory", () => {
    expect(storyIdFromPath("/repo/stories/30-10-story-tab-workspaces.md")).toBe(
      "30-10-story-tab-workspaces",
    );
  });
  it("works with no extension", () => {
    expect(storyIdFromPath("/repo/stories/30-10")).toBe("30-10");
  });
});

describe("parseBranchNameFromFrontmatter", () => {
  it("returns null when there is no frontmatter block", () => {
    expect(parseBranchNameFromFrontmatter("# Just a story\n")).toBeNull();
  });
  it("returns null when frontmatter exists but no branch_name field", () => {
    expect(parseBranchNameFromFrontmatter("---\nstatus: ready\n---\n")).toBeNull();
  });
  it("returns the bare value", () => {
    expect(parseBranchNameFromFrontmatter("---\nbranch_name: feat/foo\n---\n")).toBe("feat/foo");
  });
  it("strips matched double quotes", () => {
    expect(parseBranchNameFromFrontmatter('---\nbranch_name: "feat/foo"\n---\n')).toBe("feat/foo");
  });
  it("strips matched single quotes", () => {
    expect(parseBranchNameFromFrontmatter("---\nbranch_name: 'feat/foo'\n---\n")).toBe("feat/foo");
  });
  it("returns null for empty value", () => {
    expect(parseBranchNameFromFrontmatter("---\nbranch_name:    \n---\n")).toBeNull();
  });
  it("handles CRLF line endings", () => {
    expect(parseBranchNameFromFrontmatter("---\r\nbranch_name: feat/foo\r\n---\r\n")).toBe(
      "feat/foo",
    );
  });
  // SW-OR-1b: canonical `branch` supersedes legacy fields (precedence
  // branch → branch_name → dev_branch), matching backend `story_branch()`.
  it("prefers canonical `branch` over legacy fields", () => {
    expect(
      parseBranchNameFromFrontmatter(
        "---\nbranch: feat/canonical\nbranch_name: feat/intended\ndev_branch: feat/observed\n---\n",
      ),
    ).toBe("feat/canonical");
  });
  it("reads the unified `branch` when it is the only branch field", () => {
    expect(parseBranchNameFromFrontmatter("---\nbranch: feat/only\n---\n")).toBe("feat/only");
  });
  it("falls back to dev_branch when branch and branch_name are absent", () => {
    expect(parseBranchNameFromFrontmatter("---\ndev_branch: feat/legacy\n---\n")).toBe(
      "feat/legacy",
    );
  });
  it("does not mistake branch_name for the canonical branch", () => {
    // A doc with only `branch_name` still resolves it (the `branch:` regex is exact).
    expect(parseBranchNameFromFrontmatter("---\nbranch_name: feat/intended\n---\n")).toBe(
      "feat/intended",
    );
  });
  it("a whitespace-only branch does not swallow the next line; falls through", () => {
    // Regression: a whitespace-only value must yield null (not capture the NEXT
    // frontmatter line), so precedence falls through to the legacy field.
    expect(
      parseBranchNameFromFrontmatter("---\nbranch:    \nbranch_name: feat/legacy\n---\n"),
    ).toBe("feat/legacy");
  });
});

describe("resolveBranchName", () => {
  it("reads from frontmatter when present", async () => {
    readFileMock.mockResolvedValue("---\nbranch_name: feat/custom\n---\n");
    expect(await resolveBranchName("/repo/stories/30-10.md")).toBe("feat/custom");
  });
  it("falls back to default convention when frontmatter is missing", async () => {
    readFileMock.mockResolvedValue("# bare story\n");
    expect(await resolveBranchName("/repo/stories/30-10-foo.md")).toBe("feat/30-10-foo");
  });
  it("uses config.branch_prefix in the fallback", async () => {
    readFileMock.mockResolvedValue("# bare story\n");
    expect(
      await resolveBranchName("/repo/stories/30-10.md", {
        version: 1,
        setup: [],
        branch_prefix: "story/",
      }),
    ).toBe("story/30-10");
  });
  it("falls back to convention when file read throws", async () => {
    readFileMock.mockRejectedValue(new Error("ENOENT"));
    expect(await resolveBranchName("/repo/missing.md")).toBe("feat/missing");
  });
});

describe("resolveWorktreePath", () => {
  it("applies the default {repo}-wt/{slug} template at the repo's parent dir", () => {
    expect(resolveWorktreePath(null, "/home/u/codesense", "feat/30-10")).toBe(
      "/home/u/codesense-wt/feat-30-10",
    );
  });
  it("collapses slashes in the branch name into the slug placeholder", () => {
    expect(resolveWorktreePath(null, "/home/u/codesense", "feat/foo/bar")).toBe(
      "/home/u/codesense-wt/feat-foo-bar",
    );
  });
  it("honours an absolute worktree_dir template", () => {
    expect(
      resolveWorktreePath(
        {
          version: 1,
          setup: [],
          worktree_dir: "/wt/{repo}/{slug}",
        },
        "/home/u/codesense",
        "feat/30-10",
      ),
    ).toBe("/wt/codesense/feat-30-10");
  });
});

describe("storyDisplayLabel", () => {
  it("collapses a full stem to the numeric prefix", () => {
    expect(storyDisplayLabel("30-10-story-tab-workspaces")).toBe("30-10");
  });
  it("returns the raw id when the stem has no numeric prefix", () => {
    expect(storyDisplayLabel("misc-story")).toBe("misc-story");
  });
  it("returns the id when it is already just the numeric prefix", () => {
    expect(storyDisplayLabel("30-10")).toBe("30-10");
  });
  // R-18 Phase B (AC #25): alpha-prefixed stems (R-12, NB-7, F-30, etc.)
  // also collapse so post-fallback path-derived storyId still renders short.
  it("collapses a full stem with single-letter alpha prefix (R-12-...)", () => {
    expect(storyDisplayLabel("R-12-capture-switch-serialization")).toBe("R-12");
  });
  it("collapses a full stem with multi-letter alpha prefix (NB-7-...)", () => {
    expect(storyDisplayLabel("NB-7-epic-cleanup")).toBe("NB-7");
  });
  it("returns the id when it is already just the alpha-numeric prefix", () => {
    expect(storyDisplayLabel("F-30")).toBe("F-30");
  });
});

describe("prettifyDirLeaf", () => {
  it("title-cases a kebab-case leaf", () => {
    expect(prettifyDirLeaf("epic-workspaces")).toBe("Epic Workspaces");
  });
  it("takes the basename of a multi-segment path", () => {
    expect(prettifyDirLeaf("/a/b/seamless-workspaces/epic-workspaces")).toBe("Epic Workspaces");
  });
  it("handles snake_case leaves", () => {
    expect(prettifyDirLeaf("epic_workspaces_v2")).toBe("Epic Workspaces V2");
  });
  it("leaves an already-clean single word capitalized", () => {
    expect(prettifyDirLeaf("workspaces")).toBe("Workspaces");
  });
  it("preserves the tail casing of a word (acronyms not mangled)", () => {
    expect(prettifyDirLeaf("api-gateway")).toBe("Api Gateway");
    expect(prettifyDirLeaf("API-gateway")).toBe("API Gateway");
  });
  it("returns empty string for an empty or segment-less input", () => {
    expect(prettifyDirLeaf("")).toBe("");
    expect(prettifyDirLeaf("/")).toBe("");
  });
});

// SW-EW-28: shared by the tile chrome + the story-context HUD.
describe("workspaceDisplayLabel", () => {
  it("uses the storyId (preferred over the workspace id) and collapses its stem", () => {
    expect(workspaceDisplayLabel({ kind: "story", id: "w1", storyId: "R-12-capture-switch" })).toBe(
      "R-12",
    );
    // An already-short multi-segment id (e.g. SW-EW-28) renders as-is.
    expect(workspaceDisplayLabel({ kind: "story", id: "w1", storyId: "SW-EW-28" })).toBe(
      "SW-EW-28",
    );
  });
  it("falls back to the workspace id when storyId is absent/empty", () => {
    expect(workspaceDisplayLabel({ kind: "story", id: "30-10-foo" })).toBe("30-10");
  });
  it("prettifies an epic (integration) workspace's dir leaf", () => {
    expect(
      workspaceDisplayLabel({ kind: "epic", id: "e1", epicDirPath: "a/b/epic-workspaces" }),
    ).toBe("Epic Workspaces");
  });
  it("an epic with no dir path falls back to its id", () => {
    expect(workspaceDisplayLabel({ kind: "epic", id: "E-9" })).toBe("E-9");
  });
  it("labels a generic workspace with its dir basename (SW-GW-1)", () => {
    expect(
      workspaceDisplayLabel({
        kind: "generic",
        id: "generic:/Users/me/scratch",
        cwd: "/Users/me/scratch",
      }),
    ).toBe("scratch");
  });
  it("a generic workspace at root shows the cwd ('/'), not the raw id", () => {
    expect(workspaceDisplayLabel({ kind: "generic", id: "generic:/", cwd: "/" })).toBe("/");
  });
  it("a generic workspace with no cwd falls back to its id", () => {
    expect(workspaceDisplayLabel({ kind: "generic", id: "generic:x" })).toBe("generic:x");
  });
});

describe("epicCodeFromTitle (SW-EW-35)", () => {
  it("extracts the trailing (CODE) parenthetical", () => {
    expect(epicCodeFromTitle("Sub-Epic: Epic Planning Workspace (SW-EW)")).toBe("SW-EW");
    expect(epicCodeFromTitle("Some Epic (ABC-12)")).toBe("ABC-12");
  });
  it("uses the LAST parenthetical when several are present", () => {
    expect(epicCodeFromTitle("Foo (bar) baz (SW-EW)")).toBe("SW-EW");
  });
  it("trims whitespace inside the parenthetical", () => {
    expect(epicCodeFromTitle("Title (  SW-EW  )")).toBe("SW-EW");
  });
  it("returns null when there's no trailing parenthetical", () => {
    expect(epicCodeFromTitle("Epic Planning Workspace")).toBeNull();
    // A parenthetical not at the end doesn't count (caller wants the code badge).
    expect(epicCodeFromTitle("Epic (SW-EW) — planning")).toBeNull();
  });
  it("returns null for empty/absent title or an empty parenthetical", () => {
    expect(epicCodeFromTitle(undefined)).toBeNull();
    expect(epicCodeFromTitle(null)).toBeNull();
    expect(epicCodeFromTitle("")).toBeNull();
    expect(epicCodeFromTitle("Title ()")).toBeNull();
    expect(epicCodeFromTitle("Title (   )")).toBeNull();
  });
});

// R-18 Phase B (AC #28): defence-in-depth path canonicaliser. All
// `byAbsPath` keys and `Workspace.storyFilePath` values flow through
// this so a path-string drift (NFD/NFC, trailing slashes, // collapse)
// can't silently break the workspace-frontmatter sync lookup.
describe("normalizeStoryFilePath", () => {
  it("is idempotent — normalize(normalize(x)) === normalize(x)", () => {
    const messy = "/foo//bar/baz/";
    const once = normalizeStoryFilePath(messy);
    const twice = normalizeStoryFilePath(once);
    expect(twice).toBe(once);
  });

  it("drops a single trailing slash", () => {
    expect(normalizeStoryFilePath("/repo/stories/R-12.md/")).toBe("/repo/stories/R-12.md");
  });

  it("does NOT drop a single leading slash (preserves absolute path)", () => {
    expect(normalizeStoryFilePath("/")).toBe("/");
  });

  it("collapses repeated `/` runs to a single `/`", () => {
    expect(normalizeStoryFilePath("/foo//bar///baz.md")).toBe("/foo/bar/baz.md");
  });

  it("NFC-normalises Unicode combining marks", () => {
    // Composed (NFC): "é" = U+00E9 (one codepoint)
    // Decomposed (NFD): "é" = "e" + U+0301 (two codepoints, "combining acute")
    const nfd = "/repo/stories/café.md"; // ends with "café" but as NFD
    const nfc = "/repo/stories/café.md"; // ends with "café" as NFC
    expect(normalizeStoryFilePath(nfd)).toBe(nfc);
  });

  it("passes already-normalised absolute paths through unchanged", () => {
    const clean = "/Users/dev/.codesense/stories/R-12-foo.md";
    expect(normalizeStoryFilePath(clean)).toBe(clean);
  });

  it("does not strip the only `/` from a root path", () => {
    expect(normalizeStoryFilePath("/")).toBe("/");
  });
});

describe("parseStoryTitle", () => {
  it("returns the first H1 line", () => {
    expect(parseStoryTitle("---\nstatus: ready\n---\n\n# Story 30.10 — Workspaces\n")).toBe(
      "Story 30.10 — Workspaces",
    );
  });
  it("returns null when there is no H1", () => {
    expect(parseStoryTitle("Just prose with no heading\n")).toBeNull();
  });
  it("ignores deeper headings", () => {
    expect(parseStoryTitle("## Subheading only\n")).toBeNull();
  });
  it("does not match a # line inside YAML frontmatter (YAML comments)", () => {
    expect(parseStoryTitle("---\n# yaml comment\nstatus: ready\n---\n\n# Real Title\n")).toBe(
      "Real Title",
    );
  });
});

describe("isStoryFile", () => {
  const contentRoot =
    "/repo/_bmad-output/implementation-artifacts/in-progress/30-pipeline-viewer-ui";

  it("returns true for a story-named .md file under the content root", () => {
    expect(isStoryFile(`${contentRoot}/30-10-story-tab-workspaces.md`, contentRoot)).toBe(true);
  });

  it("returns false for a non-story .md file under the content root", () => {
    expect(isStoryFile(`${contentRoot}/README.md`, contentRoot)).toBe(false);
  });

  it("returns false for a file outside the content root", () => {
    expect(isStoryFile("/repo/elsewhere/30-10-story.md", contentRoot)).toBe(false);
  });

  it("returns false when the content root is empty", () => {
    expect(isStoryFile(`${contentRoot}/30-10-story.md`, "")).toBe(false);
  });

  it("returns false when filePath is empty", () => {
    expect(isStoryFile("", contentRoot)).toBe(false);
  });
});

describe("extractFrontmatterBlock", () => {
  it("returns the block body for valid frontmatter", () => {
    expect(extractFrontmatterBlock("---\nstatus: ready\n---\n")).toBe("status: ready");
  });
  it("returns null when no frontmatter delimiters", () => {
    expect(extractFrontmatterBlock("# Just markdown\n")).toBeNull();
  });
  it("returns null for unclosed frontmatter", () => {
    expect(extractFrontmatterBlock("---\nstatus: ready\n")).toBeNull();
  });
  it("handles CRLF line endings", () => {
    expect(extractFrontmatterBlock("---\r\nfoo: bar\r\n---\r\n")).toBe("foo: bar");
  });
});

describe("parseSourceRepoFromFrontmatter", () => {
  it("returns the bare value", () => {
    expect(parseSourceRepoFromFrontmatter("---\nsource_repo: alderco/codesense\n---\n")).toBe(
      "alderco/codesense",
    );
  });
  it("returns null when there is no frontmatter block", () => {
    expect(parseSourceRepoFromFrontmatter("# Just a story\n")).toBeNull();
  });
  it("returns null when frontmatter exists but no source_repo field", () => {
    expect(parseSourceRepoFromFrontmatter("---\nstatus: ready\n---\n")).toBeNull();
  });
  it("returns null for empty value", () => {
    expect(parseSourceRepoFromFrontmatter("---\nsource_repo:    \n---\n")).toBeNull();
  });
  it("strips matched double quotes", () => {
    expect(parseSourceRepoFromFrontmatter('---\nsource_repo: "alderco/codesense"\n---\n')).toBe(
      "alderco/codesense",
    );
  });
  it("strips matched single quotes", () => {
    expect(parseSourceRepoFromFrontmatter("---\nsource_repo: 'alderco/codesense'\n---\n")).toBe(
      "alderco/codesense",
    );
  });
  it("handles CRLF line endings", () => {
    expect(parseSourceRepoFromFrontmatter("---\r\nsource_repo: alderco/codesense\r\n---\r\n")).toBe(
      "alderco/codesense",
    );
  });
  it("strips YAML inline comments from unquoted values", () => {
    expect(
      parseSourceRepoFromFrontmatter("---\nsource_repo: alderco/codesense # main repo\n---\n"),
    ).toBe("alderco/codesense");
  });
  it("preserves hash inside quoted values", () => {
    expect(
      parseSourceRepoFromFrontmatter(
        '---\nsource_repo: "alderco/codesense # not a comment"\n---\n',
      ),
    ).toBe("alderco/codesense # not a comment");
  });
  it("returns null when value is entirely a YAML comment", () => {
    expect(
      parseSourceRepoFromFrontmatter("---\nsource_repo:  # this is a comment\n---\n"),
    ).toBeNull();
  });
});

describe("parseStoryIdFromFrontmatter", () => {
  it("returns the bare value", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: R-18\n---\n")).toBe("R-18");
  });
  it("returns null when there is no frontmatter block", () => {
    expect(parseStoryIdFromFrontmatter("# Just a story\n")).toBeNull();
  });
  it("returns null when frontmatter exists but no story_id field", () => {
    expect(parseStoryIdFromFrontmatter("---\nstatus: ready\n---\n")).toBeNull();
  });
  it("returns null for empty value", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id:    \n---\n")).toBeNull();
  });
  it("strips matched double quotes", () => {
    expect(parseStoryIdFromFrontmatter('---\nstory_id: "R-18"\n---\n')).toBe("R-18");
  });
  it("strips matched single quotes", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: 'R-18'\n---\n")).toBe("R-18");
  });
  it("handles CRLF line endings", () => {
    expect(parseStoryIdFromFrontmatter("---\r\nstory_id: R-18\r\n---\r\n")).toBe("R-18");
  });
  it("strips YAML inline comments from unquoted values", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: R-18 # the refinement\n---\n")).toBe("R-18");
  });
  it("preserves hash inside quoted values", () => {
    expect(parseStoryIdFromFrontmatter('---\nstory_id: "R-18 # not a comment"\n---\n')).toBe(
      "R-18 # not a comment",
    );
  });
  it("returns null when value is entirely a YAML comment", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id:  # this is a comment\n---\n")).toBeNull();
  });
  // Code review (2026-05-31) — Patches P1, P6, P8, P11.
  it("returns null for an empty input string (P11)", () => {
    expect(parseStoryIdFromFrontmatter("")).toBeNull();
  });
  it("returns null for YAML null literal (P6)", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: null\n---\n")).toBeNull();
  });
  it("returns null for YAML tilde-null shorthand (P6)", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: ~\n---\n")).toBeNull();
  });
  it("strips trailing # without preceding whitespace (P8)", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: R-18#\n---\n")).toBe("R-18");
  });
  // Round-2 R1: P8's `\s*#` strip was too greedy and ate `#` inside values.
  // YAML's inline-comment rule is "whitespace + #", so `bug#42` is the
  // whole value, not "bug" + a comment.
  it("preserves `#` inside an unquoted value (round-2 R1)", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: bug#42-feature\n---\n")).toBe(
      "bug#42-feature",
    );
  });
  it("strips an inline comment when preceded by whitespace, even with mid-value `#`", () => {
    expect(parseStoryIdFromFrontmatter("---\nstory_id: bug#42 # comment\n---\n")).toBe("bug#42");
  });
  it("returns null when HTML meta content is an empty string (P1)", () => {
    const html = `<meta name="cs:story-id" content="">`;
    expect(parseStoryIdFromFrontmatter(html)).toBeNull();
  });
  it("trims surrounding whitespace from HTML meta content", () => {
    const html = `<meta name="cs:story-id" content="  R-18  ">`;
    expect(parseStoryIdFromFrontmatter(html)).toBe("R-18");
  });
});

describe("parseDependsOnFromFrontmatter", () => {
  it("returns first entry from depends_on list", () => {
    expect(parseDependsOnFromFrontmatter("---\ndepends_on: [34-4-pipeline-card]\n---\n")).toBe(
      "34-4-pipeline-card",
    );
  });
  it("returns first entry when multiple entries exist", () => {
    expect(
      parseDependsOnFromFrontmatter("---\ndepends_on: [34-4-pipeline, 34-5-other]\n---\n"),
    ).toBe("34-4-pipeline");
  });
  it("returns null when depends_on is empty list", () => {
    expect(parseDependsOnFromFrontmatter("---\ndepends_on: []\n---\n")).toBeNull();
  });
  it("returns null when depends_on is missing", () => {
    expect(parseDependsOnFromFrontmatter("---\nstatus: ready\n---\n")).toBeNull();
  });
  it("returns null when no frontmatter", () => {
    expect(parseDependsOnFromFrontmatter("# Just markdown\n")).toBeNull();
  });
  it("strips quotes from entries", () => {
    expect(parseDependsOnFromFrontmatter('---\ndepends_on: ["34-4-pipeline"]\n---\n')).toBe(
      "34-4-pipeline",
    );
  });
  it("handles block-style YAML list syntax", () => {
    expect(
      parseDependsOnFromFrontmatter("---\ndepends_on:\n  - 34-4-pipeline\n  - 34-5-other\n---\n"),
    ).toBe("34-4-pipeline");
  });
  it("handles block-style with single entry", () => {
    expect(parseDependsOnFromFrontmatter("---\ndepends_on:\n  - 34-4-pipeline\n---\n")).toBe(
      "34-4-pipeline",
    );
  });
});

describe("resolveWorktreeParams", () => {
  it("returns branch from frontmatter and no start point by default (the backend picks the repo's default)", async () => {
    readFileMock.mockResolvedValue("---\nbranch_name: feat/custom\n---\n");
    const result = await resolveWorktreeParams("/repo/stories/30-10.md");
    expect(result).toEqual({ branchName: "feat/custom", startPoint: null });
  });

  it("returns dep branch as start point when depends_on is present", async () => {
    // First call: the story file with depends_on
    readFileMock.mockResolvedValueOnce(
      "---\nbranch_name: feat/30-13-fix\ndepends_on: [34-4-pipeline-card]\n---\n",
    );
    // Second call: the dependency story file
    readFileMock.mockResolvedValueOnce("---\nbranch_name: feat/34-4-pipeline-card-worktree\n---\n");

    const result = await resolveWorktreeParams("/repo/stories/30-13-fix.md");
    expect(result).toEqual({
      branchName: "feat/30-13-fix",
      startPoint: "feat/34-4-pipeline-card-worktree",
    });
  });

  it("falls back to no start point when dep file doesn't exist for any extension", async () => {
    readFileMock.mockResolvedValueOnce(
      "---\nbranch_name: feat/30-13\ndepends_on: [missing-story]\n---\n",
    );
    // readFile throws for all 3 extension probes (md, html, htm)
    readFileMock.mockRejectedValueOnce(new Error("ENOENT"));
    readFileMock.mockRejectedValueOnce(new Error("ENOENT"));
    readFileMock.mockRejectedValueOnce(new Error("ENOENT"));

    const result = await resolveWorktreeParams("/repo/stories/30-13.md");
    expect(result).toEqual({ branchName: "feat/30-13", startPoint: null });
  });

  it("falls back to convention branch + no start point when file read throws", async () => {
    readFileMock.mockRejectedValue(new Error("ENOENT"));
    const result = await resolveWorktreeParams("/repo/stories/30-10-foo.md");
    expect(result).toEqual({ branchName: "feat/30-10-foo", startPoint: null });
  });

  it("falls back to no start point when depends_on is empty list", async () => {
    readFileMock.mockResolvedValue("---\ndepends_on: []\n---\n");
    const result = await resolveWorktreeParams("/repo/stories/30-10.md");
    expect(result).toEqual({ branchName: "feat/30-10", startPoint: null });
  });

  it("uses preReadContent instead of reading from disk when provided", async () => {
    const preRead = "---\nbranch_name: feat/pre-read\n---\n";
    const result = await resolveWorktreeParams("/repo/stories/30-10.md", null, 0, preRead);
    expect(result).toEqual({ branchName: "feat/pre-read", startPoint: null });
    expect(readFileMock).not.toHaveBeenCalled();
  });

  it("still reads from disk for depends_on recursive calls when preReadContent is provided", async () => {
    const preRead = "---\nbranch_name: feat/child\ndepends_on: [34-4-dep]\n---\n";
    readFileMock.mockResolvedValueOnce("---\nbranch_name: feat/34-4-dep-branch\n---\n");
    const result = await resolveWorktreeParams("/repo/stories/child.md", null, 0, preRead);
    expect(result).toEqual({ branchName: "feat/child", startPoint: "feat/34-4-dep-branch" });
    // readFile called only for the dependency, not for the primary story
    expect(readFileMock).toHaveBeenCalledTimes(1);
    expect(readFileMock).toHaveBeenCalledWith("/repo/stories/34-4-dep.md");
  });

  it("uses config.default_start_point as the fallback start point", async () => {
    readFileMock.mockResolvedValue("---\nbranch_name: feat/story\n---\n");
    const result = await resolveWorktreeParams("/repo/stories/30-10.md", {
      version: 1,
      setup: [],
      default_start_point: "develop",
    });
    expect(result).toEqual({ branchName: "feat/story", startPoint: "develop" });
  });

  it("uses default_start_point in error fallback path", async () => {
    readFileMock.mockRejectedValue(new Error("ENOENT"));
    const result = await resolveWorktreeParams("/repo/stories/30-10-foo.md", {
      version: 1,
      setup: [],
      default_start_point: "develop",
    });
    expect(result).toEqual({ branchName: "feat/30-10-foo", startPoint: "develop" });
  });

  it("leaves the start point to the backend when config has no default_start_point", async () => {
    readFileMock.mockResolvedValue("---\nbranch_name: feat/story\n---\n");
    const result = await resolveWorktreeParams("/repo/stories/30-10.md", {
      version: 1,
      setup: [],
    });
    expect(result).toEqual({ branchName: "feat/story", startPoint: null });
  });

  it("treats an empty default_start_point as unset", async () => {
    readFileMock.mockResolvedValue("---\nbranch_name: feat/story\n---\n");
    const result = await resolveWorktreeParams("/repo/stories/30-10.md", {
      version: 1,
      setup: [],
      default_start_point: "",
    });
    expect(result).toEqual({ branchName: "feat/story", startPoint: null });
  });

  // #2840: a repo whose default branch is not `main` and that has no worktree.yaml
  // never gets an assumed `main` (git: "invalid reference: main"); the backend
  // resolves origin/HEAD (here develop or master) for the new branch instead.
  it.each(["develop", "master"])(
    "#2840: a repo whose default is %s, with no config, resolves no start point (never main)",
    async () => {
      readFileMock.mockResolvedValue("---\nbranch_name: zz-open-probe\n---\n");
      const result = await resolveWorktreeParams("/repo/stories/ZZ-PROBE-1.md", null);
      expect(result).toEqual({ branchName: "zz-open-probe", startPoint: null });
    },
  );

  // SW-FX-3: cross-epic depends_on written in BMad's path-style convention
  // (`../SIBLING-EPIC/STORY-ID`) must resolve like a bare sibling id. The
  // pre-fix guard rejected any depId containing `..` or `/`, so these stories
  // silently fell through to the default start point.
  const swFx3Config = {
    version: 1 as const,
    setup: [],
    default_start_point: "dev",
  };

  it("AC-6a: resolves cross-epic ../SIBLING-EPIC/DEP-ID depends_on to the dep's branch_name", async () => {
    // Parent story carries a path-style cross-epic dependency.
    readFileMock.mockResolvedValueOnce(
      "---\nbranch_name: feat/DEMO-1712\ndepends_on:\n  - ../DEMO-1758-foundation/DEMO-1670-dep\n---\n",
    );
    // The dependency file (resolved across the sibling-epic folder).
    readFileMock.mockResolvedValueOnce("---\nbranch_name: feat/DEMO-1670-dep-branch\n---\n");

    const result = await resolveWorktreeParams("/repo/epic-a/DEMO-1712.md");

    expect(result.startPoint).toBe("feat/DEMO-1670-dep-branch");
    // The dep read uses the `..`-relative path verbatim — the OS normalizes
    // it on open; we never hand-roll path-segment math on the TS side.
    expect(readFileMock).toHaveBeenNthCalledWith(
      2,
      "/repo/epic-a/../DEMO-1758-foundation/DEMO-1670-dep.md",
    );
  });

  it("AC-6b: cross-epic dep with every extension probe missing falls back to default_start_point", async () => {
    readFileMock.mockResolvedValueOnce(
      "---\nbranch_name: feat/DEMO-1712\ndepends_on:\n  - ../DEMO-1758-foundation/DEMO-1670-dep\n---\n",
    );
    // All three PIPELINE_EXTENSIONS probes (md, html, htm) miss.
    readFileMock.mockRejectedValueOnce(new Error("ENOENT"));
    readFileMock.mockRejectedValueOnce(new Error("ENOENT"));
    readFileMock.mockRejectedValueOnce(new Error("ENOENT"));

    const result = await resolveWorktreeParams("/repo/epic-a/DEMO-1712.md", swFx3Config);

    expect(result).toEqual({ branchName: "feat/DEMO-1712", startPoint: "dev" });
  });

  it("AC-6c: path-qualified self-reference is skipped — no recursion-into-self, falls back to default", async () => {
    // A story whose depends_on points at its own file via `../<own-epic>/<self>`.
    // The `..`-relative dep path normalizes back to the parent; without the
    // self-ref skip the unwound recursion would return the story's OWN
    // branch_name (feat/DEMO-1712) instead of the default start point.
    const parentPath = "/repo/epic-a/DEMO-1712.md";
    const selfRefDepPath = "/repo/epic-a/../epic-a/DEMO-1712.md";
    const parentContent =
      "---\nbranch_name: feat/DEMO-1712\ndepends_on:\n  - ../epic-a/DEMO-1712\n---\n";
    readFileMock.mockImplementation(async (p: string) => {
      if (p === parentPath || p === selfRefDepPath) return parentContent;
      throw new Error("ENOENT");
    });

    const result = await resolveWorktreeParams(parentPath, swFx3Config);

    expect(result.startPoint).toBe("dev");
    expect(result.startPoint).not.toBe("feat/DEMO-1712");
  });

  it("AC-6c (cross-extension): self-ref skipped even when a same-stem twin in another extension exists", async () => {
    // A `.md` story self-references; a same-stem `.html` twin also exists in
    // the dir. Self-ref identity is by stem (codesense's story id), so the
    // skip must fire for EVERY probed extension — otherwise the resolver
    // reads the `.html` twin and adopts ITS branch as the start point.
    const parentPath = "/repo/epic-a/DEMO-1712.md";
    const parentContent =
      "---\nbranch_name: feat/DEMO-1712\ndepends_on:\n  - ../epic-a/DEMO-1712\n---\n";
    const twinContent = "---\nbranch_name: feat/DEMO-1712-html-twin\n---\n";
    readFileMock.mockImplementation(async (p: string) => {
      if (p === "/repo/epic-a/../epic-a/DEMO-1712.html") return twinContent;
      if (p === parentPath) return parentContent;
      throw new Error("ENOENT");
    });

    const result = await resolveWorktreeParams(parentPath, swFx3Config);

    expect(result.startPoint).toBe("dev");
    expect(result.startPoint).not.toBe("feat/DEMO-1712-html-twin");
  });
});

// ----- R.7: HTML content detection in extractors ----------------------------

describe("HTML content detection (R.7)", () => {
  it("parseBranchNameFromFrontmatter extracts from HTML meta tag", () => {
    const html = `<html><head><meta name="cs:branch-name" content="feat/foo"></head></html>`;
    expect(parseBranchNameFromFrontmatter(html)).toBe("feat/foo");
  });

  it("parseBranchNameFromFrontmatter prefers canonical cs:branch (HTML)", () => {
    const html = `<meta name="cs:branch" content="feat/canonical"><meta name="cs:branch-name" content="feat/legacy">`;
    expect(parseBranchNameFromFrontmatter(html)).toBe("feat/canonical");
  });

  it("parseBranchNameFromFrontmatter falls through an empty cs:branch to the legacy field (HTML)", () => {
    // SW-OR-1b: an empty `cs:branch` must not shadow a valid legacy field nor be
    // used verbatim as a blank branch — the precedence chain falls through.
    const html = `<meta name="cs:branch" content=""><meta name="cs:branch-name" content="feat/legacy">`;
    expect(parseBranchNameFromFrontmatter(html)).toBe("feat/legacy");
  });

  it("parseSourceRepoFromFrontmatter extracts from HTML meta tag", () => {
    const html = `<meta name="cs:source-repo" content="alderco/codesense">`;
    expect(parseSourceRepoFromFrontmatter(html)).toBe("alderco/codesense");
  });

  it("parseDependsOnFromFrontmatter extracts from HTML meta tag", () => {
    const html = `<meta name="cs:depends-on" content="W-5-foo">`;
    expect(parseDependsOnFromFrontmatter(html)).toBe("W-5-foo");
  });

  it("parseStoryIdFromFrontmatter extracts from HTML meta tag", () => {
    const html = `<meta name="cs:story-id" content="R-18">`;
    expect(parseStoryIdFromFrontmatter(html)).toBe("R-18");
  });

  it("HTML meta tags return null when absent", () => {
    const html = `<html><head><title>T</title></head></html>`;
    expect(parseBranchNameFromFrontmatter(html)).toBeNull();
    expect(parseSourceRepoFromFrontmatter(html)).toBeNull();
    expect(parseDependsOnFromFrontmatter(html)).toBeNull();
    expect(parseStoryIdFromFrontmatter(html)).toBeNull();
  });

  it("markdown content still uses YAML path", () => {
    const md = `---\nbranch_name: feat/bar\nsource_repo: org/repo\n---\n# Title`;
    expect(parseBranchNameFromFrontmatter(md)).toBe("feat/bar");
    expect(parseSourceRepoFromFrontmatter(md)).toBe("org/repo");
  });

  it("whitespace + <!DOCTYPE detected as HTML", () => {
    const html = `  <!DOCTYPE html><html><head><meta name="cs:branch-name" content="feat/x"></head></html>`;
    expect(parseBranchNameFromFrontmatter(html)).toBe("feat/x");
  });

  it("reversed attribute order works", () => {
    const html = `<meta content="feat/rev" name="cs:branch-name">`;
    expect(parseBranchNameFromFrontmatter(html)).toBe("feat/rev");
  });

  it("markdown starting with <!-- comment --> is NOT detected as HTML", () => {
    // Frontmatter must be at line 1 for extractFrontmatterBlock to parse it,
    // so the comment goes after the frontmatter block. This tests that the
    // isHtmlContent heuristic does NOT trigger on `<!` (only `<!doctype`).
    const md = `---\nbranch_name: feat/guarded\n---\n<!-- markdownlint-disable -->\n# Title`;
    expect(parseBranchNameFromFrontmatter(md)).toBe("feat/guarded");
  });

  it("parseDependsOnFromFrontmatter returns first entry from multi-CSV HTML", () => {
    const html = `<meta name="cs:depends-on" content="W-5-foo, W-6-bar, W-7-baz">`;
    expect(parseDependsOnFromFrontmatter(html)).toBe("W-5-foo");
  });

  it("parseStoryTitle extracts from HTML <title> tag", () => {
    const html = `<!DOCTYPE html><html><head><title>My HTML Story</title></head></html>`;
    expect(parseStoryTitle(html)).toBe("My HTML Story");
  });

  it("parseStoryTitle falls back to <h1> when no <title>", () => {
    const html = `<html><body><h1>Heading Title</h1></body></html>`;
    expect(parseStoryTitle(html)).toBe("Heading Title");
  });

  it("parseStoryTitle returns null for HTML with no title or h1", () => {
    const html = `<html><body><p>No heading</p></body></html>`;
    expect(parseStoryTitle(html)).toBeNull();
  });
});

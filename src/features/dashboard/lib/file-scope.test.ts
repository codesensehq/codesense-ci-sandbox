// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { RepoScanResult } from "@/shared/lib/pipeline-graph/merge-pipeline-trees";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";

import {
  collectNavDirs,
  findDocInTree,
  findScopeForPath,
  findScopeInTree,
  inheritedFilesUnder,
  isOwnTeamFileInTree,
  repoNameFromSlug,
  resolveFileScope,
  scopeLabel,
} from "./file-scope";

describe("resolveFileScope (SW-VE-5 AC-2/AC-3 — the scope model)", () => {
  it("returns null when the doc names no owning team (degrade → no badge, AC-1)", () => {
    expect(resolveFileScope(undefined, "alderco/codesense", "default")).toBeNull();
  });

  it("maps an own-team file with its repo slug, not inherited", () => {
    expect(resolveFileScope("default", "alderco/codesense", "default")).toEqual({
      repoSlug: "alderco/codesense",
      sourceTeam: "default",
      isInherited: false,
    });
  });

  it("marks a file inherited when its owning team is a PARENT (not the active team) — AC-3", () => {
    const scope = resolveFileScope("platform", undefined, "payments");
    expect(scope?.sourceTeam).toBe("platform");
    expect(scope?.repoSlug).toBeNull();
    expect(scope?.isInherited).toBe(true);
  });

  it("is NOT inherited when sourceTeam is null (own, unnamed owner)", () => {
    expect(resolveFileScope(null, "alderco/codesense", "payments")?.isInherited).toBe(false);
  });

  it("fails closed: a named team with the active team unknown counts as inherited", () => {
    expect(resolveFileScope("platform", undefined, null)?.isInherited).toBe(true);
  });
});

describe("scopeLabel / repoNameFromSlug (SW-VE-5 AC-2 — the chrome label)", () => {
  it("names a source-repo scope by its repo leaf name", () => {
    expect(scopeLabel(resolveFileScope("default", "alderco/codesense", "default")!)).toBe(
      "codesense",
    );
  });

  it("names an own-team scope by its team", () => {
    expect(scopeLabel(resolveFileScope("default", null, "default")!)).toBe("default");
  });

  it("falls back to 'this team' for an own, unnamed owner", () => {
    expect(scopeLabel(resolveFileScope(null, null, "default")!)).toBe("this team");
  });

  it("names an inherited scope by its owning parent team", () => {
    expect(scopeLabel(resolveFileScope("platform", null, "payments")!)).toBe(
      "Inherited from platform",
    );
  });

  it("repoNameFromSlug strips the owner", () => {
    expect(repoNameFromSlug("alderco/codesense")).toBe("codesense");
    expect(repoNameFromSlug("bare")).toBe("bare");
  });
});

// ---------------------------------------------------------------------------
// findScopeForPath — the metadata-chip inherited gate (AC-3) reads scope by the
// active tab's absolute path out of the resident `installedScans`.
// ---------------------------------------------------------------------------

function tree(over: Partial<DerivedPipelineTree> = {}): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "installed://alderco/codesense/",
    stories: [],
    docs: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...over,
  };
}

describe("findScopeForPath (SW-VE-5 AC-3 — join active-tab path → scope)", () => {
  const scans: RepoScanResult[] = [
    {
      repoSlug: "alderco/codesense",
      tree: tree({
        docs: [{ fileName: "own.md", filePath: "/layers/default/own.md" }],
        subEpics: [
          tree({
            docs: [
              {
                fileName: "inherited.md",
                filePath: "/layers/platform/nested/inherited.md",
                sourceTeam: "platform",
              },
            ],
          }),
        ],
      }),
    },
  ];

  it("finds a nested inherited doc by absolute path and flags it inherited", () => {
    const scope = findScopeForPath(scans, "/layers/platform/nested/inherited.md", "default");
    expect(scope?.isInherited).toBe(true);
    expect(scope?.repoSlug).toBe("alderco/codesense");
  });

  it("resolves an own-team doc (no sourceTeam) to no scope — editable, no badge", () => {
    expect(findScopeForPath(scans, "/layers/default/own.md", "default")).toBeNull();
  });

  it("returns null for a path not present in any scan (degrade → no gate, no badge)", () => {
    expect(findScopeForPath(scans, "/somewhere/else.yaml", "default")).toBeNull();
  });
});

describe("findScopeInTree (SW-VE-6 — lifecycle-menu inherited gate)", () => {
  const navTree = tree({
    docs: [{ fileName: "own.md", filePath: "/layers/default/own.md" }],
    subEpics: [
      tree({
        docs: [
          {
            fileName: "inherited.md",
            filePath: "/layers/platform/nested/inherited.md",
            sourceTeam: "platform",
          },
        ],
      }),
    ],
  });

  it("flags a nested parent-owned doc inherited (the menu and viewer name its team)", () => {
    expect(
      findScopeInTree(navTree, "/layers/platform/nested/inherited.md", "default")?.isInherited,
    ).toBe(true);
  });
  it("resolves an own-team doc (no sourceTeam) as editable", () => {
    expect(findScopeInTree(navTree, "/layers/default/own.md", "default")?.isInherited).toBeFalsy();
  });
  it("returns null for an uncovered (generic/config) row → treated as own-surface/editable", () => {
    expect(findScopeInTree(navTree, "/layers/default/config.yaml", "default")).toBeNull();
  });

  // SW-XS-20: folders are not inherited — only files carry an owning team.
  const folderTree = tree({
    absoluteDirPath: "/repo",
    subEpics: [
      tree({
        absoluteDirPath: "/repo/skills",
        docs: [
          { fileName: "theirs.md", filePath: "/parent/skills/theirs.md", sourceTeam: "platform" },
          { fileName: "mine.md", filePath: "/repo/skills/mine.md", overridesTeam: "platform" },
        ],
        subEpics: [
          tree({
            absoluteDirPath: "/repo/skills/deep",
            docs: [
              { fileName: "a.md", filePath: "/parent/skills/deep/a.md", sourceTeam: "platform" },
              { fileName: "b.md", filePath: "/grand/skills/deep/b.md", sourceTeam: "grand" },
            ],
          }),
        ],
      }),
      tree({
        absoluteDirPath: "/repo/rules",
        docs: [{ fileName: "r.md", filePath: "/parent/rules/r.md", sourceTeam: "platform" }],
      }),
      tree({
        absoluteDirPath: "/repo/epic",
        stories: [{ fileName: "s.md", filePath: "/repo/epic/s.md" } as StoryRef],
      }),
    ],
  });

  it("gives a folder no scope, even one only a parent team has", () => {
    expect(findScopeInTree(folderTree, "/repo/rules", "default")).toBeNull();
    expect(findScopeInTree(folderTree, "/repo/skills", "default")).toBeNull();
  });
  it("still names the team of each file in a mixed folder", () => {
    expect(findScopeInTree(folderTree, "/parent/skills/theirs.md", "default")?.sourceTeam).toBe(
      "platform",
    );
    expect(findScopeInTree(folderTree, "/repo/skills/mine.md", "default")).toBeNull();
  });

  it("counts a folder's files from parent teams, all the way down, by team", () => {
    expect(inheritedFilesUnder(folderTree, "/repo/skills", "default")).toEqual([
      { team: "platform", count: 2 },
      { team: "grand", count: 1 },
    ]);
    expect(inheritedFilesUnder(folderTree, "/repo/epic", "default")).toEqual([]);
    expect(inheritedFilesUnder(folderTree, "/repo/missing", "default")).toEqual([]);
  });

  it("knows the team's own files (docs and stories), not inherited ones", () => {
    expect(isOwnTeamFileInTree(folderTree, "/repo/skills/mine.md", "default")).toBe(true);
    expect(isOwnTeamFileInTree(folderTree, "/repo/epic/s.md", "default")).toBe(true);
    expect(isOwnTeamFileInTree(folderTree, "/parent/rules/r.md", "default")).toBe(false);
    expect(isOwnTeamFileInTree(folderTree, "/repo/unknown.md", "default")).toBe(false);
  });

  it("finds a doc by path, with the parent team its own copy overrides", () => {
    expect(findDocInTree(folderTree, "/repo/skills/mine.md")?.overridesTeam).toBe("platform");
    expect(findDocInTree(folderTree, "/nope.md")).toBeNull();
  });
});

describe("collectNavDirs (SW-VE-6 — Move-to picker destinations)", () => {
  it("lists every directory depth-first", () => {
    const navTree = tree({
      absoluteDirPath: "/repo/own",
      subEpics: [
        tree({
          absoluteDirPath: "/repo/own/a",
          subEpics: [tree({ absoluteDirPath: "/repo/own/a/b" })],
        }),
        tree({ absoluteDirPath: "/repo/own/c" }),
      ],
    });
    expect(collectNavDirs(navTree)).toEqual([
      "/repo/own",
      "/repo/own/a",
      "/repo/own/a/b",
      "/repo/own/c",
    ]);
  });
});

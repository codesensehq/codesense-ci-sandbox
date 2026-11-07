// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { DerivedPipelineTree } from "./types";
import {
  installedRepoSlugFromNavPath,
  isInstalledNavPath,
  mergeInstalledIntoNav,
  wantSetToRepoScan,
} from "./composite-to-repo-scan";

// A small helper to walk to a named sub-epic by dirName from a node.
function sub(node: DerivedPipelineTree, dirName: string): DerivedPipelineTree {
  const found = node.subEpics.find((s) => s.dirName === dirName);
  if (!found) throw new Error(`no sub-epic "${dirName}" in ${node.subEpics.map((s) => s.dirName)}`);
  return found;
}

// SW-XS-15: the source-repo scan is the agent's repo-tier want-set files.
type FileIn = { logicalName: string; path?: string };
const entry = (f: FileIn): FileIn => f;
const repoScan = (slug: string, files: FileIn[]) =>
  wantSetToRepoScan(
    slug,
    files.map((f) => ({ logical: f.logicalName, path: f.path ?? `/abs/${f.logicalName}` })),
  );

describe("wantSetToRepoScan (SLW-NC-1 AC-1, SW-XS-15)", () => {
  it("nests flat logical names into a DerivedPipelineTree of dirs (subEpics) + files (docs)", () => {
    const scan = repoScan("alderco/codesense", [
      entry({ logicalName: "README.md" }),
      entry({ logicalName: "_bmad-output/prd.md" }),
      entry({ logicalName: "_bmad-output/epics/e1.md" }),
    ]);

    expect(scan.repoSlug).toBe("alderco/codesense");
    const root = scan.tree;

    // Root-level file → docs
    expect(root.docs?.map((d) => d.fileName)).toEqual(["README.md"]);
    // Root-level dir → subEpics
    const bmad = sub(root, "_bmad-output");
    expect(bmad.docs?.map((d) => d.fileName)).toEqual(["prd.md"]);
    const epics = sub(bmad, "epics");
    expect(epics.docs?.map((d) => d.fileName)).toEqual(["e1.md"]);
  });

  it("uses the file's path on this Mac as the doc filePath (openable) (AC-1)", () => {
    const scan = repoScan("o/r", [entry({ logicalName: "a/b.md", path: "/winner/a/b.md" })]);
    const a = sub(scan.tree, "a");
    expect(a.docs?.[0]?.filePath).toBe("/winner/a/b.md");
  });

  it('puts the files under one group folder when asked (the nav\'s "‹repo› content")', () => {
    const scan = wantSetToRepoScan(
      "o/r",
      [
        { logical: "docs/a.md", path: "/r/docs/a.md" },
        { logical: "gone.md", path: null },
      ],
      "r content",
    );
    expect(scan.tree.subEpics.map((s) => s.dirName)).toEqual(["r content"]);
    const docs = sub(sub(scan.tree, "r content"), "docs");
    expect(docs.docs?.[0]?.filePath).toBe("/r/docs/a.md");
    expect(JSON.stringify(scan.tree)).not.toContain("gone.md");
  });

  it("gives every dir node a unique repo-namespaced synthetic absoluteDirPath", () => {
    const scan = repoScan("alderco/codesense", [
      entry({ logicalName: "d1/f.md" }),
      entry({ logicalName: "d2/f.md" }),
    ]);
    const paths = scan.tree.subEpics.map((s) => s.absoluteDirPath);
    expect(new Set(paths).size).toBe(paths.length); // unique
    for (const p of paths) {
      expect(p).toContain("alderco/codesense");
      expect(isInstalledNavPath(p)).toBe(true); // flagged for the drill guard
    }
  });

  it("produces no stories — installed content is browsed, never graphed (AC-4)", () => {
    const scan = repoScan("o/r", [
      entry({ logicalName: "story-like.md" }),
      entry({ logicalName: "d/deep.md" }),
    ]);
    const collectStories = (n: DerivedPipelineTree): number =>
      n.stories.length + n.subEpics.reduce((acc, s) => acc + collectStories(s), 0);
    expect(collectStories(scan.tree)).toBe(0);
  });

  it("handles an empty want set (no repo-tier files)", () => {
    const scan = repoScan("o/r", []);
    expect(scan.tree.subEpics).toEqual([]);
    expect(scan.tree.docs ?? []).toEqual([]);
  });
});

describe("mergeInstalledIntoNav — Phase 3 flat unified merge (P3-AC-1/2/3/3b/5)", () => {
  // Context (team workspace): a `_bmad-output` dir with a story + a doc, and a
  // root doc. Source repo overlays this.
  const context = (): DerivedPipelineTree => ({
    dirPath: "",
    dirName: "",
    absoluteDirPath: "/ctx",
    stories: [],
    docs: [{ fileName: "README.md", filePath: "/ctx/README.md" }],
    subEpics: [
      {
        dirPath: "_bmad-output",
        dirName: "_bmad-output",
        absoluteDirPath: "/ctx/_bmad-output",
        stories: [{ fileName: "story-a.md", filePath: "/ctx/_bmad-output/story-a.md" } as never],
        docs: [{ fileName: "prd.md", filePath: "/ctx/_bmad-output/prd.md" }],
        subEpics: [],
        orderError: null,
        mainDoc: null,
      },
    ],
    orderError: null,
    mainDoc: null,
  });

  const names = (n: DerivedPipelineTree) => n.subEpics.map((s) => s.dirName);
  const docNames = (n: DerivedPipelineTree) => (n.docs ?? []).map((d) => d.fileName);
  const findSub = (n: DerivedPipelineTree, name: string) =>
    n.subEpics.find((s) => s.dirName === name)!;

  // Source repo: shadows `_bmad-output/prd.md`, adds `_bmad-output/epics.md`, and
  // brings a source-only `src` dir with `main.rs`.
  const scans = [
    repoScan("alderco/codesense", [
      entry({ logicalName: "_bmad-output/prd.md", path: "/src/prd.md" }),
      entry({ logicalName: "_bmad-output/epics.md", path: "/src/epics.md" }),
      entry({ logicalName: "src/main.rs", path: "/src/src/main.rs" }),
    ]),
  ];

  it("team-workspace default (no repo selected) returns context BY REFERENCE — no groups (P3-AC-1)", () => {
    const ctx = context();
    expect(mergeInstalledIntoNav(ctx, "", scans)).toBe(ctx);
    expect(mergeInstalledIntoNav(ctx, "", scans, "")).toBe(ctx);
  });

  it("drilled scope returns context by reference (orthogonal to selection)", () => {
    const ctx = context();
    expect(mergeInstalledIntoNav(ctx, "_bmad-output", scans, "alderco/codesense")).toBe(ctx);
  });

  it("a selection with no resolved scan returns context by reference (loading/empty)", () => {
    const ctx = context();
    expect(mergeInstalledIntoNav(ctx, "", scans, "alderco/gone")).toBe(ctx);
  });

  it("selecting a repo merges its content FLAT — no owner/repo wrapper group (P3-AC-2)", () => {
    const merged = mergeInstalledIntoNav(context(), "", scans, "alderco/codesense")!;
    // Source dirs interleave at their real relative positions; no `alderco/codesense` node.
    expect(names(merged).sort()).toEqual(["_bmad-output", "src"]);
    expect(names(merged)).not.toContain("alderco/codesense");
  });

  it("source repo WINS a colliding path — one row, winner is the source, shadow flag set (P3-AC-3/3b)", () => {
    const merged = mergeInstalledIntoNav(context(), "", scans, "alderco/codesense")!;
    const bmad = findSub(merged, "_bmad-output");
    // prd.md exists in both → source wins; only one prd.md row, from the source.
    const prd = (bmad.docs ?? []).filter((d) => d.fileName === "prd.md");
    expect(prd).toHaveLength(1);
    expect(prd[0].filePath).toBe("/src/prd.md"); // the source winner
    expect(prd[0].installedSourceRepo).toBe("alderco/codesense");
    expect(prd[0].shadowsTeamFile).toBe(true);
  });

  it("a non-colliding source doc is marked source but does NOT shadow (P3-AC-3b)", () => {
    const merged = mergeInstalledIntoNav(context(), "", scans, "alderco/codesense")!;
    const epics = (findSub(merged, "_bmad-output").docs ?? []).find(
      (d) => d.fileName === "epics.md",
    )!;
    expect(epics.installedSourceRepo).toBe("alderco/codesense");
    expect(epics.shadowsTeamFile).toBe(false);
  });

  it("a context doc the source does NOT shadow is kept untagged", () => {
    const merged = mergeInstalledIntoNav(context(), "", scans, "alderco/codesense")!;
    const readme = (merged.docs ?? []).find((d) => d.fileName === "README.md")!;
    expect(readme.installedSourceRepo).toBeUndefined();
    expect(docNames(findSub(merged, "_bmad-output"))).toContain("prd.md");
  });

  it("a source-only dir keeps its installed:// path (browse-only) and marks its docs", () => {
    const merged = mergeInstalledIntoNav(context(), "", scans, "alderco/codesense")!;
    const src = findSub(merged, "src");
    expect(isInstalledNavPath(src.absoluteDirPath)).toBe(true);
    expect(installedRepoSlugFromNavPath(src.absoluteDirPath)).toBe("alderco/codesense");
    expect((src.docs ?? [])[0].installedSourceRepo).toBe("alderco/codesense");
  });

  it("a source doc that shadows a context STORY drops the story and marks the winner (P3-AC-3)", () => {
    // Source repo shadows `_bmad-output/story-a.md` — which is a context STORY.
    const storyShadowScan = [
      repoScan("alderco/codesense", [
        entry({ logicalName: "_bmad-output/story-a.md", path: "/src/story-a.md" }),
      ]),
    ];
    const merged = mergeInstalledIntoNav(context(), "", storyShadowScan, "alderco/codesense")!;
    const bmad = findSub(merged, "_bmad-output");
    // The context story is gone from the nav (source won).
    expect(bmad.stories.map((s) => s.fileName)).not.toContain("story-a.md");
    // The winner is the source doc, badged as shadowing.
    const winner = (bmad.docs ?? []).find((d) => d.fileName === "story-a.md")!;
    expect(winner.filePath).toBe("/src/story-a.md");
    expect(winner.shadowsTeamFile).toBe(true);
  });

  it("a source doc that shadows a context epic's mainDoc drops the mainDoc + badges the winner (P3-AC-3/3b)", () => {
    const ctx: DerivedPipelineTree = {
      dirPath: "",
      dirName: "",
      absoluteDirPath: "/ctx",
      stories: [],
      docs: [],
      subEpics: [
        {
          dirPath: "epic-a",
          dirName: "epic-a",
          absoluteDirPath: "/ctx/epic-a",
          stories: [],
          docs: [],
          subEpics: [],
          orderError: null,
          mainDoc: { filePath: "/ctx/epic-a/README.md", supportingDocs: null, purposeText: null },
        },
      ],
      orderError: null,
      mainDoc: null,
    };
    const merged = mergeInstalledIntoNav(
      ctx,
      "",
      [repoScan("alderco/codesense", [entry({ logicalName: "epic-a/README.md" })])],
      "alderco/codesense",
    )!;
    const epic = findSub(merged, "epic-a");
    // The context mainDoc is dropped (no duplicate row) — source won.
    expect(epic.mainDoc).toBeNull();
    const winner = (epic.docs ?? []).find((d) => d.fileName === "README.md")!;
    expect(winner.shadowsTeamFile).toBe(true);
  });

  it("keeps context-only subtrees BY REFERENCE + never mutates the input (P3-AC-5)", () => {
    // A context sub-epic the source doesn't touch must survive as the SAME node
    // reference (so graph/readiness derived from it stay byte-identical).
    const ctx: DerivedPipelineTree = {
      ...context(),
      subEpics: [
        ...context().subEpics,
        {
          dirPath: "planning",
          dirName: "planning",
          absoluteDirPath: "/ctx/planning",
          stories: [],
          docs: [{ fileName: "brief.md", filePath: "/ctx/planning/brief.md" }],
          subEpics: [],
          orderError: null,
          mainDoc: null,
        },
      ],
    };
    const planningRef = ctx.subEpics.find((s) => s.dirName === "planning")!;
    const beforeRootDocs = ctx.docs!.length;
    const merged = mergeInstalledIntoNav(ctx, "", scans, "alderco/codesense")!;
    // The untouched context subtree is the exact same reference.
    expect(merged.subEpics.find((s) => s.dirName === "planning")).toBe(planningRef);
    // Input never mutated.
    expect(ctx.docs!.length).toBe(beforeRootDocs);
    expect(ctx.subEpics.find((s) => s.dirName === "_bmad-output")!.stories).toHaveLength(1);
  });

  it("passes a null context tree through as null", () => {
    expect(mergeInstalledIntoNav(null, "", scans, "alderco/codesense")).toBeNull();
  });
});

describe("installedRepoSlugFromNavPath (SLW-NC-1 — source dir slug for tint)", () => {
  it("extracts the owner/repo slug from an installed dir path", () => {
    expect(installedRepoSlugFromNavPath("installed://alderco/codesense/")).toBe(
      "alderco/codesense",
    );
    expect(installedRepoSlugFromNavPath("installed://alderco/codesense")).toBe("alderco/codesense");
    expect(installedRepoSlugFromNavPath("installed://alderco/codesense/_bmad-output/deep/x")).toBe(
      "alderco/codesense",
    );
  });

  it("returns null for real (non-installed) context paths and malformed input", () => {
    expect(installedRepoSlugFromNavPath("/Users/me/content/in-progress/epic-a")).toBeNull();
    expect(installedRepoSlugFromNavPath("")).toBeNull();
    expect(installedRepoSlugFromNavPath("installed://onlyowner/")).toBeNull();
  });
});

describe("isInstalledNavPath (SLW-NC-1 drill guard discriminator)", () => {
  const collectAbsPaths = (n: DerivedPipelineTree): string[] => [
    n.absoluteDirPath,
    ...n.subEpics.flatMap(collectAbsPaths),
  ];

  it("flags installed synthetic node paths, not real context filesystem paths", () => {
    const scan = repoScan("alderco/codesense", [
      entry({ logicalName: "a.md" }),
      entry({ logicalName: "_bmad-output/deep/b.md" }),
    ]);
    for (const p of collectAbsPaths(scan.tree)) {
      expect(isInstalledNavPath(p)).toBe(true);
    }
    // Real, absolute context paths are never flagged — the guard can't block a
    // legitimate context drill, even for a folder whose name contains odd chars.
    expect(isInstalledNavPath("/Users/me/.codesense/.../content/in-progress/epic-a")).toBe(false);
    expect(isInstalledNavPath("/Users/me/content/notes::draft")).toBe(false);
    expect(isInstalledNavPath("")).toBe(false);
    expect(isInstalledNavPath("in-progress/epic-a")).toBe(false);
  });

  it("after a flat merge, source-only dirs are flagged and context dirs are not (P3-AC-6)", () => {
    // Context has a `_bmad-output` dir; the source repo brings a source-only
    // `src` dir. The merged tree keeps the context dir (real path, not flagged)
    // and the source-only dir (installed:// path, flagged for browse-only).
    const context: DerivedPipelineTree = {
      dirPath: "",
      dirName: "",
      absoluteDirPath: "/ctx",
      stories: [],
      docs: [],
      subEpics: [
        {
          dirPath: "_bmad-output",
          dirName: "_bmad-output",
          absoluteDirPath: "/ctx/_bmad-output",
          stories: [],
          docs: [],
          subEpics: [],
          orderError: null,
          mainDoc: null,
        },
      ],
      orderError: null,
      mainDoc: null,
    };
    const merged = mergeInstalledIntoNav(
      context,
      "",
      [
        repoScan("alderco/codesense", [
          entry({ logicalName: "_bmad-output/shared.md" }),
          entry({ logicalName: "src/main.rs" }),
        ]),
      ],
      "alderco/codesense",
    )!;
    expect(isInstalledNavPath(merged.absoluteDirPath)).toBe(false); // context root
    const bmad = merged.subEpics.find((s) => s.dirName === "_bmad-output")!;
    expect(isInstalledNavPath(bmad.absoluteDirPath)).toBe(false); // merged context dir, real path
    const src = merged.subEpics.find((s) => s.dirName === "src")!;
    for (const p of collectAbsPaths(src)) {
      expect(isInstalledNavPath(p)).toBe(true); // source-only dir + descendants
    }
  });
});

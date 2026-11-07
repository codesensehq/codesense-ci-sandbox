// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  flattenNavTree,
  buildNavSearchFilter,
  collectPinTargets,
  collectTreePinTargets,
  prependPinnedSection,
  type FlatNavItem,
} from "./flatten-nav-tree";
import type { PinnedNavItem } from "./nav-pins";
import type { FileEntry } from "@/tauri/commands";
import { buildPipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import { findScopedSubtree } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { computeCountsByAbsPath } from "./nav-ordering";

function makeEntry(name: string, parent: string, opts: Partial<FileEntry> = {}): FileEntry {
  return {
    name,
    path: `${parent}/${name}`,
    is_dir: false,
    extension: name.split(".").pop() ?? null,
    size: null,
    ...opts,
  };
}

function defaultArgs(overrides: Partial<Parameters<typeof flattenNavTree>[0]> = {}) {
  const tree = overrides.tree ?? buildTree("");
  return {
    tree,
    expandedPaths: new Set<string>(),
    graph: buildPipelineGraph(tree),
    viewerRootEntries: [] as FileEntry[],
    genericExpandedPaths: new Set<string>(),
    genericChildrenByPath: new Map<string, FileEntry[]>(),
    countsByAbsPath: computeCountsByAbsPath(tree),
    isDrilledIn: false,
    ...overrides,
  };
}

describe("flattenNavTree", () => {
  it("returns empty list for empty tree", () => {
    expect(flattenNavTree(defaultArgs())).toEqual([]);
  });

  // ----- R-14 Task 8 / AC #33 ----------------------------------------
  // Real-app repro: a story-typed file under `_bmad-output/docs/` must
  // route to a `story` nav item (not `doc` / `generic-file`).
  describe("R-14: docs/ story-typed file routes to StoryRow", () => {
    it("emits type:story for a docs/ file with `type: story` frontmatter", () => {
      const docsEpic = buildTree("docs", {
        absoluteDirPath: "/ctx/_bmad-output/docs",
        dirPath: "docs",
        stories: [
          buildStoryRef("test.md", {
            documentType: "story",
            status: "todo",
            absolutePath: "/ctx/_bmad-output/docs/test.md",
          }),
        ],
      });
      const tree = buildTree("", {
        absoluteDirPath: "/ctx/_bmad-output",
        subEpics: [docsEpic],
      });

      const items = flattenNavTree(
        defaultArgs({ tree, expandedPaths: new Set(["/ctx/_bmad-output/docs"]) }),
      );

      // Find the story under the expanded docs/ folder.
      const storyItem = items.find(
        (i): i is Extract<FlatNavItem, { type: "story" }> => i.type === "story",
      );
      expect(storyItem, "story-typed file should route to type:story").toBeDefined();
      expect(storyItem?.story.fileName).toBe("test.md");
      expect(storyItem?.story.status).toBe("todo");
    });

    // ---- R-14 code-review patch: double-render guard ----
    it("does NOT emit a generic-dir row for a viewerRoot subdir already represented as a sub-epic", () => {
      // Scanner emits a sub-epic at /ctx/_bmad-output/docs with md children.
      // viewerRootEntries (from listDirectory) also includes the docs dir.
      // After the dedup patch, the scanner sub-epic wins — no generic-dir.
      const docsEpic = buildTree("docs", {
        absoluteDirPath: "/ctx/_bmad-output/docs",
        dirPath: "docs",
        stories: [
          buildStoryRef("test.md", {
            documentType: "story",
            status: "todo",
            absolutePath: "/ctx/_bmad-output/docs/test.md",
          }),
        ],
      });
      const tree = buildTree("", {
        absoluteDirPath: "/ctx/_bmad-output",
        subEpics: [docsEpic],
      });

      const viewerRootEntries: FileEntry[] = [
        makeEntry("docs", "/ctx/_bmad-output", { is_dir: true, extension: null }),
      ];

      const items = flattenNavTree(defaultArgs({ tree, viewerRootEntries }));
      const types = items.map((i) => i.type);
      // Sub-epic folder-header present (collapsed), no generic-dir for the same path.
      expect(types).toContain("folder-header");
      expect(types).not.toContain("generic-dir");
    });

    it("emits a single generic-dir row for an md-empty viewerRoot subdir not represented as a sub-epic", () => {
      // viewerRootEntries lists a screenshots dir; the scanner did not emit
      // a sub-epic for it (md-empty). The dir should appear exactly once,
      // via generic-dir.
      const tree = buildTree("", { absoluteDirPath: "/ctx/_bmad-output" });
      const viewerRootEntries: FileEntry[] = [
        makeEntry("screenshots", "/ctx/_bmad-output", { is_dir: true, extension: null }),
      ];

      const items = flattenNavTree(defaultArgs({ tree, viewerRootEntries }));
      const genericDirs = items.filter((i) => i.type === "generic-dir");
      expect(genericDirs).toHaveLength(1);
      if (genericDirs[0].type === "generic-dir") {
        expect(genericDirs[0].entry.path).toBe("/ctx/_bmad-output/screenshots");
      }
    });

    it("emits type:doc for non-pipeline md files routed into tree.docs[]", () => {
      // PTS-2 follow-up: the Rust scanner now does the story/doc split via
      // the pipeline-type gate. `type: doc` / `type: retro` / `type:
      // llm-distillate` / no-frontmatter md files land in `tree.docs[]`,
      // not `tree.stories[]`. flattenNavTree iterates that field and emits
      // doc nav items — no TS-side `documentType` partition any more.
      const planning = buildTree("planning-artifacts", {
        absoluteDirPath: "/ctx/_bmad-output/planning-artifacts",
        dirPath: "planning-artifacts",
        docs: [
          {
            fileName: "architecture.md",
            filePath: "/ctx/_bmad-output/planning-artifacts/architecture.md",
          },
        ],
      });
      const tree = buildTree("", {
        absoluteDirPath: "/ctx/_bmad-output",
        subEpics: [planning],
      });

      const items = flattenNavTree(
        defaultArgs({
          tree,
          expandedPaths: new Set(["/ctx/_bmad-output/planning-artifacts"]),
        }),
      );

      const docItem = items.find(
        (i): i is Extract<FlatNavItem, { type: "doc" }> => i.type === "doc",
      );
      expect(docItem, "non-pipeline md should route to type:doc nav item").toBeDefined();
      expect(docItem?.filePath).toBe("/ctx/_bmad-output/planning-artifacts/architecture.md");
    });
  });

  it("emits collapsed folder-header without children", () => {
    const sub = buildTree("epic-a", {
      absoluteDirPath: "/repo/epic-a",
      dirPath: "epic-a",
      stories: [buildStoryRef("a.md", { status: "in-progress", title: "A" })],
    });
    const tree = buildTree("", { absoluteDirPath: "/repo", subEpics: [sub] });

    const items = flattenNavTree(defaultArgs({ tree }));
    expect(items).toHaveLength(1);
    expect(items[0].type).toBe("folder-header");
    if (items[0].type === "folder-header") {
      expect(items[0].node.absoluteDirPath).toBe("/repo/epic-a");
      expect(items[0].expanded).toBe(false);
      expect(items[0].depth).toBe(0);
      expect(items[0].total).toBe(1);
      expect(items[0].done).toBe(0);
    }
  });

  it("recurses into expanded sub-epics and indents children by depth", () => {
    const child = buildTree("child", {
      absoluteDirPath: "/repo/epic-a/child",
      dirPath: "epic-a/child",
      stories: [buildStoryRef("c.md", { status: "todo", title: "C" })],
    });
    const sub = buildTree("epic-a", {
      absoluteDirPath: "/repo/epic-a",
      dirPath: "epic-a",
      subEpics: [child],
      stories: [buildStoryRef("a.md", { status: "in-progress", title: "A" })],
    });
    const tree = buildTree("", { absoluteDirPath: "/repo", subEpics: [sub] });

    const items = flattenNavTree(
      defaultArgs({
        tree,
        expandedPaths: new Set(["/repo/epic-a", "/repo/epic-a/child"]),
      }),
    );

    // Expected order: epic-a header, child header, c story (depth 2), a story (depth 1)
    expect(items.map((i) => i.type)).toEqual(["folder-header", "folder-header", "story", "story"]);
    expect(items.map((i) => i.depth)).toEqual([0, 1, 2, 1]);
  });

  it("emits doc rows from tree.docs[] after stories at the same level", () => {
    // PTS-2 follow-up: rust scanner is the single source of truth for the
    // story/doc split via the pipeline-type gate. tree.stories[] now
    // contains only actionable types (story/bug/spike); tree.docs[]
    // contains non-pipeline md/html (no-type, doc, retro, etc.).
    const tree = buildTree("", {
      absoluteDirPath: "/repo",
      stories: [buildStoryRef("story.md", { status: "todo", title: "Story" })],
      docs: [
        { fileName: "notes.md", filePath: "/repo/notes.md" },
        { fileName: "retro.md", filePath: "/repo/retro.md" },
      ],
    });

    const items = flattenNavTree(defaultArgs({ tree }));
    expect(items.map((i) => i.type)).toEqual(["story", "doc", "doc"]);
  });

  it("emits doc rows for files in expanded sub-epics from tree.docs[]", () => {
    // PTS-2 follow-up: orphan-files hook retired; doc rows in expanded
    // sub-epic dirs now come from the scanner's per-node tree.docs[].
    const sub = buildTree("epic-a", {
      absoluteDirPath: "/repo/epic-a",
      dirPath: "epic-a",
      docs: [{ fileName: "notes.html", filePath: "/repo/epic-a/notes.html" }],
    });
    const tree = buildTree("", { absoluteDirPath: "/repo", subEpics: [sub] });

    const items = flattenNavTree(
      defaultArgs({
        tree,
        expandedPaths: new Set(["/repo/epic-a"]),
      }),
    );

    const docItems = items.filter(
      (i): i is Extract<FlatNavItem, { type: "doc" }> => i.type === "doc",
    );
    expect(docItems).toHaveLength(1);
    expect(docItems[0].filePath).toBe("/repo/epic-a/notes.html");
    expect(docItems[0].depth).toBe(1);
  });

  it("emits mainDoc as main-doc item at its level", () => {
    const tree = buildTree("", {
      absoluteDirPath: "/repo",
      mainDoc: {
        filePath: "/repo/README.md",
        supportingDocs: null,
        purposeText: null,
      },
    });

    const items = flattenNavTree(defaultArgs({ tree }));
    expect(items).toHaveLength(1);
    expect(items[0].type).toBe("main-doc");
    if (items[0].type === "main-doc") {
      expect(items[0].filePath).toBe("/repo/README.md");
      expect(items[0].label).toBe("README.md");
      expect(items[0].depth).toBe(0);
    }
  });

  it("inserts viewerRootEntries dirs before stories and files after mainDoc at root", () => {
    const tree = buildTree("", {
      absoluteDirPath: "/repo",
      stories: [buildStoryRef("a.md", { status: "todo", title: "A" })],
      mainDoc: {
        filePath: "/repo/README.md",
        supportingDocs: null,
        purposeText: null,
      },
    });

    const viewerRootEntries: FileEntry[] = [
      makeEntry("docs", "/viewer", { is_dir: true, extension: null }),
      makeEntry("planning.md", "/viewer", { extension: "md" }),
    ];

    const items = flattenNavTree(defaultArgs({ tree, viewerRootEntries }));

    // generic-dir, story, main-doc, generic-file
    expect(items.map((i) => i.type)).toEqual(["generic-dir", "story", "main-doc", "generic-file"]);
  });

  it("suppresses viewerRootEntries when isDrilledIn is true", () => {
    const tree = buildTree("", { absoluteDirPath: "/repo" });
    const viewerRootEntries: FileEntry[] = [
      makeEntry("docs", "/viewer", { is_dir: true, extension: null }),
      makeEntry("planning.md", "/viewer", { extension: "md" }),
    ];

    const items = flattenNavTree(defaultArgs({ tree, viewerRootEntries, isDrilledIn: true }));
    expect(items).toEqual([]);
  });

  it("recurses into expanded generic dirs at depth + 1", () => {
    const tree = buildTree("", { absoluteDirPath: "/repo" });
    const viewerRootEntries: FileEntry[] = [
      makeEntry("docs", "/viewer", { is_dir: true, extension: null }),
    ];
    const docsChildren: FileEntry[] = [
      makeEntry("sub-dir", "/viewer/docs", { is_dir: true, extension: null }),
      makeEntry("guide.md", "/viewer/docs", { extension: "md" }),
    ];

    const items = flattenNavTree(
      defaultArgs({
        tree,
        viewerRootEntries,
        genericExpandedPaths: new Set(["/viewer/docs"]),
        genericChildrenByPath: new Map([["/viewer/docs", docsChildren]]),
      }),
    );

    expect(items.map((i) => i.type)).toEqual([
      "generic-dir", // docs (depth 0)
      "generic-dir", // sub-dir (depth 1)
      "generic-file", // guide.md (depth 1)
    ]);
    expect(items.map((i) => i.depth)).toEqual([0, 1, 1]);
  });

  it("marks isReady=true for todo stories with met dependencies", () => {
    const sub = buildTree("epic-a", {
      absoluteDirPath: "/repo/epic-a",
      dirPath: "epic-a",
      stories: [
        buildStoryRef("F-1.md", { status: "done", title: "Done" }),
        buildStoryRef("F-2.md", {
          status: "todo",
          title: "Ready",
          dependsOn: ["F-1"],
        }),
        buildStoryRef("F-3.md", {
          status: "todo",
          title: "Blocked",
          dependsOn: ["F-99"],
        }),
      ],
    });
    const tree = buildTree("", { absoluteDirPath: "/repo", subEpics: [sub] });

    const items = flattenNavTree(
      defaultArgs({
        tree,
        expandedPaths: new Set(["/repo/epic-a"]),
      }),
    );

    const storyItems = items.filter(
      (i): i is Extract<FlatNavItem, { type: "story" }> => i.type === "story",
    );
    expect(storyItems).toHaveLength(3);
    expect(storyItems.map((s) => `${s.story.fileName}:${s.isReady}`)).toEqual([
      "F-1.md:false", // done → not ready
      "F-2.md:true", // todo + dep met
      "F-3.md:false", // todo + dep unresolved
    ]);
  });

  // SW-FX-1: the nav resolves readiness through the FULL graph even when the
  // display tree is a drilled subtree — so a cross-epic dep doesn't flip a
  // story to not-ready on drill.
  describe("SW-FX-1: readiness is full-graph even when display is drilled", () => {
    function fullTwoEpicTree(ss3Status: string) {
      return buildTree("", {
        absoluteDirPath: "/repo",
        subEpics: [
          buildTree("seamless-sync", {
            dirPath: "seamless-sync",
            absoluteDirPath: "/repo/seamless-sync",
            stories: [buildStoryRef("SS-3.md", { status: ss3Status, documentType: "story" })],
          }),
          buildTree("seamless-workspaces", {
            dirPath: "seamless-workspaces",
            absoluteDirPath: "/repo/seamless-workspaces",
            stories: [
              buildStoryRef("CP-1.md", {
                status: "todo",
                documentType: "story",
                dependsOn: ["../seamless-sync/SS-3"],
              }),
            ],
          }),
        ],
      });
    }

    it("marks the drilled story ready when its off-scope dep is done", () => {
      const full = fullTwoEpicTree("done");
      const graph = buildPipelineGraph(full);
      const scoped = findScopedSubtree(full, "seamless-workspaces")!;

      const items = flattenNavTree(defaultArgs({ tree: scoped, graph }));
      const cp1 = items.find(
        (i): i is Extract<FlatNavItem, { type: "story" }> =>
          i.type === "story" && i.story.fileName === "CP-1.md",
      );
      expect(cp1?.isReady).toBe(true);
      expect(cp1?.blockedBy).toEqual([]);
    });

    it("keeps the drilled story not-ready and names the off-screen blocker when the dep is unmet", () => {
      const full = fullTwoEpicTree("in-progress");
      const graph = buildPipelineGraph(full);
      const scoped = findScopedSubtree(full, "seamless-workspaces")!;

      const items = flattenNavTree(defaultArgs({ tree: scoped, graph }));
      const cp1 = items.find(
        (i): i is Extract<FlatNavItem, { type: "story" }> =>
          i.type === "story" && i.story.fileName === "CP-1.md",
      );
      expect(cp1?.isReady).toBe(false);
      expect(cp1?.blockedBy).toEqual([
        { ref: "../seamless-sync/SS-3", reason: "unmet", status: "in-progress" },
      ]);
    });
  });

  // ----- SW-XS-19: source-repo + owning-team markers threaded onto doc items ----
  describe("doc provenance plumbing", () => {
    it("threads the source repo, shadow flag and owning team onto the doc nav item", () => {
      const tree = {
        dirPath: "",
        dirName: "",
        absoluteDirPath: "/ctx",
        stories: [],
        docs: [
          {
            fileName: "prd.md",
            filePath: "/src/prd.md",
            installedSourceRepo: "alderco/codesense",
            shadowsTeamFile: true,
          },
          { fileName: "skill.md", filePath: "/ctx/skill.md", sourceTeam: "platform" },
        ],
        subEpics: [],
        orderError: null,
        mainDoc: null,
      };
      const items = flattenNavTree(defaultArgs({ tree }));
      const docs = items.filter(
        (i): i is Extract<FlatNavItem, { type: "doc" }> => i.type === "doc",
      );
      const prd = docs.find((d) => d.filePath === "/src/prd.md");
      expect(prd?.sourceRepo).toBe("alderco/codesense");
      expect(prd?.shadows).toBe(true);
      expect(prd?.sourceTeam).toBeUndefined();
      expect(docs.find((d) => d.filePath === "/ctx/skill.md")?.sourceTeam).toBe("platform");
    });
  });

  // SLW-NC-7 -----------------------------------------------------------------
  describe("collectPinTargets", () => {
    // Root anchor → repo-relative key space (strip `/ctx/`).
    const toRel = (abs: string) => (abs.startsWith("/ctx/") ? abs.slice("/ctx/".length) : null);

    it("collects dirs and non-story files as pin targets, keyed repo-relative", () => {
      const node = buildTree("epic-a", { dirPath: "epic-a", absoluteDirPath: "/ctx/epic-a" });
      // A flat list mixing a folder-header (dir), a doc (file), a generic-file, and
      // a story row (which must NOT become a pin target).
      const flat: FlatNavItem[] = [
        { type: "folder-header", node, depth: 0, expanded: false, total: 1, done: 0 },
        {
          type: "doc",
          filePath: "/ctx/epic-a/notes.md",
          label: "notes",
          depth: 1,
          installed: false,
        },
        { type: "generic-file", entry: makeEntry("todo.txt", "/ctx"), depth: 0 },
        {
          type: "story",
          story: buildStoryRef("s1.md", { absolutePath: "/ctx/epic-a/s1.md" }),
          depth: 1,
          isReady: true,
          blockedBy: [],
        },
      ];
      const map = collectPinTargets(flat, toRel);
      expect(map.get("epic-a")).toEqual({ isDir: true, label: "epic-a" });
      expect(map.get("epic-a/notes.md")).toEqual({ isDir: false, label: "notes" });
      expect(map.get("todo.txt")).toEqual({ isDir: false, label: "todo.txt" });
      // No story entry.
      expect([...map.keys()]).not.toContain("epic-a/s1.md");
    });

    it("skips paths outside the anchor (toRelative → null)", () => {
      const flat: FlatNavItem[] = [
        { type: "doc", filePath: "/other/root/x.md", label: "x", depth: 0, installed: false },
      ];
      expect(collectPinTargets(flat, toRel).size).toBe(0);
    });
  });

  describe("collectTreePinTargets", () => {
    const toRel = (abs: string) => (abs.startsWith("/ctx/") ? abs.slice("/ctx/".length) : null);

    it("resolves nested sub-epics and docs regardless of expand state (wholeness)", () => {
      // A nested epic + doc that would NOT be in a collapsed flat list.
      const deep = buildTree("deep-epic", {
        dirPath: "top/deep-epic",
        absoluteDirPath: "/ctx/top/deep-epic",
        docs: [{ fileName: "spec.md", filePath: "/ctx/top/deep-epic/spec.md" }],
      });
      const top = buildTree("top", {
        dirPath: "top",
        absoluteDirPath: "/ctx/top",
        subEpics: [deep],
      });
      const tree = buildTree("", { absoluteDirPath: "/ctx", subEpics: [top] });

      const map = collectTreePinTargets(tree, toRel);
      // The nested epic dir + its doc are BOTH resolvable even though nothing is expanded.
      expect(map.get("top")).toEqual({ isDir: true, label: "top" });
      expect(map.get("top/deep-epic")).toEqual({ isDir: true, label: "deep-epic" });
      expect(map.get("top/deep-epic/spec.md")).toEqual({ isDir: false, label: "spec" });
    });
  });

  describe("prependPinnedSection", () => {
    const pin = (over: Partial<PinnedNavItem>): PinnedNavItem => ({
      path: "epic-a",
      label: "epic-a",
      isDir: true,
      scope: "team",
      stale: false,
      ...over,
    });
    const base: FlatNavItem[] = [
      { type: "generic-file", entry: makeEntry("root.md", "/ctx"), depth: 0 },
    ];

    it("returns the list unchanged when there are no pins (hidden when empty)", () => {
      expect(prependPinnedSection(base, [], false)).toBe(base);
    });

    it("emits header + one row per pin when expanded", () => {
      const out = prependPinnedSection(base, [pin({}), pin({ path: "b", label: "b" })], false);
      expect(out[0]).toEqual({ type: "pinned-header", depth: 0, count: 2, collapsed: false });
      expect(out.filter((i) => i.type === "pinned-row")).toHaveLength(2);
      // Real tree rows still follow.
      expect(out[out.length - 1].type).toBe("generic-file");
    });

    it("emits only the header (with count) when collapsed", () => {
      const out = prependPinnedSection(base, [pin({})], true);
      expect(out[0]).toEqual({ type: "pinned-header", depth: 0, count: 1, collapsed: true });
      expect(out.some((i) => i.type === "pinned-row")).toBe(false);
    });
  });

  // -- SW-VE-1 AC-3: expanded generic dir → loading / empty / populated -------
  describe("SW-VE-1: expanded generic-dir placeholder rows", () => {
    const dir = makeEntry("config", "/ctx", { is_dir: true, extension: null });

    function argsWith(children: Map<string, FileEntry[]>) {
      return defaultArgs({
        viewerRootEntries: [dir],
        genericExpandedPaths: new Set([dir.path]),
        genericChildrenByPath: children,
      });
    }

    it("emits a skeleton row while children are still loading (unfetched)", () => {
      const out = flattenNavTree(argsWith(new Map()));
      expect(out.map((i) => i.type)).toEqual(["generic-dir", "generic-loading"]);
      const loading = out[1];
      expect(loading).toMatchObject({ type: "generic-loading", parentPath: dir.path });
    });

    it("emits a muted 'Empty folder' row when the dir resolves to zero children", () => {
      const out = flattenNavTree(argsWith(new Map([[dir.path, []]])));
      expect(out.map((i) => i.type)).toEqual(["generic-dir", "generic-empty"]);
      expect(out[1]).toMatchObject({ type: "generic-empty", parentPath: dir.path });
    });

    it("emits the file rows (no placeholder) once children resolve", () => {
      const out = flattenNavTree(
        argsWith(new Map([[dir.path, [makeEntry("core.yaml", dir.path, { extension: "yaml" })]]])),
      );
      expect(out.map((i) => i.type)).toEqual(["generic-dir", "generic-file"]);
      expect(out.some((i) => i.type === "generic-empty" || i.type === "generic-loading")).toBe(
        false,
      );
    });
  });
});

// ----- SW-LNS-1: left-nav search overlay ----------------------------------
describe("buildNavSearchFilter + flattenNavTree(search)", () => {
  it("returns null for an empty / whitespace-only query", () => {
    const tree = buildTree("");
    expect(
      buildNavSearchFilter({
        query: "",
        tree,
        viewerRootEntries: [],
        genericChildrenByPath: new Map(),
        isDrilledIn: false,
      }),
    ).toBeNull();
    expect(
      buildNavSearchFilter({
        query: "   ",
        tree,
        viewerRootEntries: [],
        genericChildrenByPath: new Map(),
        isDrilledIn: false,
      }),
    ).toBeNull();
  });

  it("surfaces a matching story and auto-expands its ancestor sub-epic", () => {
    const docsEpic = buildTree("docs", {
      absoluteDirPath: "/ctx/_bmad-output/docs",
      dirPath: "docs",
      stories: [
        buildStoryRef("green-finder.md", {
          documentType: "story",
          status: "todo",
          absolutePath: "/ctx/_bmad-output/docs/green-finder.md",
        }),
        buildStoryRef("red-herring.md", {
          documentType: "story",
          status: "todo",
          absolutePath: "/ctx/_bmad-output/docs/red-herring.md",
        }),
      ],
    });
    const tree = buildTree("", {
      absoluteDirPath: "/ctx/_bmad-output",
      subEpics: [docsEpic],
    });
    const search = buildNavSearchFilter({
      query: "green",
      tree,
      viewerRootEntries: [],
      genericChildrenByPath: new Map(),
      isDrilledIn: false,
    });
    expect(search).not.toBeNull();
    const items = flattenNavTree(defaultArgs({ tree, search: search ?? undefined }));
    const types = items.map((i) => i.type);
    expect(types).toContain("folder-header");
    const storyItems = items.filter(
      (i): i is Extract<FlatNavItem, { type: "story" }> => i.type === "story",
    );
    expect(storyItems).toHaveLength(1);
    expect(storyItems[0].story.fileName).toBe("green-finder.md");
    const folder = items.find(
      (i): i is Extract<FlatNavItem, { type: "folder-header" }> => i.type === "folder-header",
    );
    expect(folder?.expanded).toBe(true);
  });

  it("hides a sub-epic with no matching descendant", () => {
    const greenEpic = buildTree("green", {
      absoluteDirPath: "/ctx/_bmad-output/green",
      dirPath: "green",
      stories: [
        buildStoryRef("green-finder.md", {
          documentType: "story",
          status: "todo",
          absolutePath: "/ctx/_bmad-output/green/green-finder.md",
        }),
      ],
    });
    const redEpic = buildTree("red", {
      absoluteDirPath: "/ctx/_bmad-output/red",
      dirPath: "red",
      stories: [
        buildStoryRef("red-herring.md", {
          documentType: "story",
          status: "todo",
          absolutePath: "/ctx/_bmad-output/red/red-herring.md",
        }),
      ],
    });
    const tree = buildTree("", {
      absoluteDirPath: "/ctx/_bmad-output",
      subEpics: [greenEpic, redEpic],
    });
    const search = buildNavSearchFilter({
      query: "green",
      tree,
      viewerRootEntries: [],
      genericChildrenByPath: new Map(),
      isDrilledIn: false,
    });
    const items = flattenNavTree(defaultArgs({ tree, search: search ?? undefined }));
    const folders = items.filter(
      (i): i is Extract<FlatNavItem, { type: "folder-header" }> => i.type === "folder-header",
    );
    expect(folders).toHaveLength(1);
    expect(folders[0].node.absoluteDirPath).toBe("/ctx/_bmad-output/green");
  });

  it("matches on dir name — the folder header is shown but non-matching children stay hidden", () => {
    const greenEpic = buildTree("green", {
      absoluteDirPath: "/ctx/_bmad-output/green",
      dirPath: "green",
      stories: [
        buildStoryRef("whatever.md", {
          documentType: "story",
          status: "todo",
          absolutePath: "/ctx/_bmad-output/green/whatever.md",
        }),
      ],
    });
    const tree = buildTree("", {
      absoluteDirPath: "/ctx/_bmad-output",
      subEpics: [greenEpic],
    });
    const search = buildNavSearchFilter({
      query: "GRE",
      tree,
      viewerRootEntries: [],
      genericChildrenByPath: new Map(),
      isDrilledIn: false,
    });
    const items = flattenNavTree(defaultArgs({ tree, search: search ?? undefined }));
    expect(items.some((i) => i.type === "folder-header")).toBe(true);
    expect(items.some((i) => i.type === "story")).toBe(false);
  });

  it("is case-insensitive on file names", () => {
    const docsEpic = buildTree("docs", {
      absoluteDirPath: "/ctx/_bmad-output/docs",
      dirPath: "docs",
      stories: [
        buildStoryRef("MixedCase.md", {
          documentType: "story",
          status: "todo",
          absolutePath: "/ctx/_bmad-output/docs/MixedCase.md",
        }),
      ],
    });
    const tree = buildTree("", {
      absoluteDirPath: "/ctx/_bmad-output",
      subEpics: [docsEpic],
    });
    const search = buildNavSearchFilter({
      query: "mixedcase",
      tree,
      viewerRootEntries: [],
      genericChildrenByPath: new Map(),
      isDrilledIn: false,
    });
    const items = flattenNavTree(defaultArgs({ tree, search: search ?? undefined }));
    expect(items.some((i) => i.type === "story")).toBe(true);
  });
});

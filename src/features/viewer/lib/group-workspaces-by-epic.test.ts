// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  buildEpicDocPathIndex,
  trayWorkspaceOrder,
  railKeyOf,
  railInfo,
  epicRailEntries,
  epicChildEntries,
  pickMruResumeTarget,
  railsFromOrder,
  resolveAdjacentEligibleRail,
  resolveAdjacentWorkspaceInScope,
  resolveSelectedRailKey,
  ROOTS_RAIL_KEY,
  type EpicRailMembers,
} from "./group-workspaces-by-epic";
import type { SoftClosedWorkspace, Workspace } from "@/shared/stores/viewer-store";
import type { WorkspaceKind } from "@/shared/lib/workspace-kind-capabilities";
import type { DerivedPipelineTree, MainDoc } from "@/shared/lib/pipeline-graph/types";

const mainDoc = (filePath: string): MainDoc => ({
  filePath,
  supportingDocs: null,
  purposeText: null,
});

function node(partial: Partial<DerivedPipelineTree>): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...partial,
  };
}

function storyRef(filePath: string, sourceRepo: string | null = null) {
  return {
    fileName: filePath.split("/").pop()!,
    filePath,
    dependsOn: null,
    status: null,
    documentType: null,
    supportingDocs: null,
    sourceRepo,
    team: null,
  };
}

const story = (id: string, storyFilePath?: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/wt/${id}`,
  storyFilePath,
  storyId: id,
  status: "ready",
});

const tree = node({
  subEpics: [
    node({
      dirName: "alpha",
      dirPath: "in-progress/alpha",
      stories: [storyRef("/r/alpha/A-1.md"), storyRef("/r/alpha/A-2.md")],
    }),
    node({
      dirName: "beta",
      dirPath: "in-progress/beta",
      stories: [storyRef("/r/beta/B-1.md")],
    }),
  ],
});

describe("buildEpicDocPathIndex", () => {
  const docTree = node({
    subEpics: [
      node({ dirName: "alpha", mainDoc: mainDoc("/r/alpha/README.md") }),
      node({ dirName: "beta", mainDoc: mainDoc("/r/beta/index.md") }),
      node({ dirName: "gamma", mainDoc: null }),
    ],
  });

  it("maps each epic dirName to its mainDoc filePath", () => {
    const index = buildEpicDocPathIndex(docTree);
    expect(index.get("alpha")).toBe("/r/alpha/README.md");
    expect(index.get("beta")).toBe("/r/beta/index.md");
  });

  it("omits dirs without a mainDoc", () => {
    const index = buildEpicDocPathIndex(docTree);
    expect(index.has("gamma")).toBe(false);
  });

  it("returns an empty index for a null tree", () => {
    expect(buildEpicDocPathIndex(null).size).toBe(0);
  });
});

describe("trayWorkspaceOrder (SW-EW-33)", () => {
  const root = (id: string): Workspace => ({ id, kind: "root", cwd: `/${id}`, status: "ready" });
  const ids = (entries: { workspace: Workspace }[]) => entries.map((e) => e.workspace.id);
  const soft = (ws: Workspace): SoftClosedWorkspace => ({ workspace: ws });

  it("roots first, then epic-grouped story/integration; spans open ∪ minimized", () => {
    const open = [story("A-1", "/r/alpha/A-1.md"), root("root-1")];
    const softClosed = { "B-1": soft(story("B-1", "/r/beta/B-1.md")) };
    const out = trayWorkspaceOrder(open, softClosed, tree);
    expect(ids(out)).toEqual(["root-1", "A-1", "B-1"]); // root, alpha group, beta group
    expect(out.find((e) => e.workspace.id === "B-1")?.minimized).toBe(true);
    expect(out.find((e) => e.workspace.id === "A-1")?.minimized).toBe(false);
  });

  it("includes kind:epic (now a real terminal workspace, no longer excluded)", () => {
    const open: Workspace[] = [
      { id: "epic-x", kind: "epic", cwd: "/e", status: "ready" },
      story("A-1", "/r/alpha/A-1.md"),
    ];
    // A-1 groups under alpha; the epic (no epicDirPath) falls in the trailing "" bucket.
    expect(ids(trayWorkspaceOrder(open, {}, tree))).toEqual(["A-1", "epic-x"]);
  });

  it("orders by id ALONE within a group — minimized does NOT sort after open (SW-EW-35)", () => {
    // Same epic 'alpha': A-2 open, A-1 minimized. The slot is the id, so A-1
    // stays FIRST even though it's the minimized one (no open-before-minimized).
    const open = [story("A-2", "/r/alpha/A-2.md")];
    const softClosed = { "A-1": soft(story("A-1", "/r/alpha/A-1.md")) };
    expect(ids(trayWorkspaceOrder(open, softClosed, tree))).toEqual(["A-1", "A-2"]);
  });

  it("a chip keeps its slot when it flips open↔minimized (SW-EW-35 stable slots)", () => {
    const a1 = story("A-1", "/r/alpha/A-1.md");
    const a2 = story("A-2", "/r/alpha/A-2.md");
    // Both open → [A-1, A-2].
    const bothOpen = ids(trayWorkspaceOrder([a1, a2], {}, tree));
    // A-1 minimized, A-2 open (the activate/replace swap) → SAME order.
    const a1Minimized = ids(trayWorkspaceOrder([a2], { "A-1": soft(a1) }, tree));
    // A-2 minimized, A-1 open → still SAME order.
    const a2Minimized = ids(trayWorkspaceOrder([a1], { "A-2": soft(a2) }, tree));
    expect(bothOpen).toEqual(["A-1", "A-2"]);
    expect(a1Minimized).toEqual(["A-1", "A-2"]);
    expect(a2Minimized).toEqual(["A-1", "A-2"]);
  });

  it("group order is stable regardless of minimize/restore churn (same set → same order)", () => {
    const a1 = story("A-1", "/r/alpha/A-1.md");
    const a2 = story("A-2", "/r/alpha/A-2.md");
    const openFirst = trayWorkspaceOrder([a1, a2], {}, tree);
    const openReordered = trayWorkspaceOrder([a2, a1], {}, tree);
    expect(ids(openFirst)).toEqual(ids(openReordered)); // id-stable, not insertion-order
  });

  it("orders epic GROUPS by pipeline-tree position, not smallest child id (SW-EW-35 / EC#2)", () => {
    // Tree order is alpha THEN beta, but give beta the smaller workspace id.
    // Min-child-id would float beta first; tree order must keep alpha first.
    const alphaWs = story("Z-1", "/r/alpha/A-1.md"); // big id, alpha (tree pos 0)
    const betaWs = story("A-1", "/r/beta/B-1.md"); // small id, beta (tree pos 1)
    expect(ids(trayWorkspaceOrder([betaWs, alphaWs], {}, tree))).toEqual(["Z-1", "A-1"]);
  });

  it("a new group slots at its tree position; existing groups don't jump (EC#2)", () => {
    const a1 = story("A-1", "/r/alpha/A-1.md"); // alpha (tree pos 0)
    const b1 = story("B-1", "/r/beta/B-1.md"); // beta (tree pos 1)
    expect(ids(trayWorkspaceOrder([b1], {}, tree))).toEqual(["B-1"]); // only beta open
    // Opening an alpha workspace inserts alpha BEFORE beta (its tree slot).
    expect(ids(trayWorkspaceOrder([b1, a1], {}, tree))).toEqual(["A-1", "B-1"]);
  });
});

describe("SW-EW-38 — rail + per-epic slice helpers (derived from trayWorkspaceOrder)", () => {
  const root = (id: string, slug?: string): Workspace => ({
    id,
    kind: "root",
    cwd: `/${id}`,
    sourceRepoSlug: slug,
    status: "ready",
  });
  const soft = (ws: Workspace): SoftClosedWorkspace => ({ workspace: ws });
  const railKeys = (entries: ReturnType<typeof epicRailEntries>) => entries.map((r) => r.railKey);

  describe("railKeyOf — disambiguates roots-'' from Ungrouped-'' (R2)", () => {
    it("a root entry maps to ROOTS_RAIL_KEY (not '')", () => {
      const [entry] = trayWorkspaceOrder([root("root-1")], {}, tree);
      expect(railKeyOf(entry)).toBe(ROOTS_RAIL_KEY);
      expect(ROOTS_RAIL_KEY).not.toBe(""); // sentinel can't collide with Ungrouped
    });
    it("an Ungrouped (unresolved) story entry maps to ''", () => {
      const [entry] = trayWorkspaceOrder([story("X-1", "/unknown/X-1.md")], {}, tree);
      expect(railKeyOf(entry)).toBe("");
    });
    it("an epic member maps to its dirName", () => {
      const [entry] = trayWorkspaceOrder([story("A-1", "/r/alpha/A-1.md")], {}, tree);
      expect(railKeyOf(entry)).toBe("alpha");
    });
  });

  describe("epicRailEntries", () => {
    it("roots-only → one '~' roots pill", () => {
      const entries = trayWorkspaceOrder([root("root-1", "acme/web")], {}, tree);
      const rail = epicRailEntries(entries);
      expect(rail).toHaveLength(1);
      expect(rail[0].railKey).toBe(ROOTS_RAIL_KEY);
      expect(rail[0].title).toBe("acme/web"); // single root → its slug
      expect(rail[0].members.map((m) => m.workspace.id)).toEqual(["root-1"]);
      expect(rail[0].hasOpen).toBe(true);
    });

    it("single epic → prettified title + member count", () => {
      const entries = trayWorkspaceOrder(
        [story("A-1", "/r/alpha/A-1.md"), story("A-2", "/r/alpha/A-2.md")],
        {},
        tree,
      );
      const rail = epicRailEntries(entries);
      expect(rail).toHaveLength(1);
      expect(rail[0].railKey).toBe("alpha");
      expect(rail[0].title).toBe("Alpha"); // prettifyDirLeaf
      expect(rail[0].members).toHaveLength(2);
    });

    it("multi-epic → pills in pipeline-tree order (roots first, Ungrouped last)", () => {
      const entries = trayWorkspaceOrder(
        [
          story("B-1", "/r/beta/B-1.md"), // beta (tree pos 1)
          story("A-1", "/r/alpha/A-1.md"), // alpha (tree pos 0)
          story("X-1", "/unknown/X-1.md"), // ungrouped
          root("root-1"), // roots
        ],
        {},
        tree,
      );
      // Rail order falls straight out of the single ordered array.
      expect(railKeys(epicRailEntries(entries))).toEqual([ROOTS_RAIL_KEY, "alpha", "beta", ""]);
    });

    it("roots + Ungrouped coexist as TWO distinct pills (R2 regression guard)", () => {
      const entries = trayWorkspaceOrder(
        [root("root-1"), story("X-1", "/unknown/X-1.md")],
        {},
        tree,
      );
      const rail = epicRailEntries(entries);
      expect(rail).toHaveLength(2);
      expect(railKeys(rail)).toEqual([ROOTS_RAIL_KEY, ""]);
      // The Ungrouped pill is titled, the roots pill is "~"-style — never merged.
      expect(rail.find((r) => r.railKey === "")?.title).toBe("Ungrouped");
    });

    it("a minimized-only group still gets a (dimmed) pill: hasOpen === false", () => {
      const entries = trayWorkspaceOrder(
        [],
        { "A-1": soft(story("A-1", "/r/alpha/A-1.md")) },
        tree,
      );
      const rail = epicRailEntries(entries);
      expect(rail).toHaveLength(1);
      expect(rail[0].railKey).toBe("alpha");
      expect(rail[0].hasOpen).toBe(false);
    });
  });

  describe("epicChildEntries — the selected epic's row-2 slice (single source)", () => {
    const entries = trayWorkspaceOrder(
      [
        story("A-2", "/r/alpha/A-2.md"),
        story("A-1", "/r/alpha/A-1.md"),
        story("B-1", "/r/beta/B-1.md"),
        root("root-1"),
        story("X-1", "/unknown/X-1.md"),
      ],
      {},
      tree,
    );

    it("returns only the selected epic's members, in source order", () => {
      expect(epicChildEntries(entries, "alpha").map((e) => e.workspace.id)).toEqual(["A-1", "A-2"]);
      expect(epicChildEntries(entries, "beta").map((e) => e.workspace.id)).toEqual(["B-1"]);
    });

    it("roots key and Ungrouped key select disjoint slices (R2)", () => {
      expect(epicChildEntries(entries, ROOTS_RAIL_KEY).map((e) => e.workspace.id)).toEqual([
        "root-1",
      ]);
      expect(epicChildEntries(entries, "").map((e) => e.workspace.id)).toEqual(["X-1"]);
    });

    it("the child slice is an in-order subsequence of the source array (single-source invariant)", () => {
      const sourceIds = entries.map((e) => e.workspace.id);
      for (const key of [ROOTS_RAIL_KEY, "alpha", "beta", ""]) {
        const sliceIds = epicChildEntries(entries, key).map((e) => e.workspace.id);
        // every slice preserves the relative order of the source
        const positions = sliceIds.map((id) => sourceIds.indexOf(id));
        expect(positions).toEqual([...positions].sort((a, b) => a - b));
      }
    });

    it("an unknown rail key → empty slice (caller falls back to first pill)", () => {
      expect(epicChildEntries(entries, "nonexistent")).toEqual([]);
    });
  });

  describe("pickMruResumeTarget — MRU member, else integration, else first child (AC-6)", () => {
    const integration: Workspace = {
      id: "integration:alpha",
      kind: "epic",
      cwd: "/repo",
      epicDirPath: "alpha",
      status: "ready",
    };
    // OC-6: pickMruResumeTarget now takes bare {id,kind} — map the tray entries'
    // workspaces so the same fixtures exercise the generalized signature.
    const alphaMembers = trayWorkspaceOrder(
      [story("A-2", "/r/alpha/A-2.md"), story("A-1", "/r/alpha/A-1.md"), integration],
      {},
      tree,
    )
      .filter((e) => e.epicDirName === "alpha")
      .map((e) => e.workspace); // [integration, A-1, A-2] in source order

    it("prefers the most-recent member present in the MRU stack", () => {
      expect(pickMruResumeTarget(alphaMembers, ["B-1", "A-2", "A-1"])).toBe("A-2");
    });

    it("falls back to the integration/planning workspace when no member is in the MRU stack", () => {
      expect(pickMruResumeTarget(alphaMembers, ["B-1", "elsewhere"])).toBe("integration:alpha");
    });

    it("falls back to the first child by source order when there is no integration", () => {
      const noIntegration = alphaMembers.filter((m) => m.kind !== "epic");
      // first by source order; within a group trayWorkspaceOrder is id-sorted, so A-1.
      expect(pickMruResumeTarget(noIntegration, [])).toBe("A-1");
    });

    it("returns undefined for an empty group", () => {
      expect(pickMruResumeTarget([], ["A-1"])).toBeUndefined();
    });
  });

  // OC-6 — keyboard epic-group navigation: rail reconstruction + adjacency.
  describe("railsFromOrder — group an ordered id list by rail key (no tree)", () => {
    const kindOf = (id: string): WorkspaceKind =>
      id.startsWith("integration:") ? "epic" : id.startsWith("root:") ? "root" : "story";
    const lookup = (id: string) => ({ id, kind: kindOf(id) });

    it("reproduces epicRailEntries order from the flat union order + rail-key map", () => {
      const orderedIds = ["root:r", "integration:alpha", "A-1", "B-1", "U-1"];
      const railKeyById: Record<string, string> = {
        "root:r": ROOTS_RAIL_KEY,
        "integration:alpha": "alpha",
        "A-1": "alpha",
        "B-1": "beta",
        "U-1": "",
      };
      const rails = railsFromOrder(orderedIds, railKeyById, lookup);
      expect(rails.map((r) => r.railKey)).toEqual([ROOTS_RAIL_KEY, "alpha", "beta", ""]);
      expect(rails[1].members.map((m) => m.id)).toEqual(["integration:alpha", "A-1"]);
    });

    it("skips ids with no resolvable workspace", () => {
      const rails = railsFromOrder(["A-1", "ghost"], { "A-1": "alpha", ghost: "alpha" }, (id) =>
        id === "ghost" ? undefined : lookup(id),
      );
      expect(rails).toEqual([{ railKey: "alpha", members: [{ id: "A-1", kind: "story" }] }]);
    });
  });

  it("rail stepping and MRU resolution include a custom group between roots and epics", () => {
    const ids = ["root", "check-a", "check-b", "epic-a"];
    const keys = {
      root: ROOTS_RAIL_KEY,
      "check-a": " group:checks",
      "check-b": " group:checks",
      "epic-a": "alpha",
    };
    const rails = railsFromOrder(ids, keys, (id) => ({
      id,
      kind: id === "root" ? "root" : "story",
    }));
    const target = resolveAdjacentEligibleRail(rails, ROOTS_RAIL_KEY, "next", ["check-b"]);
    expect(target?.railKey).toBe(" group:checks");
    expect(pickMruResumeTarget(target?.members ?? [], ["check-b", "epic-a"])).toBe("check-b");
    expect(resolveAdjacentEligibleRail(rails, "alpha", "prev", [])?.railKey).toBe(" group:checks");
  });
  describe("resolveAdjacentEligibleRail — wrap + skip ineligible (AC1)", () => {
    const rail = (railKey: string, ids: string[]): EpicRailMembers => ({
      railKey,
      members: ids.map((id) => ({ id, kind: "story" as const })),
    });
    // alpha + gamma eligible; beta is empty (ineligible — no members).
    const rails = [rail("alpha", ["A-1"]), rail("beta", []), rail("gamma", ["G-1"])];

    it("next skips the ineligible middle rail", () => {
      expect(resolveAdjacentEligibleRail(rails, "alpha", "next", [])?.railKey).toBe("gamma");
    });

    it("prev wraps from the first eligible rail to the last", () => {
      expect(resolveAdjacentEligibleRail(rails, "alpha", "prev", [])?.railKey).toBe("gamma");
    });

    it("next wraps from the last eligible rail to the first", () => {
      expect(resolveAdjacentEligibleRail(rails, "gamma", "next", [])?.railKey).toBe("alpha");
    });

    it("a single eligible rail is a no-op (undefined)", () => {
      const one = [rail("alpha", ["A-1"]), rail("beta", [])];
      expect(resolveAdjacentEligibleRail(one, "alpha", "next", [])).toBeUndefined();
      expect(resolveAdjacentEligibleRail(one, "alpha", "prev", [])).toBeUndefined();
    });

    it("an empty rail list is a no-op (undefined)", () => {
      expect(resolveAdjacentEligibleRail([], "alpha", "next", [])).toBeUndefined();
    });
  });

  describe("resolveAdjacentWorkspaceInScope — workspace-axis order + wrap (OC-7 AC3/AC4)", () => {
    const scope = ["a", "b", "c"];

    it("steps forward in scope order", () => {
      expect(resolveAdjacentWorkspaceInScope(scope, "a", "next")).toBe("b");
      expect(resolveAdjacentWorkspaceInScope(scope, "b", "next")).toBe("c");
    });

    it("steps backward in scope order", () => {
      expect(resolveAdjacentWorkspaceInScope(scope, "c", "prev")).toBe("b");
      expect(resolveAdjacentWorkspaceInScope(scope, "b", "prev")).toBe("a");
    });

    it("wraps last↔first", () => {
      expect(resolveAdjacentWorkspaceInScope(scope, "c", "next")).toBe("a");
      expect(resolveAdjacentWorkspaceInScope(scope, "a", "prev")).toBe("c");
    });

    it("a scope of ≤1 is a no-op (undefined)", () => {
      expect(resolveAdjacentWorkspaceInScope(["only"], "only", "next")).toBeUndefined();
      expect(resolveAdjacentWorkspaceInScope([], "x", "prev")).toBeUndefined();
    });

    it("a current id outside the scope lands on the near end", () => {
      expect(resolveAdjacentWorkspaceInScope(scope, "zzz", "next")).toBe("a");
      expect(resolveAdjacentWorkspaceInScope(scope, "zzz", "prev")).toBe("c");
    });
  });

  describe("resolveSelectedRailKey — AC-8 stale-selection fallback (OC-7)", () => {
    it("keeps the stored key when it is still visible", () => {
      expect(resolveSelectedRailKey(["epicA", "epicB"], "epicB")).toBe("epicB");
    });

    it("falls back to the first visible rail when the stored key is gone", () => {
      expect(resolveSelectedRailKey(["epicA", "epicB"], "stale")).toBe("epicA");
    });

    it("falls back to the first visible rail when the stored key is unset", () => {
      expect(resolveSelectedRailKey(["epicA", "epicB"], "")).toBe("epicA");
    });

    it("returns empty string when there are no visible rails", () => {
      expect(resolveSelectedRailKey([], "epicA")).toBe("");
    });

    it("tolerates duplicate keys (per-id order input)", () => {
      expect(resolveSelectedRailKey(["epicA", "epicA", "epicB"], "epicB")).toBe("epicB");
      expect(resolveSelectedRailKey(["epicA", "epicA", "epicB"], "stale")).toBe("epicA");
    });
  });
});

describe("the ROOTS rail pill leads whatever the person's order", () => {
  const rootWs: Workspace = {
    id: "root-1",
    kind: "root",
    cwd: "/r",
    sourceRepoSlug: "acme/web",
    status: "ready",
  };
  const storyWs: Workspace = {
    id: "A-1",
    kind: "story",
    cwd: "/A-1",
    storyFilePath: "/r/alpha/A-1.md",
    status: "ready",
  };
  const alphaTree: DerivedPipelineTree = {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    orderError: null,
    mainDoc: null,
    subEpics: [
      {
        dirPath: "alpha",
        dirName: "alpha",
        absoluteDirPath: "",
        stories: [
          {
            fileName: "A-1.md",
            filePath: "/r/alpha/A-1.md",
            dependsOn: null,
            status: null,
            documentType: null,
            supportingDocs: null,
            sourceRepo: null,
            team: null,
          },
        ],
        subEpics: [],
        orderError: null,
        mainDoc: null,
      },
    ],
  };

  it("a story moved ahead of the root (a flat-mode drag) leaves ROOTS first on the rail", () => {
    const [root, story] = trayWorkspaceOrder([rootWs, storyWs], {}, alphaTree);
    expect(epicRailEntries([story!, root!]).map((r) => r.railKey)).toEqual([
      ROOTS_RAIL_KEY,
      "alpha",
    ]);
  });

  it("the keyboard's rails read the same order", () => {
    const rails = railsFromOrder(
      ["A-1", "root-1"],
      { "A-1": "alpha", "root-1": ROOTS_RAIL_KEY },
      (id) => [rootWs, storyWs].find((w) => w.id === id),
    );
    expect(rails.map((r) => r.railKey)).toEqual([ROOTS_RAIL_KEY, "alpha"]);
  });
});

describe("railInfo", () => {
  it("distinguishes epic, roots and ungrouped identities through the shared seam", () => {
    expect(railInfo("org:team", "pre-beta-fixes")).toEqual({
      title: "Pre Beta Fixes",
      kind: "epic",
    });
    expect(railInfo("org:team", ROOTS_RAIL_KEY).kind).toBe("roots");
    expect(railInfo("org:team", "")).toEqual({ title: "Ungrouped", kind: "ungrouped" });
  });
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";
import { markContentPathChanged, useFileRefreshStore } from "./file-refresh-store";
import { seedFileRefresh } from "./__test-utils__/seed-file-refresh";

describe("useFileRefreshStore", () => {
  beforeEach(() => {
    seedFileRefresh();
  });

  describe("bumpFileRefresh — per-repo (SS-9 verbatim move from sync-store)", () => {
    it("increments fileRefreshKeys[repoPath] for a specific repo", () => {
      seedFileRefresh({ "/repo/a": 1 });
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a");
      expect(useFileRefreshStore.getState().fileRefreshKeys["/repo/a"]).toBe(2);
    });

    it("stores changedPaths Set on the per-repo entry", () => {
      const paths = new Set(["/a.md", "/b.md"]);
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a", paths);
      expect(useFileRefreshStore.getState().lastChangedContextFile["/repo/a"]).toBe(paths);
    });

    it("stores null on the per-repo entry when changedPaths omitted (broad refresh)", () => {
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a");
      expect(useFileRefreshStore.getState().lastChangedContextFile["/repo/a"]).toBeNull();
    });

    it("SW-SY-4: counterOnly bumps the scan counter WITHOUT touching lastChangedContextFile", () => {
      const paths = new Set(["/x.md"]);
      // A reverse wave records a precise Set...
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a", paths);
      expect(useFileRefreshStore.getState().fileRefreshKeys["/repo/a"]).toBe(1);

      // ...a forward ContentLayerEvent then counter-only bumps the SAME key.
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a", undefined, true);

      const s = useFileRefreshStore.getState();
      // Counter advanced (pipeline rescan still fires)...
      expect(s.fileRefreshKeys["/repo/a"]).toBe(2);
      // ...but the reverse Set is PRESERVED — not clobbered to the broad null.
      expect(s.lastChangedContextFile["/repo/a"]).toBe(paths);
    });
  });

  describe("bumpFileRefresh — bump-all", () => {
    it("increments every fileRefreshKeys entry and resets lastChangedContextFile to null", () => {
      const aPaths = new Set(["/a.md"]);
      const bPaths = new Set(["/b.md"]);
      seedFileRefresh({ "/repo/a": 1, "/repo/b": 1 }, { "/repo/a": aPaths, "/repo/b": bPaths });
      useFileRefreshStore.getState().bumpFileRefresh();
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys["/repo/a"]).toBe(2);
      expect(s.fileRefreshKeys["/repo/b"]).toBe(2);
      expect(s.lastChangedContextFile["/repo/a"]).toBeNull();
      expect(s.lastChangedContextFile["/repo/b"]).toBeNull();
    });
  });

  // SR-9: `contentRefreshKeys` is the viewer-facing content-version. It advances on
  // content-affecting bumps ONLY — never on a `counterOnly` pipeline-scan bump — so a
  // `ContentLayerEvent` materialize storm can no longer drive a viewer `readFile`.
  describe("bumpFileRefresh — contentRefreshKeys (SR-9 viewer dedupe)", () => {
    it("advances contentRefreshKeys alongside fileRefreshKeys on a normal per-repo bump", () => {
      seedFileRefresh({ "/repo/a": 1 }, undefined, { "/repo/a": 1 });
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a");
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys["/repo/a"]).toBe(2);
      expect(s.contentRefreshKeys["/repo/a"]).toBe(2);
    });

    it("advances contentRefreshKeys on a broad (undefined changedPaths) content bump", () => {
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a");
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys["/repo/a"]).toBe(1);
      expect(s.contentRefreshKeys["/repo/a"]).toBe(1);
    });

    it("advances contentRefreshKeys on a precise-Set (reverse wave) content bump", () => {
      const paths = new Set(["/a.md"]);
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a", paths);
      const s = useFileRefreshStore.getState();
      expect(s.contentRefreshKeys["/repo/a"]).toBe(1);
      expect(s.lastChangedContextFile["/repo/a"]).toBe(paths);
    });

    it("counterOnly bump advances fileRefreshKeys but NOT contentRefreshKeys (the fix)", () => {
      seedFileRefresh({ "/repo/a": 1 }, undefined, { "/repo/a": 1 });
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a", undefined, true);
      const s = useFileRefreshStore.getState();
      // Pipeline scan still fires...
      expect(s.fileRefreshKeys["/repo/a"]).toBe(2);
      // ...but the viewer content-version is untouched — no viewer re-read.
      expect(s.contentRefreshKeys["/repo/a"]).toBe(1);
    });

    it("bump-all advances contentRefreshKeys for every entry", () => {
      seedFileRefresh({ "/repo/a": 1, "/repo/b": 1 }, undefined, { "/repo/a": 1, "/repo/b": 1 });
      useFileRefreshStore.getState().bumpFileRefresh();
      const s = useFileRefreshStore.getState();
      expect(s.contentRefreshKeys["/repo/a"]).toBe(2);
      expect(s.contentRefreshKeys["/repo/b"]).toBe(2);
    });

    it("empty-string guard leaves contentRefreshKeys untouched", () => {
      seedFileRefresh({ "/repo/a": 1 }, undefined, { "/repo/a": 3 });
      useFileRefreshStore.getState().bumpFileRefresh("");
      expect(useFileRefreshStore.getState().contentRefreshKeys["/repo/a"]).toBe(3);
    });
  });

  describe("seedFileRefresh helper", () => {
    it("no-args call resets BOTH slices to empty", () => {
      useFileRefreshStore.setState({
        fileRefreshKeys: { "/a": 1 },
        lastChangedContextFile: { "/a": new Set(["/a.md"]) },
      });
      seedFileRefresh();
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys).toEqual({});
      expect(s.lastChangedContextFile).toEqual({});
    });

    it("partial seed: only fileRefreshKeys passed → lastChangedContextFile preserved", () => {
      const aPaths = new Set(["/a.md"]);
      useFileRefreshStore.setState({
        fileRefreshKeys: { "/a": 5 },
        lastChangedContextFile: { "/a": aPaths },
      });
      seedFileRefresh({ "/r1": 1 });
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys).toEqual({ "/r1": 1 });
      // lastChangedContextFile must NOT have been clobbered by an empty default.
      expect(s.lastChangedContextFile).toEqual({ "/a": aPaths });
    });

    it("partial seed: only lastChangedContextFile passed → fileRefreshKeys preserved", () => {
      useFileRefreshStore.setState({
        fileRefreshKeys: { "/a": 5 },
        lastChangedContextFile: {},
      });
      seedFileRefresh(undefined, { "/r1": new Set(["/a.md"]) });
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys).toEqual({ "/a": 5 });
      expect(s.lastChangedContextFile["/r1"]?.has("/a.md")).toBe(true);
    });

    it("explicit single-undefined arg does NOT trigger a full reset (iter-2 contract)", () => {
      // Regression guard: pre-iter-2 the implementation checked
      // `fileRefreshKeys === undefined && lastChangedContextFile === undefined`
      // which collapsed `seedFileRefresh(undefined)` into a no-args full reset.
      // The arguments.length check enforces the docstring contract.
      useFileRefreshStore.setState({
        fileRefreshKeys: { "/a": 5 },
        lastChangedContextFile: { "/a": new Set(["/a.md"]) },
      });
      seedFileRefresh(undefined);
      const s = useFileRefreshStore.getState();
      expect(s.fileRefreshKeys).toEqual({ "/a": 5 });
      expect(s.lastChangedContextFile["/a"]?.has("/a.md")).toBe(true);
    });
  });

  describe("pruneRepos (SW-FX-64 AC-8)", () => {
    it("deletes evicted repo keys from ALL slices, leaving others intact", () => {
      const aPaths = new Set(["/a.md"]);
      const bPaths = new Set(["/b.md"]);
      seedFileRefresh(
        { "workspace:acme:default:o/a": 3, "workspace:acme:default:o/b": 2 },
        { "workspace:acme:default:o/a": aPaths, "workspace:acme:default:o/b": bPaths },
        { "workspace:acme:default:o/a": 3, "workspace:acme:default:o/b": 2 },
      );
      useFileRefreshStore.getState().pruneRepos(["workspace:acme:default:o/a"]);
      const s = useFileRefreshStore.getState();
      // Evicted key gone from all records (SR-9: incl. contentRefreshKeys, else it
      // accumulates dead slots)...
      expect(s.fileRefreshKeys["workspace:acme:default:o/a"]).toBeUndefined();
      expect(s.lastChangedContextFile["workspace:acme:default:o/a"]).toBeUndefined();
      expect(s.contentRefreshKeys["workspace:acme:default:o/a"]).toBeUndefined();
      // ...the surviving team's key untouched.
      expect(s.fileRefreshKeys["workspace:acme:default:o/b"]).toBe(2);
      expect(s.lastChangedContextFile["workspace:acme:default:o/b"]).toBe(bPaths);
      expect(s.contentRefreshKeys["workspace:acme:default:o/b"]).toBe(2);
    });

    it("prunes multiple keys at once", () => {
      seedFileRefresh({ "/r/a": 1, "/r/b": 1, "/r/c": 1 });
      useFileRefreshStore.getState().pruneRepos(["/r/a", "/r/c"]);
      expect(useFileRefreshStore.getState().fileRefreshKeys).toEqual({ "/r/b": 1 });
    });

    it("empty list is a no-op and preserves slice identity (no re-render churn)", () => {
      seedFileRefresh({ "/r/a": 1 });
      const before = useFileRefreshStore.getState();
      before.pruneRepos([]);
      const after = useFileRefreshStore.getState();
      expect(after.fileRefreshKeys).toBe(before.fileRefreshKeys);
      expect(after.lastChangedContextFile).toBe(before.lastChangedContextFile);
    });

    it("pruning an unknown key mutates nothing and preserves slice identity", () => {
      seedFileRefresh({ "/r/a": 1 });
      const before = useFileRefreshStore.getState();
      before.pruneRepos(["/does/not/exist"]);
      const after = useFileRefreshStore.getState();
      expect(after.fileRefreshKeys).toBe(before.fileRefreshKeys);
      expect(after.fileRefreshKeys["/r/a"]).toBe(1);
    });
  });

  describe("bumpFileRefresh — empty-string defensive guard", () => {
    it("returns silently on empty string repoPath (no destructive bump-all)", () => {
      const aPaths = new Set(["/a.md"]);
      const bPaths = new Set(["/b.md"]);
      seedFileRefresh({ "/repo/a": 1, "/repo/b": 1 }, { "/repo/a": aPaths, "/repo/b": bPaths });
      useFileRefreshStore.getState().bumpFileRefresh("");
      const s = useFileRefreshStore.getState();
      // No bumps, no resets — empty-string is a caller bug, defensively no-op.
      expect(s.fileRefreshKeys["/repo/a"]).toBe(1);
      expect(s.fileRefreshKeys["/repo/b"]).toBe(1);
      expect(s.fileRefreshKeys[""]).toBeUndefined();
      expect(s.lastChangedContextFile["/repo/a"]).toBe(aPaths);
      expect(s.lastChangedContextFile["/repo/b"]).toBe(bPaths);
    });
  });

  describe("markContentPathChanged — SW-FX-139 save-exit re-read seam", () => {
    it("routes a retained private file to its content root rather than the clone key", () => {
      const root = "/home/users/42/scratch/orgs/acme/plans/content";
      markContentPathChanged("/clone", `${root}/2026/plan.md`);
      const state = useFileRefreshStore.getState();
      expect(state.fileRefreshKeys[root]).toBe(1);
      expect(state.contentRefreshKeys[root]).toBe(1);
      expect(state.fileRefreshKeys["/clone"]).toBeUndefined();
      expect(state.lastChangedContextFile[root]).toEqual(new Set([`${root}/2026/plan.md`]));
    });
    it("advances the content key and marks only that path changed", () => {
      markContentPathChanged("/repo/a", "docs/a.md");
      const s = useFileRefreshStore.getState();
      expect(s.contentRefreshKeys["/repo/a"]).toBe(1);
      expect([...(s.lastChangedContextFile["/repo/a"] ?? [])]).toEqual(["docs/a.md"]);
    });

    it("each save is its own burst: a previous burst does not carry over", () => {
      markContentPathChanged("/repo/a", "docs/a.md");
      markContentPathChanged("/repo/a", "docs/b.md");
      expect([...(useFileRefreshStore.getState().lastChangedContextFile["/repo/a"] ?? [])]).toEqual(
        ["docs/b.md"],
      );
    });

    it("a stale broad (null) burst is replaced, not kept broad", () => {
      useFileRefreshStore.getState().bumpFileRefresh("/repo/a");
      markContentPathChanged("/repo/a", "docs/a.md");
      expect([...(useFileRefreshStore.getState().lastChangedContextFile["/repo/a"] ?? [])]).toEqual(
        ["docs/a.md"],
      );
    });

    it("an empty repo path is a no-op", () => {
      markContentPathChanged("", "docs/a.md");
      expect(useFileRefreshStore.getState().contentRefreshKeys).toEqual({});
    });
  });
});

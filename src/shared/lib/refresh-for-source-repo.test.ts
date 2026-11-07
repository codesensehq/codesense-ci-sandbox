// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import { logger } from "@/shared/lib/logger";
import { syntheticKeyForContextRepoPath } from "@/shared/lib/build-synthetic-repo";
import { bumpFileRefreshForSourceRepo } from "./refresh-for-source-repo";

vi.mock("@/shared/lib/logger", () => ({
  logger: { warn: vi.fn(), info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));

function makeRepo(overrides: Partial<RepoState>): RepoState {
  return {
    repoPath: "synth:codesense:default:alderco/codesense",
    repoName: "codesense (codesense)",
    contextRepoPath: "/org/_team/default/repos/alderco/codesense",
    teamContentPath: "/org/_team/default/content",
    hasRemote: true,
    remoteUrl: "git@github.com:alderco/codesense.git",
    mainDoc: "",
    localPath: "/Users/me/code/codesense",
    ...overrides,
  };
}

describe("bumpFileRefreshForSourceRepo", () => {
  beforeEach(() => {
    seedFileRefresh();
    useOrgStore.getState().clearWorkspaceRepos();
    vi.mocked(logger.warn).mockClear();
  });

  it("bumps the source path itself (single-repo / non-workspace mode)", () => {
    bumpFileRefreshForSourceRepo("/Users/me/code/codesense");
    expect(useFileRefreshStore.getState().fileRefreshKeys["/Users/me/code/codesense"]).toBe(1);
  });

  it("fans out to every workspace synthetic repo whose localPath matches", () => {
    useOrgStore.getState().setWorkspaceRepo(
      makeRepo({
        repoPath: "synth:codesense:default:alderco/codesense",
        localPath: "/Users/me/code/codesense",
      }),
    );
    useOrgStore.getState().setWorkspaceRepo(
      makeRepo({
        repoPath: "synth:codesense:smart-web:alderco/codesense",
        localPath: "/Users/me/code/codesense",
      }),
    );

    bumpFileRefreshForSourceRepo("/Users/me/code/codesense", new Set(["/path/to/file.md"]));

    const keys = useFileRefreshStore.getState().fileRefreshKeys;
    expect(keys["/Users/me/code/codesense"]).toBe(1);
    expect(keys["synth:codesense:default:alderco/codesense"]).toBe(1);
    expect(keys["synth:codesense:smart-web:alderco/codesense"]).toBe(1);
  });

  it("does NOT bump synthetic repos whose localPath is null (unattached)", () => {
    useOrgStore.getState().setWorkspaceRepo(
      makeRepo({
        repoPath: "synth:codesense:default:alderco/codesense",
        localPath: null,
      }),
    );

    bumpFileRefreshForSourceRepo("/Users/me/code/codesense");

    const keys = useFileRefreshStore.getState().fileRefreshKeys;
    expect(keys["/Users/me/code/codesense"]).toBe(1);
    expect(keys["synth:codesense:default:alderco/codesense"]).toBeUndefined();
  });

  it("does NOT bump synthetic repos backed by a different source path", () => {
    useOrgStore.getState().setWorkspaceRepo(
      makeRepo({
        repoPath: "synth:codesense:default:other/repo",
        localPath: "/Users/me/code/other-repo",
      }),
    );

    bumpFileRefreshForSourceRepo("/Users/me/code/codesense");

    const keys = useFileRefreshStore.getState().fileRefreshKeys;
    expect(keys["/Users/me/code/codesense"]).toBe(1);
    expect(keys["synth:codesense:default:other/repo"]).toBeUndefined();
  });

  it("avoids double-bumping when synthetic key coincides with source path", () => {
    // Defensive: synthetic keys should never collide with source paths
    // (they start with `synth:`), but the guard prevents a counter bump of
    // 2 if they ever did.
    useOrgStore.getState().setWorkspaceRepo(
      makeRepo({
        repoPath: "/Users/me/code/codesense", // hypothetical collision
        localPath: "/Users/me/code/codesense",
      }),
    );

    bumpFileRefreshForSourceRepo("/Users/me/code/codesense");

    expect(useFileRefreshStore.getState().fileRefreshKeys["/Users/me/code/codesense"]).toBe(1);
  });

  it("forwards the changedPaths Set to each bump (preserves F-26 Set-membership optimization)", () => {
    useOrgStore.getState().setWorkspaceRepo(
      makeRepo({
        repoPath: "synth:codesense:default:alderco/codesense",
        localPath: "/Users/me/code/codesense",
      }),
    );

    const paths = new Set(["/the/changed/file.md"]);
    bumpFileRefreshForSourceRepo("/Users/me/code/codesense", paths);

    const lastChanged = useFileRefreshStore.getState().lastChangedContextFile;
    expect(lastChanged["/Users/me/code/codesense"]).toBe(paths);
    expect(lastChanged["synth:codesense:default:alderco/codesense"]).toBe(paths);
  });

  // PTS-5 bug 1: when a synthetic workspace key is the bump input, fan out to
  // every sibling synthetic in the same (orgSlug, team) — they share the
  // per-team flat content dir under PTS-2, but the watcher emits only one
  // key per FS event.
  describe("synthetic-key fan-out to siblings in same (org, team)", () => {
    it("bumps every sibling synthetic with matching org+team", () => {
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:mycorp/mycorp_app",
          localPath: "/Users/me/code/mycorp_app",
        }),
      );
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );

      bumpFileRefreshForSourceRepo(
        "workspace:9north:smart-web:AllWidgets/AllWidgets",
        new Set(["/team/content/_smoke-epic/SD-SMOKE-1.md"]),
      );

      const keys = useFileRefreshStore.getState().fileRefreshKeys;
      expect(keys["workspace:9north:smart-web:AllWidgets/AllWidgets"]).toBe(1);
      expect(keys["workspace:9north:smart-web:mycorp/mycorp_app"]).toBe(1);
    });

    it("does NOT bump siblings in a different team", () => {
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:default:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );

      bumpFileRefreshForSourceRepo("workspace:9north:smart-web:AllWidgets/AllWidgets");

      const keys = useFileRefreshStore.getState().fileRefreshKeys;
      expect(keys["workspace:9north:smart-web:AllWidgets/AllWidgets"]).toBe(1);
      // Different team — must not bump.
      expect(keys["workspace:9north:default:AllWidgets/AllWidgets"]).toBeUndefined();
    });

    it("does NOT bump siblings in a different org", () => {
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:codesense:smart-web:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );

      bumpFileRefreshForSourceRepo("workspace:9north:smart-web:AllWidgets/AllWidgets");

      const keys = useFileRefreshStore.getState().fileRefreshKeys;
      expect(keys["workspace:9north:smart-web:AllWidgets/AllWidgets"]).toBe(1);
      expect(keys["workspace:codesense:smart-web:AllWidgets/AllWidgets"]).toBeUndefined();
    });

    it("forwards changedFilePath to every sibling bump", () => {
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:mycorp/mycorp_app",
          localPath: "/Users/me/code/mycorp_app",
        }),
      );
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );

      const paths = new Set(["/team/content/changed.md"]);
      bumpFileRefreshForSourceRepo("workspace:9north:smart-web:AllWidgets/AllWidgets", paths);

      const lastChanged = useFileRefreshStore.getState().lastChangedContextFile;
      expect(lastChanged["workspace:9north:smart-web:AllWidgets/AllWidgets"]).toBe(paths);
      expect(lastChanged["workspace:9north:smart-web:mycorp/mycorp_app"]).toBe(paths);
    });

    it("non-synthetic input (source disk path) skips the synthetic fan-out branch", () => {
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:AllWidgets/AllWidgets",
          localPath: "/different/path",
        }),
      );

      bumpFileRefreshForSourceRepo("/Users/me/code/codesense");

      const keys = useFileRefreshStore.getState().fileRefreshKeys;
      expect(keys["/Users/me/code/codesense"]).toBe(1);
      // Synthetic with non-matching localPath stays untouched.
      expect(keys["workspace:9north:smart-web:AllWidgets/AllWidgets"]).toBeUndefined();
    });
  });

  // Code-review patches:
  describe("defensive guards", () => {
    it("returns immediately on empty repoIdentity (prevents bump-all path)", () => {
      // Seed a workspaceRepo + sync state so the bump-all path would visibly
      // affect them if triggered. Without the workspaceRepo entry, the
      // legacy fan-out loop would iterate zero times and a passing test
      // wouldn't prove the bump-all hazard is actually neutralized.
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:codesense:default:alderco/codesense",
          localPath: "/Users/me/code/codesense",
        }),
      );
      seedFileRefresh(
        { "workspace:codesense:default:alderco/codesense": 5 },
        {
          "workspace:codesense:default:alderco/codesense": new Set(["/old/path.md"]),
        },
      );

      bumpFileRefreshForSourceRepo("");

      const sync = useFileRefreshStore.getState();
      // Counter unchanged — bump-all would have incremented to 6.
      expect(sync.fileRefreshKeys["workspace:codesense:default:alderco/codesense"]).toBe(5);
      // lastChangedContextFile preserved — bump-all would have reset to null.
      const stored = sync.lastChangedContextFile["workspace:codesense:default:alderco/codesense"];
      expect(stored instanceof Set).toBe(true);
      expect(stored?.has("/old/path.md")).toBe(true);
      // No entry created for empty string.
      expect(sync.fileRefreshKeys[""]).toBeUndefined();
    });

    it("logs a warning AND skips bump when synthetic-prefixed key fails strict parse", () => {
      // `workspace:` prefix passes isSyntheticRepoPath but `a:b:c:d`'s
      // split-by-colon length (4) fails parseSyntheticRepoKey's
      // `parts.length !== 3` check.
      bumpFileRefreshForSourceRepo("workspace:a:b:c:d");

      expect(logger.warn).toHaveBeenCalledWith(
        "refresh-for-source-repo",
        expect.stringContaining("malformed synthetic key"),
      );
      // Malformed keys must NOT pollute fileRefreshKeys — no subscriber
      // reads keys that aren't valid synthetics, so a bump there is junk.
      expect(useFileRefreshStore.getState().fileRefreshKeys["workspace:a:b:c:d"]).toBeUndefined();
    });

    it("does NOT warn for well-formed synthetic keys", () => {
      useOrgStore.getState().setWorkspaceRepo(
        makeRepo({
          repoPath: "workspace:9north:smart-web:AllWidgets/AllWidgets",
          localPath: "/Users/me/code/AllWidgets",
        }),
      );

      bumpFileRefreshForSourceRepo("workspace:9north:smart-web:AllWidgets/AllWidgets");

      expect(logger.warn).not.toHaveBeenCalled();
    });
  });

  // SW-FX-16 (AC-6): a context-file-change from a WORKTREE surface emits
  // `repo_path` = `ws.cwd` (the worktree path), which matches no `localPath`
  // (= the root-clone attachment path), so the legacy disk-path fan-out lands
  // on a dead key. The App listener now resolves the surface-independent
  // `context_repo_path` → synthetic and bumps THAT. These tests mirror that
  // resolve-then-bump wiring and pin the regression.
  describe("context-file-change routes by context_repo_path (worktree-surface fix)", () => {
    const CTX = "/org/_team/default/repos/alderco/codesense";
    const SYNTH = "workspace:codesense:default:alderco/codesense";
    const ROOT_CLONE = "/Users/me/code/codesense";
    const WORKTREE_CWD = "/Users/me/wt/feat-x";

    beforeEach(() => {
      useOrgStore
        .getState()
        .setWorkspaceRepo(
          makeRepo({ repoPath: SYNTH, contextRepoPath: CTX, localPath: ROOT_CLONE }),
        );
    });

    it("bumps the synthetic key the tree subscribes on for a worktree-surface edit", () => {
      // App listener: `syntheticKeyForContextRepoPath(context_repo_path) ?? repo_path`.
      const bumpKey = syntheticKeyForContextRepoPath(CTX) ?? WORKTREE_CWD;
      bumpFileRefreshForSourceRepo(bumpKey, new Set([`${CTX}/content/story.md`]));

      const keys = useFileRefreshStore.getState().fileRefreshKeys;
      expect(keys[SYNTH]).toBe(1);
      // The dead worktree-cwd key is never touched on this path.
      expect(keys[WORKTREE_CWD]).toBeUndefined();
    });

    it("REGRESSION: bumping the raw worktree cwd (old behavior) misses the synthetic", () => {
      bumpFileRefreshForSourceRepo(WORKTREE_CWD);

      const keys = useFileRefreshStore.getState().fileRefreshKeys;
      // ws.cwd ≠ localPath (root clone) → fan-out finds nothing → dead-key bump.
      expect(keys[SYNTH]).toBeUndefined();
      expect(keys[WORKTREE_CWD]).toBe(1);
    });

    it("falls back to repo_path when no workspace repo owns the context dir", () => {
      // `_codesense` internal handler: context_repo_path resolves to null →
      // App keeps the emitted repo_path, preserving current behavior.
      const bumpKey = syntheticKeyForContextRepoPath("/org/_codesense") ?? ROOT_CLONE;
      expect(bumpKey).toBe(ROOT_CLONE);

      bumpFileRefreshForSourceRepo(bumpKey);
      // Root clone IS a localPath → its synthetic still bumps (root-surface path).
      expect(useFileRefreshStore.getState().fileRefreshKeys[SYNTH]).toBe(1);
    });
  });
});

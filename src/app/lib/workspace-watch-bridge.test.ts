// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi, type Mock } from "vitest";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { resetWorkspaceWatchHooks } from "@/shared/lib/workspace-watch-hooks";
import {
  buildWorkspaceWatchConfig,
  registerWorkspaceWatchBridge,
  reattachWorkspaceWatchesWithNewContext,
  __resetWorkspaceWatchBridge,
} from "./workspace-watch-bridge";
import { lazyAttachWorkspace, lazyDetachWorkspace } from "@/tauri/commands";

// Spy on the commands; keep the real `toWatchConfig` (pure) so the built
// config shape is exercised end-to-end.
vi.mock("@/tauri/commands", async (orig) => {
  const actual = await orig<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    lazyAttachWorkspace: vi.fn().mockResolvedValue(undefined),
    lazyDetachWorkspace: vi.fn().mockResolvedValue(undefined),
  };
});

const attachSpy = lazyAttachWorkspace as unknown as Mock;
const detachSpy = lazyDetachWorkspace as unknown as Mock;

const TEAM = "test-org:default";
const SLUG = "test-org/test-repo";
const SYNTH = `workspace:test-org:default:${SLUG}`;
// Realistic context-repo path so deriveOrgRoot strips {team}/content.
const CTX = "/home/u/.codesense/users/42/orgs/test-org/default/content";
const ORG_ROOT = "/home/u/.codesense/users/42/orgs/test-org";

function seedRepo() {
  useOrgStore.setState({
    workspaceRepos: {
      [SYNTH]: {
        repoPath: SYNTH,
        contextRepoPath: CTX,
      },
    },
  } as never);
}

function seedWorkspaces(ws: Workspace[]) {
  useViewerStore.setState({ workspaces: { [TEAM]: ws }, workspaceOpenSet: {} });
}

const story = (id: string, cwd: string): Workspace => ({
  id,
  kind: "story",
  cwd,
  status: "ready",
  sourceRepoSlug: SLUG,
  storyFilePath: `${cwd}/story.md`,
});

beforeEach(() => {
  attachSpy.mockClear();
  detachSpy.mockClear();
  __resetWorkspaceWatchBridge();
  resetWorkspaceWatchHooks();
  seedRepo();
});

describe("buildWorkspaceWatchConfig (SW-WL-3 T2)", () => {
  it("follows the workspace cwd; pulls context/viewer from the source repo", () => {
    seedWorkspaces([story("ws-1", "/worktrees/ws-1")]);
    const cfg = buildWorkspaceWatchConfig(TEAM, "ws-1");
    expect(cfg).toEqual({
      repo_path: "/worktrees/ws-1",
      context_repo_path: CTX,
    });
  });

  it("returns null when the workspace, its slug, its cwd, or its repo can't resolve", () => {
    seedWorkspaces([story("ws-1", "/worktrees/ws-1")]);
    expect(buildWorkspaceWatchConfig(TEAM, "missing")).toBeNull();

    seedWorkspaces([{ ...story("ws-2", ""), cwd: "" }]); // root still creating
    expect(buildWorkspaceWatchConfig(TEAM, "ws-2")).toBeNull();

    seedWorkspaces([{ ...story("ws-3", "/wt/ws-3"), sourceRepoSlug: undefined }]);
    expect(buildWorkspaceWatchConfig(TEAM, "ws-3")).toBeNull();

    useOrgStore.setState({ workspaceRepos: {} } as never);
    seedWorkspaces([story("ws-4", "/wt/ws-4")]);
    expect(buildWorkspaceWatchConfig(TEAM, "ws-4")).toBeNull();
  });
});

describe("registerWorkspaceWatchBridge (SW-WL-3 T2 / AC-2)", () => {
  it("invokes lazy_attach_workspace on open with the slug + built config (axes a+c)", () => {
    seedWorkspaces([story("ws-1", "/worktrees/ws-1")]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "ws-1");
    expect(attachSpy).toHaveBeenCalledTimes(1);
    expect(attachSpy).toHaveBeenCalledWith(
      "ws-1",
      SLUG,
      expect.objectContaining({ repo_path: "/worktrees/ws-1", context_repo_path: CTX }),
    );
  });

  it("detaches with the config captured at attach time even after the workspace is removed", () => {
    seedWorkspaces([story("ws-1", "/worktrees/ws-1")]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "ws-1");

    // Simulate closeWorkspace removing the workspace BEFORE the watch detaches.
    useViewerStore.setState({ workspaces: { [TEAM]: [] } });
    useViewerStore.getState().closeWorkspaceWatch(TEAM, "ws-1");

    expect(detachSpy).toHaveBeenCalledTimes(1);
    expect(detachSpy).toHaveBeenCalledWith(
      "ws-1",
      expect.objectContaining({ repo_path: "/worktrees/ws-1" }),
    );
  });

  it("does not attach when the config can't be built (no source repo)", () => {
    useOrgStore.setState({ workspaceRepos: {} } as never);
    seedWorkspaces([story("ws-1", "/worktrees/ws-1")]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "ws-1");
    expect(attachSpy).not.toHaveBeenCalled();
  });
});

describe("epic integration reverse-watch attach (SW-EW-8 Slice A)", () => {
  const ORG = "test-org";
  const EPIC_DIR = `${ORG_ROOT}/_epic/SW-EW`;
  const integration = (id: string, epicDir: string, slug?: string): Workspace => ({
    id,
    kind: "epic",
    cwd: epicDir,
    status: "ready",
    epicDirPath: "SW-EW",
    epicWorkspaceDir: epicDir,
    sourceRepoSlug: slug,
  });

  function seedOrg() {
    useOrgStore.setState({
      orgs: [{ slug: ORG, org_root: ORG_ROOT } as never],
    } as never);
  }

  it("attaches the epic dir's reverse watch with an empty slug (skip reconcile)", () => {
    seedOrg();
    seedWorkspaces([integration("integration:SW-EW", EPIC_DIR)]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "integration:SW-EW");

    expect(attachSpy).toHaveBeenCalledTimes(1);
    expect(attachSpy).toHaveBeenCalledWith(
      "integration:SW-EW",
      "", // empty slug → Rust skips reconcile-on-attach
      expect.objectContaining({
        repo_path: EPIC_DIR,
        context_repo_path: `${ORG_ROOT}/default/content`,
      }),
    );
  });

  it("detaches the epic watch on close (AC-5)", () => {
    seedOrg();
    seedWorkspaces([integration("integration:SW-EW", EPIC_DIR)]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "integration:SW-EW");

    useViewerStore.setState({ workspaces: { [TEAM]: [] } });
    useViewerStore.getState().closeWorkspaceWatch(TEAM, "integration:SW-EW");

    expect(detachSpy).toHaveBeenCalledTimes(1);
    expect(detachSpy).toHaveBeenCalledWith(
      "integration:SW-EW",
      expect.objectContaining({ repo_path: EPIC_DIR }),
    );
  });

  it("does not attach a per-repo integration terminal via the epic path (has a slug)", () => {
    seedOrg();
    // A per-repo integration row has a real slug but no workspaceRepos entry →
    // neither resolver matches → no attach (it is not the epic-level surface).
    seedWorkspaces([integration("integration:SW-EW:x/y", EPIC_DIR, "x/y")]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "integration:SW-EW:x/y");
    expect(attachSpy).not.toHaveBeenCalled();
  });
});

describe("SW-LN-B29 · a local (no-remote) repo's open worktrees attach their watch", () => {
  const LOCAL_TEAM = "local/e2e:general";
  // `syntheticRepoKey` host-qualifies every non-GitHub repo; a no-remote repo is
  // `{ owner: "local", repo, provider: "local", host: "local" }`.
  const LOCAL_SYNTH = "workspace:local/e2e:general:local/local/fixture-api";
  const LOCAL_CTX = "/home/u/.codesense/users/1/orgs/local/e2e/general/content";

  beforeEach(() => {
    useOrgStore.setState({
      workspaceRepos: {
        [LOCAL_SYNTH]: {
          repoPath: LOCAL_SYNTH,
          contextRepoPath: LOCAL_CTX,
        },
      },
    } as never);
    // The workspace names its repo by the host-less `owner/repo` (story frontmatter /
    // `deriveSourceRepoSlug`), which is not the host-qualified synthetic key.
    useViewerStore.setState({
      workspaces: {
        [LOCAL_TEAM]: [
          { ...story("wt-a", "/code/fixture-api-wt/TR-1"), sourceRepoSlug: "local/fixture-api" },
          { ...story("wt-b", "/code/fixture-api-wt/GSLFA-1"), sourceRepoSlug: "local/fixture-api" },
        ],
      },
      workspaceOpenSet: {},
    });
  });

  it("both open worktrees register as watch subscribers (lazy_attach_workspace)", () => {
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(LOCAL_TEAM, "wt-a");
    useViewerStore.getState().openWorkspaceWatch(LOCAL_TEAM, "wt-b");
    expect(attachSpy).toHaveBeenCalledTimes(2);
    expect(attachSpy).toHaveBeenCalledWith(
      "wt-a",
      "local/fixture-api",
      expect.objectContaining({
        repo_path: "/code/fixture-api-wt/TR-1",
        context_repo_path: LOCAL_CTX,
      }),
    );
    expect(buildWorkspaceWatchConfig(LOCAL_TEAM, "wt-b")).toEqual(
      expect.objectContaining({ repo_path: "/code/fixture-api-wt/GSLFA-1" }),
    );
  });
});

describe("reattachWorkspaceWatchesWithNewContext (SW-LN-B51)", () => {
  it("re-attaches a workspace attached with an empty context path, detaching the old one first", async () => {
    // The repo was added mid-session: its workspace repo had no context path yet.
    useOrgStore.setState({
      workspaceRepos: {
        [SYNTH]: { repoPath: SYNTH, contextRepoPath: "" },
      },
    } as never);
    seedWorkspaces([story("ws-1", "/worktrees/ws-1")]);
    registerWorkspaceWatchBridge();
    useViewerStore.getState().openWorkspaceWatch(TEAM, "ws-1");
    expect(attachSpy).toHaveBeenLastCalledWith(
      "ws-1",
      SLUG,
      expect.objectContaining({ context_repo_path: "" }),
    );

    // Rust resolves the path; the workspace repo is patched.
    seedRepo();
    reattachWorkspaceWatchesWithNewContext();
    await vi.waitFor(() => expect(attachSpy).toHaveBeenCalledTimes(2));
    expect(detachSpy).toHaveBeenCalledWith(
      "ws-1",
      expect.objectContaining({ context_repo_path: "" }),
    );
    expect(attachSpy).toHaveBeenLastCalledWith(
      "ws-1",
      SLUG,
      expect.objectContaining({ repo_path: "/worktrees/ws-1", context_repo_path: CTX }),
    );
    expect(detachSpy.mock.invocationCallOrder[0]).toBeLessThan(
      attachSpy.mock.invocationCallOrder[1],
    );

    // Unchanged path: nothing more happens.
    reattachWorkspaceWatchesWithNewContext();
    await Promise.resolve();
    expect(attachSpy).toHaveBeenCalledTimes(2);
    expect(detachSpy).toHaveBeenCalledTimes(1);
  });
});

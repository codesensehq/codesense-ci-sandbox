// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, cleanup } from "@/test-utils";
import { applyFieldPatch } from "@/shared/lib/field-patch";

// Seam: the real story open (`openOrFocusStoryWorkspace` → `ensureWorktree`),
// the real Where dialog and machines driver, the real viewer persistence and
// the real remote-tab open path — against a fake machine at the Tauri boundary
// that prepares a workspace once per request (sending its bundle then),
// reconciles every repeat without one, and holds a request to the identity it
// was first made with (a repeat with another base or branch is refused). Like
// the backend command, it replays a prior intent for the same worktree when
// `replay` is on.

const fake = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  prepared: new Map<
    string,
    { handle: string; cwd: string; baseRef: string; branch: string; worktree: string }
  >(),
  replay: true,
  prepareCalls: [] as Array<Record<string, string>>,
  bundlesSent: 0,
  /** The viewer state the backend held when each prepare arrived. */
  pushedAtPrepare: [] as unknown[],
  lastPushed: null as unknown,
  labelsDuringPrepare: [] as unknown[],
  remoteTabs: [] as Array<Record<string, unknown>>,
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    success: vi.fn(),
    error: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
    loading: vi.fn(),
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    fake.listeners.set(name, handler);
    return () => fake.listeners.delete(name);
  }),
  emit: vi.fn(),
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn(async (viewer: unknown) => {
    fake.lastPushed = JSON.parse(JSON.stringify(viewer));
  }),
  readFile: vi.fn().mockResolvedValue("# Remote story\n"),
  probeWorktreeConfig: vi.fn().mockResolvedValue({ status: { kind: "missing" }, config: null }),
  checkBranchExists: vi.fn().mockResolvedValue(true),
  storyDefaultStartPoint: vi.fn(),
  establishWorktreeStoryLink: vi.fn().mockResolvedValue(true),
  dashboardRegisterSession: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(false),
  listMachines: vi.fn().mockResolvedValue([]),
  prepareRemoteStoryWorkspace: vi.fn(async (request: Record<string, string>) => {
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    fake.prepareCalls.push(request);
    fake.pushedAtPrepare.push(fake.lastPushed);
    const worktree = `${request.machineId}|${request.teamKey}|${request.workspaceId}`;
    let identity = {
      requestId: request.requestId,
      baseRef: request.baseRef,
      branch: request.branch,
    };
    if (fake.replay) {
      for (const [requestId, prior] of fake.prepared)
        if (prior.worktree === worktree)
          identity = { requestId, baseRef: prior.baseRef, branch: prior.branch };
    }
    const answer = (result: Record<string, unknown>) => ({ ...identity, result });
    const known = fake.prepared.get(identity.requestId);
    if (known) {
      if (known.baseRef !== identity.baseRef || known.branch !== identity.branch)
        return answer({
          status: "refused",
          message: "request identity belongs to another intent or server",
        });
      return answer({ status: "alreadyPrepared", handle: known.handle, cwd: known.cwd });
    }
    // The machine's first sight of this request: the bundle crosses, with progress.
    const emit = fake.listeners.get("remote-workspace-progress");
    emit?.({ payload: { requestId: request.requestId, phase: "bundling" } });
    emit?.({ payload: { requestId: request.requestId, phase: "copying", sent: 50, total: 100 } });
    fake.labelsDuringPrepare.push(
      useViewerStore
        .getState()
        .workspaces[request.teamKey]?.find((w) => w.id === request.workspaceId)?.setupState,
    );
    fake.bundlesSent += 1;
    const done = {
      handle: `remote-workspace-${identity.requestId}`,
      cwd: `/home/dev/codesense/worktrees/default/${request.storyId}`,
    };
    fake.prepared.set(identity.requestId, {
      ...done,
      baseRef: identity.baseRef,
      branch: identity.branch,
      worktree,
    });
    return answer({ status: "prepared", ...done });
  }),
}));

vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-launch-source")
    >()),
    remoteShellTab: vi.fn(async (request: Record<string, unknown>) => {
      fake.remoteTabs.push(request);
      return { status: "unknown", headed: false, message: "fake machine" };
    }),
  }),
);

vi.mock("@/shared/lib/worktree", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/worktree")>()),
  normalizeStoryFilePath: (p: string) => p,
  parseSourceRepoFromFrontmatter: () => null,
  parseStoryIdFromFrontmatter: () => "SW-9",
  resolveWorktreeParams: vi
    .fn()
    .mockResolvedValue({ branchName: "story/sw-9", startPoint: "main" }),
  resolveWorktreePath: vi.fn().mockReturnValue("/wt/story-sw-9"),
  findWorktreeOnDisk: vi.fn().mockResolvedValue(null),
  createWorktree: vi.fn(),
  setupWorktree: vi.fn(),
  runWorktreeSetupPhase: vi.fn(),
}));

vi.mock("@/shared/lib/build-synthetic-repo", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/build-synthetic-repo")>()),
  workspaceReposForTeam: vi.fn(() => [
    { repoPath: "workspace:test-org:default:test/repo", localPath: "/test/repo" },
  ]),
}));

import { expectAgentLaunch, useViewerStore, selectWorkspaceTabs } from "./viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { WorkspaceWhereDialog } from "@/features/machines/components/workspace-where-dialog";
import "@/features/machines/lib/remote-story-workspace";
import { newTerminalTab } from "@/features/viewer/lib/terminal-tab-keys";
import { clearRemoteOpens } from "@/features/viewer/lib/remote-tab-open";
import { installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import { createWorktree, resolveWorktreeParams } from "@/shared/lib/worktree";
import { probeWorktreeConfig, saveAppState, storyDefaultStartPoint } from "@/tauri/commands";
import type { MachineSummary } from "@/tauri/commands";

const TEAM = "test-org:default";
const STORY = "/ctx/stories/sw-9-remote.md";
const WS = "sw-9-remote";

const BOX = {
  machineId: "box",
  kind: "ssh",
  label: "codesense-test-1",
  scope: { kind: "personal" },
  declaration: { mode: "private-account" },
  binding: null,
  canPublish: true,
  status: { state: "connected", workspaces: null },
  attention: 0,
  capabilities: { agentCallbacks: true },
} as unknown as MachineSummary;

installTerminalLayoutHarness();

beforeEach(() => {
  cleanup();
  fake.prepared.clear();
  fake.replay = true;
  fake.prepareCalls = [];
  fake.bundlesSent = 0;
  fake.pushedAtPrepare = [];
  fake.lastPushed = null;
  fake.labelsDuringPrepare = [];
  fake.remoteTabs = [];
  // The fake machine never answers a tab open, so each stays pending; a prior test's
  // would stop this one's first terminal (an unanswered open is never duplicated).
  clearRemoteOpens();
  localStorage.clear();
  useMachinesStore.setState({ byTeam: { [TEAM]: [BOX] } });
  useViewerStore.setState({
    stateLoaded: true,
    workspaces: {},
    softClosedWorkspaces: {},
    activeWorkspace: {},
  });
});

const record = () => useViewerStore.getState().workspaces[TEAM]?.find((w) => w.id === WS);

async function createOnBox(): Promise<string> {
  render(<WorkspaceWhereDialog />);
  const opened = useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, STORY);
  fireEvent.click(await screen.findByTestId("workspace-where-box"));
  let cwd = "";
  await act(async () => {
    cwd = await opened;
  });
  return cwd;
}

describe("a story workspace created on a machine", () => {
  // #2840: with no configured base the resolver leaves the start point to the repo;
  // a machine is sent the repo's default branch, never an assumed `main`.
  it("#2840: a repo whose default is develop (no local develop) is prepared from origin/develop", async () => {
    vi.mocked(resolveWorktreeParams).mockResolvedValueOnce({
      branchName: "story/sw-9",
      startPoint: null,
    });
    vi.mocked(storyDefaultStartPoint).mockResolvedValueOnce("refs/remotes/origin/develop");
    await createOnBox();
    expect(storyDefaultStartPoint).toHaveBeenCalledWith(expect.any(String), null);
    expect(fake.prepareCalls[0]).toMatchObject({
      baseRef: "refs/remotes/origin/develop",
      branch: "story/sw-9",
    });
    // Its first terminal open settles here, not in the next test.
    await vi.waitFor(() => expect(fake.remoteTabs).toHaveLength(1));
  });

  // A configured `default_start_point: develop` is resolved against the local clone as the
  // MCP open resolves it (its cached `origin/develop`), not sent as a bare `develop`.
  it("a configured default_start_point is resolved to its remote ref before the machine gets it", async () => {
    vi.mocked(probeWorktreeConfig).mockResolvedValueOnce({
      status: { kind: "present" },
      config: { version: 1, setup: [], default_start_point: "develop" },
    } as never);
    vi.mocked(resolveWorktreeParams).mockResolvedValueOnce({
      branchName: "story/sw-9",
      startPoint: "develop",
    });
    vi.mocked(storyDefaultStartPoint).mockResolvedValueOnce("refs/remotes/origin/develop");
    await createOnBox();
    expect(storyDefaultStartPoint).toHaveBeenCalledWith(expect.any(String), "develop");
    expect(fake.prepareCalls[0]).toMatchObject({
      baseRef: "refs/remotes/origin/develop",
      branch: "story/sw-9",
    });
    // Its first terminal open settles here, not in the next test.
    await vi.waitFor(() => expect(fake.remoteTabs).toHaveLength(1));
  });

  it("is prepared there, recorded with where it lives, and opens its terminals there", async () => {
    const cwd = await createOnBox();

    // Remote create = prepare, with paths derived on the machine (not sent).
    expect(fake.prepareCalls).toHaveLength(1);
    const call = fake.prepareCalls[0];
    expect(call).toMatchObject({
      machineId: "box",
      teamKey: TEAM,
      workspaceId: WS,
      storyId: "SW-9",
      sourceRepo: "test/repo",
      baseRef: "main",
      branch: "story/sw-9",
    });
    expect(call.requestId).toMatch(/^[0-9a-f-]{36}$/);
    expect(call).not.toHaveProperty("repoPath");
    expect(call).not.toHaveProperty("worktreePath");
    // The backend held the creating record when the prepare arrived.
    const pushed = fake.pushedAtPrepare[0] as {
      workspaces: Record<string, Array<{ id: string; status: string; location?: unknown }>>;
    };
    expect(pushed.workspaces[TEAM]).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          id: WS,
          status: "creating",
          location: expect.objectContaining({ machineId: "box", requestId: call.requestId }),
        }),
      ]),
    );
    // The creating overlay showed the machine's progress.
    expect(fake.labelsDuringPrepare[0]).toEqual({
      kind: "step1",
      phase: "copying",
      repo: "repo",
      machine: "codesense-test-1",
      done: 50,
      total: 100,
      unit: "bytes",
    });

    // Tracked, not typed: the record holds the location, handle and remote cwd;
    // nothing local was created and its local cwd stays empty.
    expect(cwd).toBe("/home/dev/codesense/worktrees/default/SW-9");
    expect(record()).toMatchObject({
      status: "ready",
      cwd: "",
      location: {
        kind: "machine",
        machineId: "box",
        requestId: call.requestId,
        handle: `remote-workspace-${call.requestId}`,
        cwd: "/home/dev/codesense/worktrees/default/SW-9",
      },
    });
    expect(createWorktree).not.toHaveBeenCalled();
    // SW-XS-10 (X5): Step 1 finished, no agent has been prepared — the setup
    // line never says Ready (the removed early ready).
    expect(record()?.setupState?.kind).not.toBe("ready");

    // Its first terminal opens on the machine, at the worktree; never locally.
    await vi.waitFor(() => expect(fake.remoteTabs).toHaveLength(1));
    expect(fake.remoteTabs[0]).toMatchObject({
      operation: "create",
      machineId: "box",
      workspaceId: WS,
      teamKey: TEAM,
      cwd: "/home/dev/codesense/worktrees/default/SW-9",
    });
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)).toHaveLength(0);

    // ⌘T opens the next tab there too.
    newTerminalTab(TEAM, WS);
    await vi.waitFor(() => expect(fake.remoteTabs).toHaveLength(2));
    expect(fake.remoteTabs[1]).toMatchObject({
      machineId: "box",
      cwd: "/home/dev/codesense/worktrees/default/SW-9",
    });
    await expect(
      useViewerStore
        .getState()
        .addTerminalTab(TEAM, { sessionId: "local-1", name: "x" } as never, WS),
    ).rejects.toThrow(/remote machine/);

    // The answer is the team's next default.
    expect(JSON.parse(localStorage.getItem("codesense.workspace-where") ?? "{}")).toEqual({
      [TEAM]: "box",
    });
  });

  it("opens again instantly: no Where question, no prepare, no bundle", async () => {
    await createOnBox();
    expect(fake.bundlesSent).toBe(1);
    cleanup();
    await act(async () => {
      await useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, STORY);
    });
    expect(screen.queryByTestId("workspace-where-dialog")).toBeNull();
    expect(fake.prepareCalls).toHaveLength(1);
    expect(fake.bundlesSent).toBe(1);
  });

  it("persists where it lives, and an interrupted create reconciles the same request", async () => {
    await createOnBox();
    // Saves are field patches; merge them all as the backend does.
    const blob = vi
      .mocked(saveAppState)
      .mock.calls.reduce<unknown>(
        (state, [patch]) => applyFieldPatch(state, JSON.parse(patch as string)),
        {},
      ) as {
      viewer: { workspaces: Record<string, Array<{ id: string; location?: { handle?: string } }>> };
    };
    const row = blob.viewer.workspaces[TEAM].find((w) => w.id === WS);
    expect(row?.location?.handle).toBe(`remote-workspace-${fake.prepareCalls[0].requestId}`);

    // A quit mid-create leaves the record without its handle: reopening the
    // story finishes it under the SAME request, and the machine sends no
    // second bundle.
    const requestId = fake.prepareCalls[0].requestId;
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [TEAM]: s.workspaces[TEAM].map((w) =>
          w.id === WS
            ? {
                ...w,
                status: "error" as const,
                location: { kind: "machine" as const, machineId: "box", requestId },
              }
            : w,
        ),
      },
    }));
    cleanup();
    await act(async () => {
      await useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, STORY);
    });
    expect(screen.queryByTestId("workspace-where-dialog")).toBeNull();
    expect(fake.prepareCalls).toHaveLength(2);
    expect(fake.prepareCalls[1].requestId).toBe(requestId);
    expect(fake.bundlesSent).toBe(1);
    expect(record()?.status).toBe("ready");
  });

  it("a retry replays the base and branch it was made with, even after the story's branch changed", async () => {
    // Only the viewer's own replay: the backend's prior-intent replay is off.
    fake.replay = false;
    await createOnBox();
    const { requestId } = fake.prepareCalls[0];
    expect(record()?.location).toMatchObject({ baseRef: "main", branch: "story/sw-9" });
    useViewerStore.setState((s) => ({
      workspaces: {
        ...s.workspaces,
        [TEAM]: s.workspaces[TEAM].map((w) =>
          w.id === WS && w.location
            ? {
                ...w,
                status: "error" as const,
                location: { ...w.location, handle: undefined, cwd: undefined },
              }
            : w,
        ),
      },
    }));
    vi.mocked(resolveWorktreeParams).mockResolvedValueOnce({
      branchName: "story/sw-9-renamed",
      startPoint: "release",
    } as never);
    cleanup();
    await act(async () => {
      await useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, STORY);
    });
    expect(fake.prepareCalls[1]).toMatchObject({
      requestId,
      baseRef: "main",
      branch: "story/sw-9",
    });
    expect(record()?.status).toBe("ready");
  });

  it("a record lost after its worktree was made adopts that preparation, not a refusal", async () => {
    await createOnBox();
    const first = fake.prepareCalls[0].requestId;
    // The record is gone (closed, or the saved state lost) and the story's base moved.
    useViewerStore.setState({ workspaces: {}, softClosedWorkspaces: {} });
    vi.mocked(resolveWorktreeParams).mockResolvedValueOnce({
      branchName: "story/sw-9",
      startPoint: "release",
    } as never);
    cleanup();
    await createOnBox();
    expect(fake.prepareCalls[1].requestId).not.toBe(first);
    expect(fake.bundlesSent).toBe(1);
    expect(record()).toMatchObject({
      status: "ready",
      location: { requestId: first, baseRef: "main", handle: `remote-workspace-${first}` },
    });
  });

  it("an open on a machine never opens a workspace that lives elsewhere", async () => {
    vi.mocked(createWorktree).mockResolvedValue({
      worktreePath: "/wt/story-sw-9",
      rehydrated: false,
    } as never);
    useMachinesStore.setState({ byTeam: { [TEAM]: [] } });
    await act(async () => {
      await useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, STORY);
    });
    expect(record()?.location).toBeUndefined();
    await expect(
      useViewerStore.getState().ensureStoryWorkspaceOpen(TEAM, "SW-9", {
        location: { kind: "machine", machineId: "box" },
      }),
    ).rejects.toThrow(/already lives on This Mac/);
    await expect(
      useViewerStore
        .getState()
        .openOrFocusStoryWorkspace(TEAM, STORY, undefined, undefined, true, true, {
          kind: "machine",
          machineId: "box",
        }),
    ).rejects.toThrow(/already lives on This Mac/);
    expect(fake.prepareCalls).toHaveLength(0);
  });

  it("a team without machines is never asked, and stays on This Mac", async () => {
    useMachinesStore.setState({ byTeam: { [TEAM]: [] } });
    vi.mocked(createWorktree).mockResolvedValue({
      worktreePath: "/wt/story-sw-9",
      rehydrated: false,
    } as never);
    render(<WorkspaceWhereDialog />);
    await act(async () => {
      await useViewerStore.getState().openOrFocusStoryWorkspace(TEAM, STORY);
    });
    expect(screen.queryByTestId("workspace-where-dialog")).toBeNull();
    expect(fake.prepareCalls).toHaveLength(0);
    expect(record()?.location).toBeUndefined();
    expect(record()?.cwd).toBe("/wt/story-sw-9");
  });
});

describe("SW-XS-10: the setup line of a machine workspace", () => {
  it("SW-XS-10 AC1: with an agent launch expected, Step 1 ends waiting for its Step 2, then Ready", async () => {
    expectAgentLaunch(TEAM, WS);
    await createOnBox();
    expect(record()).toMatchObject({ status: "ready", setupState: { kind: "awaitingAgent" } });
    const handle = record()!.location!.handle!;
    const base = { workspace: handle, agent: "Barb", files: 0, bytes: 0, total: 3, settled: false };
    act(() => useViewerStore.getState().applyAgentPrepareProgress({ ...base, phase: "diff" }));
    // The agent's run is its own slot; the base line still waits.
    expect(record()?.agentRuns?.Barb).toMatchObject({ kind: "running", agent: "Barb" });
    expect(record()?.setupState?.kind).toBe("awaitingAgent");
    act(() =>
      useViewerStore
        .getState()
        .applyAgentPrepareProgress({ ...base, phase: "ready", settled: true }),
    );
    expect(record()?.setupState?.kind).toBe("ready");
    expect(record()?.agentRuns).toBeUndefined();
  });
});

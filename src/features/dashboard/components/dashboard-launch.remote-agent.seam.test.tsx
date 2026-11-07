// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, cleanup } from "@/test-utils";

// Seam: the real story agent launch (`openStoryWorkspace` over the real launch
// deps) for a story whose workspace lives on a machine, through the real
// machines driver and remote-tab open path, to a fake machine at the Tauri
// boundary. The agent starts there, as the named Agent, at the worktree;
// nothing local is created or spawned. The machine's refusals (CLI missing,
// agents not allowed) show in the workspace with what to do next.

const fake = vi.hoisted(() => ({
  remoteTabs: [] as Array<Record<string, unknown>>,
  callers: [] as unknown[],
  answers: [] as Array<Record<string, unknown>>,
  allowed: [] as Array<[string, boolean]>,
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

vi.mock("@/shared/lib/private-terminal-authority", () => ({
  acceptsPrivateAuthority: () => true,
  currentPrivateAuthorityEpoch: () => "1",
  onPrivateAuthorityRetired: () => () => {},
  observePrivateAuthority: () => {},
  retirePrivateAuthority: () => {},
  listenForPrivateAuthorityRetirement: () => Promise.resolve(),
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  listMachines: vi.fn().mockResolvedValue([]),
  setMachineRemoteMcpControl: vi.fn(async (machineId: string, enabled: boolean) => {
    fake.allowed.push([machineId, enabled]);
  }),
  connectMachine: vi.fn(async () => BOX),
  dashboardBegin: vi.fn(async () => {
    throw new Error("a remote launch reserved a local session");
  }),
}));

vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-launch-source")
    >()),
    remoteShellTab: vi.fn(async (request: Record<string, unknown>, caller?: unknown) => {
      fake.remoteTabs.push(request);
      fake.callers.push(caller);
      return fake.answers.shift() ?? { status: "unknown", headed: false, message: "fake machine" };
    }),
  }),
);

import { useViewerStore } from "@/shared/stores/viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import "@/features/machines/lib/remote-story-workspace";
import { openStoryWorkspace } from "./dashboard-launch";
import { makeRealDashboardLaunchDeps } from "./dashboard-launch-controller";
import { PendingRemoteOpens } from "@/features/viewer/components/pending-remote-opens";
import { clearRemoteOpens } from "@/features/viewer/lib/remote-tab-open";
import type { MachineSummary } from "@/tauri/commands";

const TEAM = "test-org:default";
const STORY = "/ctx/stories/sw-9-remote.md";
const WS = "sw-9-remote";
const CWD = "/home/dev/codesense/worktrees/default/SW-9";

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

function deps(frameId?: string) {
  const real = makeRealDashboardLaunchDeps("/ctx/dashboards/board.html", frameId);
  return {
    ...real,
    resolveStory: async () => ({ storyFilePath: STORY, teamKey: TEAM }),
    ensureWorktree: vi.fn(real.ensureWorktree),
    connect: vi.fn(real.connect),
  };
}

beforeEach(() => {
  cleanup();
  fake.remoteTabs = [];
  fake.callers = [];
  fake.answers = [];
  fake.allowed = [];
  clearRemoteOpens();
  localStorage.clear();
  useMachinesStore.setState({ byTeam: { [TEAM]: [BOX] } });
  useViewerStore.setState({
    stateLoaded: true,
    softClosedWorkspaces: {},
    activeWorkspace: { [TEAM]: WS },
    workspaces: {
      [TEAM]: [
        {
          id: WS,
          kind: "story",
          storyId: "SW-9",
          storyFilePath: STORY,
          cwd: "",
          status: "ready",
          location: {
            kind: "machine",
            machineId: "box",
            requestId: "00000000-0000-4000-8000-000000000009",
            handle: "remote-workspace-h",
            cwd: CWD,
          },
        } as never,
      ],
    },
  });
});

describe("an agent launched for a story that lives on a machine", () => {
  it("starts there as the built-in Agent, at the worktree, and never locally", async () => {
    const launchDeps = deps();
    fake.answers.push({
      status: "refused",
      headed: false,
      code: "agentCliMissing",
      message: "Claude Code isn't installed on this machine",
    });
    await expect(openStoryWorkspace("SW-9", launchDeps)).rejects.toThrow(
      /Claude Code isn't installed/,
    );

    expect(fake.remoteTabs).toHaveLength(1);
    expect(fake.remoteTabs[0]).toMatchObject({
      operation: "create",
      machineId: "box",
      teamKey: TEAM,
      workspaceId: WS,
      cwd: CWD,
      agentKey: "builtin/claude",
      preparedWorkspaceHandle: "remote-workspace-h",
    });
    expect(fake.remoteTabs[0]).not.toHaveProperty("mcpProfile");
    expect(launchDeps.ensureWorktree).not.toHaveBeenCalled();
    expect(launchDeps.connect).not.toHaveBeenCalled();
  });

  it("its first prompt goes to the machine with the launch, so the agent starts with it", async () => {
    const launchDeps = deps();
    fake.answers.push({ status: "refused", headed: false, code: "invalid", message: "stop here" });
    await expect(
      openStoryWorkspace("SW-9", launchDeps, { initialPrompt: "Fix the flaky test" }),
    ).rejects.toThrow(/stop here/);
    expect(fake.remoteTabs[0]).toMatchObject({
      agentKey: "builtin/claude",
      initialPrompt: "Fix the flaky test",
    });
  });

  it("a named Codex agent starts that agent; an Agent Account is refused, not run locally", async () => {
    const launchDeps = deps();
    await expect(
      openStoryWorkspace("SW-9", launchDeps, { agent: "builtin/codex" }),
    ).rejects.toThrow();
    expect(fake.remoteTabs[0]).toMatchObject({ agentKey: "builtin/codex" });
    await expect(
      openStoryWorkspace("SW-9", launchDeps, {
        agent: "builtin/codex",
        account: { userId: 1, id: "acct", generation: 1 },
      }),
    ).rejects.toThrow(/lives on a machine/);
    expect(fake.remoteTabs).toHaveLength(1);
    expect(launchDeps.connect).not.toHaveBeenCalled();
  });

  it("a missing CLI says so with its install command, and Try again asks anew", async () => {
    fake.answers.push({
      status: "refused",
      headed: false,
      code: "agentCliMissing",
      message: "Claude Code isn't installed on this machine",
    });
    await openStoryWorkspace("SW-9", deps()).catch(() => {});
    render(<PendingRemoteOpens teamKey={TEAM} workspaceId={WS} />);
    expect(await screen.findByText("Claude isn't installed on codesense-test-1")).toBeTruthy();
    expect(screen.getByTestId("pending-remote-open-install").textContent).toBe(
      "curl -fsSL https://claude.ai/install.sh | bash",
    );
    expect(screen.queryByText(/command not found/)).toBeNull();
    await act(async () => {
      fireEvent.click(screen.getByText("Try again"));
    });
    await vi.waitFor(() => expect(fake.remoteTabs).toHaveLength(2));
    expect(fake.remoteTabs[1].requestId).not.toBe(fake.remoteTabs[0].requestId);
    expect(fake.remoteTabs[1]).toMatchObject({ agentKey: "builtin/claude", cwd: CWD });
  });

  it("agents not allowed offers to allow them, then launches again", async () => {
    fake.answers.push({
      status: "refused",
      headed: false,
      code: "agentsNotAllowed",
      message: "Agents aren't allowed on this machine yet",
    });
    await openStoryWorkspace("SW-9", deps()).catch(() => {});
    render(<PendingRemoteOpens teamKey={TEAM} workspaceId={WS} />);
    expect(await screen.findByText("Agents aren't allowed on codesense-test-1 yet")).toBeTruthy();
    const allow = screen.getByTestId("pending-remote-open-allow-agents");
    expect(allow.textContent).toBe("Allow agents on codesense-test-1");
    fireEvent.click(allow);
    await act(async () => {
      fireEvent.click(screen.getByTestId("pending-remote-open-allow-agents-confirm"));
    });
    await vi.waitFor(() => expect(fake.remoteTabs).toHaveLength(2));
    expect(fake.allowed).toEqual([["box", true]]);
    expect(fake.remoteTabs[1]).toMatchObject({ agentKey: "builtin/claude" });
  });
});

describe("SW-XS-10: a launch's refusal reaches the workspace's setup line", () => {
  it("a too-old terminal server lands on the line by its typed code, and Retry launches that agent again", async () => {
    const { agentLaunchRetry } = await import("@/features/viewer/lib/agent-launch-retry");
    fake.answers.push({
      status: "refused",
      headed: false,
      code: "terminalServerOutdated",
      message: "Update the terminal server on box: it is too old to sync agent context",
    });
    await openStoryWorkspace("SW-9", deps()).catch(() => {});
    const ws = useViewerStore.getState().workspaces[TEAM].find((w) => w.id === WS)!;
    const runs = Object.values(ws.agentRuns ?? {});
    expect(runs).toHaveLength(1);
    expect(runs[0]).toMatchObject({ kind: "failed", code: "terminalServerOutdated" });
    // The recorded launch is found by the name the line carries.
    const retry = agentLaunchRetry(TEAM, WS, runs[0].agent);
    expect(retry).toBeDefined();
    await retry!().catch(() => {});
    expect(fake.remoteTabs).toHaveLength(2);
    expect(fake.remoteTabs[1]).toMatchObject({ agentKey: "builtin/claude" });
  });

  it("an expected launch whose create failed is forgotten (a later create waits for no agent)", async () => {
    const { agentLaunchExpected } = await import("@/shared/stores/viewer-store");
    const real = makeRealDashboardLaunchDeps("/ctx/dashboards/board.html");
    useViewerStore.setState({
      ensureWorktree: vi.fn().mockRejectedValue(new Error("no clone attached")),
    });
    await expect(real.ensureWorktree(TEAM, "/ctx/stories/sw-10-local.md")).rejects.toThrow();
    expect(agentLaunchExpected(TEAM, "sw-10-local")).toBe(false);
  });
});

it("page remote-story launches retain caller authority through the native tab boundary", async () => {
  const launchDeps = deps("child-frame");
  fake.answers.push({
    status: "refused",
    headed: false,
    code: "notFound",
    message: "File not found",
  });
  await expect(
    openStoryWorkspace("SW-9", launchDeps, {
      agent: "acme/ctx/parent/reviewer",
      sessionMode: "new",
    }),
  ).rejects.toThrow("File not found");
  expect(fake.remoteTabs[0]).toMatchObject({ agentKey: "acme/ctx/parent/reviewer", teamKey: TEAM });
  expect(fake.callers[0]).toEqual({
    frameId: "child-frame",
    pagePath: "/ctx/dashboards/board.html",
  });
  expect(launchDeps.connect).not.toHaveBeenCalled();
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";

import { logger } from "@/shared/lib/logger";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { revealOpenedWorkspace } from "./use-open-workspace-reveal";

const showAgent = vi.hoisted(() => vi.fn(async () => ({ shown: "adopted" as const })));
vi.mock("@/features/viewer/lib/show-agent", () => ({ showAgent }));

const resolveOpenWorkspaceMachine = vi.hoisted(() => vi.fn(async (): Promise<void> => undefined));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  resolveOpenWorkspaceMachine,
}));

const actualOpenWorkspace = useViewerStore.getState().openWorkspace;
const ensureStoryWorkspaceOpen = vi.fn(async (): Promise<string | null> => "SW-9");

afterEach(() => {
  ensureStoryWorkspaceOpen.mockReset();
  resolveOpenWorkspaceMachine.mockClear();
  vi.restoreAllMocks();
  useViewerStore.setState({ openWorkspace: actualOpenWorkspace });
});

function reveal(focus: boolean) {
  useViewerStore.setState({ ensureStoryWorkspaceOpen });
  return revealOpenedWorkspace({ handle: "SW-9", teamKey: "acme/app:core", focus });
}

describe("revealOpenedWorkspace — show the story an MCP open_workspace opened", () => {
  it("a running session no workspace shows goes through the one Show (adopted into a tab)", async () => {
    await revealOpenedWorkspace({
      target: "session",
      handle: "SW-9",
      teamKey: "acme/app:core",
      sessionId: "shell-7",
      focus: true,
    });
    expect(showAgent).toHaveBeenCalledWith(
      expect.objectContaining({ handle: "SW-9", sessionId: "shell-7", alive: true }),
      { teamKey: "acme/app:core", stayInTeam: true },
    );
  });

  it("a headless run opens its run output through the same Show", async () => {
    await revealOpenedWorkspace({
      target: "runOutput",
      handle: "SW-9",
      teamKey: "acme/app:core",
      sessionId: "headless-1",
      runId: "run-1",
      focus: true,
    });
    expect(showAgent).toHaveBeenLastCalledWith(
      expect.objectContaining({ kind: "headless", provenance: { runId: "run-1", storyId: "" } }),
      { teamKey: "acme/app:core", stayInTeam: true },
    );
  });

  it("a headed open opens the story's workspace and focuses it", async () => {
    await reveal(true);
    expect(ensureStoryWorkspaceOpen).toHaveBeenCalledWith("acme/app:core", "SW-9", {
      focus: true,
    });
  });

  it("a background open opens the workspace without taking focus", async () => {
    await reveal(false);
    expect(ensureStoryWorkspaceOpen).toHaveBeenCalledWith("acme/app:core", "SW-9", {
      focus: false,
    });
  });

  it("a named session in a story reveals that story's workspace", async () => {
    useViewerStore.setState({ ensureStoryWorkspaceOpen });
    await revealOpenedWorkspace({
      target: "story",
      handle: "SW-9#worker-1",
      teamKey: "acme/app:core",
      workspaceId: "SW-9",
      focus: false,
    });
    expect(ensureStoryWorkspaceOpen).toHaveBeenCalledWith("acme/app:core", "SW-9", {
      focus: false,
    });
  });

  it("an open on a machine opens (creating there) the story's workspace on that machine", async () => {
    useViewerStore.setState({ ensureStoryWorkspaceOpen });
    await revealOpenedWorkspace({
      target: "story",
      handle: "SW-9",
      teamKey: "acme/app:core",
      focus: true,
      machineId: "box",
    });
    expect(ensureStoryWorkspaceOpen).toHaveBeenCalledWith("acme/app:core", "SW-9", {
      focus: true,
      location: { kind: "machine", machineId: "box" },
    });
  });

  it("a story it can't open is logged, never thrown to the event listener", async () => {
    const warn = vi.spyOn(logger, "warn").mockImplementation(() => {});
    ensureStoryWorkspaceOpen.mockRejectedValueOnce(new Error("no such story"));
    await expect(reveal(true)).resolves.toBeUndefined();
    expect(warn).toHaveBeenCalledWith("mcp", expect.stringContaining("SW-9"));
  });
});

describe("revealOpenedWorkspace — reports a machine open back to the backend (SW-FX-131)", () => {
  const machineReveal = {
    target: "story" as const,
    handle: "SW-9",
    teamKey: "acme/app:core",
    focus: true,
    machineId: "box",
    requestId: "machine-open-7",
  };

  it("replies success once the story's workspace is open on the machine", async () => {
    useViewerStore.setState({ ensureStoryWorkspaceOpen });
    await revealOpenedWorkspace(machineReveal);
    expect(resolveOpenWorkspaceMachine).toHaveBeenCalledWith("machine-open-7", null);
  });

  it("replies with the failure reason when the open throws, and still logs it", async () => {
    const warn = vi.spyOn(logger, "warn").mockImplementation(() => {});
    useViewerStore.setState({ ensureStoryWorkspaceOpen });
    ensureStoryWorkspaceOpen.mockRejectedValueOnce(
      new Error("SW-9 already lives on This Mac; a workspace stays where it was created"),
    );
    await expect(revealOpenedWorkspace(machineReveal)).resolves.toBeUndefined();
    expect(resolveOpenWorkspaceMachine).toHaveBeenCalledWith(
      "machine-open-7",
      "SW-9 already lives on This Mac; a workspace stays where it was created",
    );
    expect(warn).toHaveBeenCalledWith("mcp", expect.stringContaining("SW-9"));
  });

  it("replies with a failure when the story can't be opened (no workspace id)", async () => {
    useViewerStore.setState({ ensureStoryWorkspaceOpen });
    ensureStoryWorkspaceOpen.mockResolvedValueOnce(null);
    await revealOpenedWorkspace(machineReveal);
    expect(resolveOpenWorkspaceMachine).toHaveBeenCalledWith(
      "machine-open-7",
      expect.stringContaining("SW-9"),
    );
  });

  it("does not reply when no open is waiting on it", async () => {
    useViewerStore.setState({ ensureStoryWorkspaceOpen });
    await revealOpenedWorkspace({ ...machineReveal, requestId: undefined });
    expect(resolveOpenWorkspaceMachine).not.toHaveBeenCalled();
  });
});

it.each([true, false])(
  "worker reveal keeps its own team and workspace with focus=%s",
  async (focus) => {
    const openWorkspace = vi.fn();
    useViewerStore.setState({ openWorkspace });
    await revealOpenedWorkspace({
      target: "team-workspace",
      teamKey: "acme/app:other-team",
      handle: "_worker/acme/app/other-team/sprint-1",
      workspaceId: "worker-home",
      cwd: "/managed/sprint-1",
      focus,
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      teamKey: "acme/app:other-team",
      workspaceId: "worker-home",
      kind: "generic",
      cwd: "/managed/sprint-1",
      focus,
      restoreSoftClosed: true,
      spawn: false,
    });
    expect(ensureStoryWorkspaceOpen).not.toHaveBeenCalled();
  },
);

it("two team agents reveal one retained context-worktree pill without spawning a terminal", async () => {
  useViewerStore.setState({ workspaces: {}, activeWorkspace: {}, softClosedWorkspaces: {} });
  for (const handle of ["first", "second"]) {
    await revealOpenedWorkspace({
      target: "team-workspace",
      handle,
      teamKey: "acme:core",
      workspaceId: "cw_one",
      cwd: "/worktrees/sprint-1/core/content",
      workspaceKind: "context",
      workspaceName: "Sprint 1",
      branch: "core/sprint-1",
      focus: false,
    });
  }
  expect(useViewerStore.getState().workspaces["acme:core"]).toEqual([
    expect.objectContaining({
      id: "cw_one",
      kind: "context",
      contextWorktreeId: "cw_one",
      contextWorktreeName: "Sprint 1",
      contextWorktreeBranch: "core/sprint-1",
      cwd: "/worktrees/sprint-1/core/content",
    }),
  ]);
  useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

const { mockDirty, mockUnpublished, mockRemove, mockEnd, mockLiveCount, mockAlive } = vi.hoisted(
  () => ({
    mockDirty: vi.fn(),
    mockUnpublished: vi.fn(),
    mockRemove: vi.fn(),
    mockEnd: vi.fn(),
    mockLiveCount: vi.fn(),
    mockAlive: vi.fn(),
  }),
);
vi.mock("@/tauri/commands", () => ({
  checkWorktreeDirty: mockDirty,
  getWorktreeUnpublishedContext: mockUnpublished,
  removeWorktree: mockRemove,
  worktreeLiveSessionCount: mockLiveCount,
  dashboardProbeSessionsAlive: mockAlive,
}));
vi.mock("../lib/terminal-close", () => ({ endWorkspace: mockEnd }));
vi.mock("@/features/viewer/lib/cli-workspace/store/viewer-access", () => ({
  getViewerState: () => ({ removeOnDiskWorktree: vi.fn() }),
  selectWorkspaceTabs: () => [{ sessionId: "own-1" }],
  selectHiddenTerminalTabs: () => [],
}));

import {
  RemoveWorktreeDialog,
  removeWorktreeTargetFor,
  type RemoveWorktreeTarget,
} from "./remove-worktree-dialog";
import type { WorktreeRow } from "@/tauri/commands";

const TARGET: RemoveWorktreeTarget = {
  teamKey: "acme:core",
  workspaceId: "ws-1",
  path: "/wt/acme/api/story-1",
  branch: "story/SW-1",
  repoPath: "/clones/acme/api",
};

const row = (over: Partial<WorktreeRow> = {}) =>
  ({
    kind: "story",
    workspaceKey: "SW-1",
    path: TARGET.path,
    branch: TARGET.branch,
    dirty: false,
    repoPath: TARGET.repoPath,
    liveCliSession: false,
    ...over,
  }) as WorktreeRow;

beforeEach(() => {
  vi.clearAllMocks();
  mockDirty.mockResolvedValue({ dirty: false });
  mockUnpublished.mockResolvedValue({ count: 0, files: [] });
  mockRemove.mockResolvedValue(undefined);
  mockEnd.mockResolvedValue(true);
  // The workspace's own tab is the one live session in the folder until it is ended.
  mockAlive.mockResolvedValue(["own-1"]);
  mockLiveCount.mockResolvedValue(1);
  mockEnd.mockImplementation(async () => {
    mockLiveCount.mockResolvedValue(0);
    return true;
  });
});

async function openAndConfirm() {
  render(<RemoveWorktreeDialog target={TARGET} onClose={vi.fn()} />);
  const confirm = screen.getByTestId("remove-worktree-confirm");
  await waitFor(() => expect(confirm).not.toBeDisabled());
  fireEvent.click(confirm);
}

describe("removeWorktreeTargetFor", () => {
  it("matches a local workspace to the CodeSense worktree it sits in", () => {
    const t = removeWorktreeTargetFor("acme:core", { id: "ws-1", cwd: `${TARGET.path}/` }, [row()]);
    expect(t).toMatchObject({
      path: TARGET.path,
      branch: TARGET.branch,
      repoPath: TARGET.repoPath,
    });
  });

  it("offers nothing for a plain folder, a machine workspace, a vanished clone or a worktree made outside CodeSense", () => {
    expect(removeWorktreeTargetFor("t", { id: "w", cwd: "/elsewhere" }, [row()])).toBeNull();
    expect(
      removeWorktreeTargetFor(
        "t",
        { id: "w", cwd: TARGET.path, location: { kind: "machine" } as never },
        [row()],
      ),
    ).toBeNull();
    expect(
      removeWorktreeTargetFor("t", { id: "w", cwd: TARGET.path }, [row({ repoPath: null })]),
    ).toBeNull();
    expect(
      removeWorktreeTargetFor("t", { id: "w", cwd: TARGET.path }, [
        row({ kind: "orphan" } as Partial<WorktreeRow>),
      ]),
    ).toBeNull();
    expect(removeWorktreeTargetFor("t", { id: "w", cwd: TARGET.path }, null)).toBeNull();
  });
});

describe("RemoveWorktreeDialog", () => {
  it("names the folder, branch and ignored files, then ends the workspace and removes it discarding ignored files only", async () => {
    render(<RemoveWorktreeDialog target={TARGET} onClose={vi.fn()} />);
    const dialog = screen.getByTestId("remove-worktree-dialog");
    expect(dialog).toHaveTextContent(TARGET.path);
    expect(dialog).toHaveTextContent(TARGET.branch!);
    expect(dialog).toHaveTextContent(/ignored files such as node_modules/);
    const confirm = screen.getByTestId("remove-worktree-confirm");
    await waitFor(() => expect(confirm).not.toBeDisabled());
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(mockRemove).toHaveBeenCalledWith(TARGET.repoPath, TARGET.path, false, true),
    );
    expect(mockEnd).toHaveBeenCalledWith(TARGET.teamKey, TARGET.workspaceId);
    expect(mockEnd.mock.invocationCallOrder[0]).toBeLessThan(
      mockRemove.mock.invocationCallOrder[0],
    );
  });

  it.each([
    [
      "uncommitted changes",
      () => mockDirty.mockResolvedValue({ dirty: true }),
      /uncommitted changes/,
    ],
    [
      "unpublished context edits",
      () => mockUnpublished.mockResolvedValue({ count: 2, files: ["a.md", "b.md"] }),
      /2 unpublished context edits/,
    ],
    [
      "a failed check",
      () => mockDirty.mockRejectedValue(new Error("git exploded")),
      /git exploded/,
    ],
    [
      "a session outside this workspace",
      () => mockLiveCount.mockResolvedValue(2),
      /Something else is running in this folder: 1 session outside this workspace/,
    ],
    [
      "unknown session liveness",
      () => mockLiveCount.mockResolvedValue(null),
      /couldn't check what is running/,
    ],
  ])("refuses on %s without ending anything", async (_name, arrange, message) => {
    arrange();
    render(<RemoveWorktreeDialog target={TARGET} onClose={vi.fn()} />);
    expect(await screen.findByTestId("remove-worktree-blocked")).toHaveTextContent(message);
    expect(screen.getByTestId("remove-worktree-confirm")).toBeDisabled();
    expect(mockEnd).not.toHaveBeenCalled();
    expect(mockRemove).not.toHaveBeenCalled();
  });

  it("re-checks on confirm and refuses before ending when something changed", async () => {
    const onClose = vi.fn();
    render(<RemoveWorktreeDialog target={TARGET} onClose={onClose} />);
    const confirm = screen.getByTestId("remove-worktree-confirm");
    await waitFor(() => expect(confirm).not.toBeDisabled());
    mockDirty.mockResolvedValue({ dirty: true });
    fireEvent.click(confirm);
    expect(await screen.findByTestId("remove-worktree-blocked")).toHaveTextContent(
      /uncommitted changes/,
    );
    expect(onClose).not.toHaveBeenCalled();
    expect(mockEnd).not.toHaveBeenCalled();
    expect(mockRemove).not.toHaveBeenCalled();
  });

  it("waits for the ended sessions to exit before removing", async () => {
    let polls = 0;
    mockEnd.mockImplementation(async () => {
      mockLiveCount.mockImplementation(async () => (++polls < 3 ? 1 : 0));
      return true;
    });
    await openAndConfirm();
    await waitFor(() => expect(mockRemove).toHaveBeenCalled(), { timeout: 3000 });
    expect(polls).toBeGreaterThanOrEqual(3);
  });

  it("keeps the worktree when the ending agent left an unpublished context edit", async () => {
    mockEnd.mockImplementation(async () => {
      mockUnpublished.mockResolvedValue({ count: 1 });
      return true;
    });
    await openAndConfirm();
    await waitFor(() => expect(mockEnd).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(mockRemove).not.toHaveBeenCalled();
  });

  it("keeps the worktree when the user cancels ending the workspace", async () => {
    mockEnd.mockResolvedValue(false);
    await openAndConfirm();
    await waitFor(() => expect(mockEnd).toHaveBeenCalled());
    expect(mockRemove).not.toHaveBeenCalled();
  });
});

it("does not offer deletion of a retained context worktree", () => {
  expect(
    removeWorktreeTargetFor("acme:core", { id: "cw_one", cwd: TARGET.path }, [
      row({ kind: "context" }),
    ]),
  ).toBeNull();
});

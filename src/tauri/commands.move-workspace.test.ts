// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, expectTypeOf, it, vi } from "vitest";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import {
  listMoveIntentsBridge,
  moveWorkspaceBridge,
  type MoveIntentView,
  type MoveWorkspaceResult,
} from "./commands";

beforeEach(() => mockInvoke.mockReset());

describe("workspace move bridges", () => {
  it("nests camelCase move inputs under params and preserves nullable results", async () => {
    const result: MoveWorkspaceResult = {
      moved: true,
      from: "remote-dev",
      to: null,
      handle: null,
      cwd: "/worktrees/story",
      cleanupPending: true,
    };
    mockInvoke.mockResolvedValueOnce(result);

    const pending = moveWorkspaceBridge({
      storyId: "story-1",
      targetMachine: "local",
      discardUncommitted: true,
      targetWorktreePath: "/worktrees/story",
    });

    expectTypeOf(pending).toEqualTypeOf<Promise<MoveWorkspaceResult>>();
    await expect(pending).resolves.toBe(result);
    expect(mockInvoke).toHaveBeenCalledExactlyOnceWith("move_workspace_bridge", {
      params: {
        storyId: "story-1",
        targetMachine: "local",
        discardUncommitted: true,
        targetWorktreePath: "/worktrees/story",
      },
    });
  });

  it("lists intents without arguments and preserves the Rust hydrate shape", async () => {
    const intents: MoveIntentView[] = [
      {
        teamKey: "org:team",
        workspaceId: "story-1",
        from: {
          machineId: null,
          requestId: "source-request",
          handle: null,
          cwd: null,
          repoPath: "/repo",
          repositoryKey: "github.com/org/repo",
        },
        to: { machineId: "remote-dev", requestId: "target-request" },
        step: "preparing-target",
        dirtyAllowed: false,
        startedAtMs: 100,
        lastProgressAtMs: 200,
      },
    ];
    mockInvoke.mockResolvedValueOnce(intents);

    const pending = listMoveIntentsBridge();

    expectTypeOf(pending).toEqualTypeOf<Promise<MoveIntentView[]>>();
    await expect(pending).resolves.toBe(intents);
    expect(mockInvoke).toHaveBeenCalledExactlyOnceWith("list_move_intents_bridge");
  });

  it("preserves Rust refusals for callers to handle", async () => {
    mockInvoke.mockRejectedValueOnce("dirty_worktree");
    await expect(
      moveWorkspaceBridge({ storyId: "story-1", targetMachine: "remote-dev" }),
    ).rejects.toBe("dirty_worktree");

    mockInvoke.mockRejectedValueOnce("intent root unavailable");
    await expect(listMoveIntentsBridge()).rejects.toBe("intent root unavailable");
  });
});

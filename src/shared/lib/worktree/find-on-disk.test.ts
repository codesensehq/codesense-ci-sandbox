// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { findWorktreeOnDisk } from "./find-on-disk";

vi.mock("@/tauri/commands", () => ({
  listWorktrees: vi.fn().mockResolvedValue([]),
  readFile: vi.fn().mockRejectedValue(new Error("not found")),
}));

import { listWorktrees, readFile } from "@/tauri/commands";

const listWorktreesMock = listWorktrees as unknown as ReturnType<typeof vi.fn>;
const readFileMock = readFile as unknown as ReturnType<typeof vi.fn>;

beforeEach(() => {
  listWorktreesMock.mockReset().mockResolvedValue([]);
  readFileMock.mockReset().mockRejectedValue(new Error("not found"));
});

describe("findWorktreeOnDisk", () => {
  const repoPath = "/repo";
  const storyFilePath = "/repo/stories/34-9-worktree-creation-gate.md";

  it("returns null when no worktrees exist on disk", async () => {
    listWorktreesMock.mockResolvedValue([]);

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toBeNull();
    expect(listWorktreesMock).toHaveBeenCalledWith(repoPath);
  });

  it("matches a worktree whose branch ends with the resolved branch name", async () => {
    listWorktreesMock.mockResolvedValue([
      {
        path: "/wt/feat-34-9",
        branch: "refs/heads/feat/34-9-worktree-creation-gate",
        head_sha: "abc",
      },
    ]);

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toEqual({
      path: "/wt/feat-34-9",
      branch: "refs/heads/feat/34-9-worktree-creation-gate",
      head_sha: "abc",
    });
  });

  it("matches a worktree whose branch exactly equals the resolved branch name", async () => {
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/feat-34-9", branch: "feat/34-9-worktree-creation-gate", head_sha: "abc" },
    ]);

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toEqual({
      path: "/wt/feat-34-9",
      branch: "feat/34-9-worktree-creation-gate",
      head_sha: "abc",
    });
  });

  it("returns null when branches don't match", async () => {
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/other", branch: "refs/heads/feat/other-story", head_sha: "abc" },
    ]);

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toBeNull();
  });

  it("skips entries with null branch (detached HEAD)", async () => {
    listWorktreesMock.mockResolvedValue([{ path: "/wt/detached", branch: null, head_sha: "abc" }]);

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toBeNull();
  });

  it("uses pre-fetched worktrees when provided, skipping the IPC call", async () => {
    const prefetched = [
      {
        path: "/wt/feat-34-9",
        branch: "refs/heads/feat/34-9-worktree-creation-gate",
        head_sha: "abc",
      },
    ];

    const result = await findWorktreeOnDisk(repoPath, storyFilePath, prefetched);

    expect(result).toEqual(prefetched[0]);
    expect(listWorktreesMock).not.toHaveBeenCalled();
  });

  it("uses explicit branch_name from story file frontmatter when available", async () => {
    readFileMock.mockResolvedValue("---\nbranch_name: custom/my-branch\n---\n# Story");
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/custom", branch: "refs/heads/custom/my-branch", head_sha: "abc" },
      {
        path: "/wt/feat-34-9",
        branch: "refs/heads/feat/34-9-worktree-creation-gate",
        head_sha: "def",
      },
    ]);

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toEqual({
      path: "/wt/custom",
      branch: "refs/heads/custom/my-branch",
      head_sha: "abc",
    });
  });

  it("returns null when listWorktrees fails", async () => {
    listWorktreesMock.mockRejectedValue(new Error("IPC error"));

    const result = await findWorktreeOnDisk(repoPath, storyFilePath);

    expect(result).toBeNull();
  });
});

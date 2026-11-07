// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";

const resolveLocalMock = vi.fn();
const resolveRemoteMock = vi.fn();

vi.mock("@/tauri/commands", () => ({
  resolveTabSourceRepo: (...args: unknown[]) => resolveLocalMock(...args),
  resolveTabSourceRepoRemote: (...args: unknown[]) => resolveRemoteMock(...args),
}));

import { useTabSourceRepo } from "./use-tab-source-repo";
import type { Workspace } from "@/shared/stores/viewer-store";

/**
 * DESIGN-per-tab-primitives.md §3.4 — the resolver reports two independent
 * signals: `folder` (any `sourceCapable` kind, no round-trip) + `repo` (git
 * only, tri-state while the backend resolves). The folder stays usable while
 * the Diff icon waits — a non-git cwd degrades to `repo:null` without
 * dropping the Source affordance.
 */
describe("useTabSourceRepo — split folder vs repo", () => {
  beforeEach(() => {
    resolveLocalMock.mockReset();
    resolveRemoteMock.mockReset();
  });

  const base: Partial<Workspace> = {
    id: "ws-1",
    kind: "story",
    cwd: "",
    status: "ready",
  };

  const localGeneric: Workspace = {
    ...base,
    id: "ws-local",
    kind: "generic",
    cwd: "/Users/dev/wt",
  } as Workspace;

  const localEpic: Workspace = {
    ...base,
    id: "ws-epic",
    kind: "epic",
    cwd: "/Users/dev/epic-dir",
  } as Workspace;

  const remoteStory: Workspace = {
    ...base,
    id: "ws-remote-ready",
    kind: "story",
    cwd: "",
    location: {
      kind: "machine",
      machineId: "m-01",
      requestId: "req-1",
      handle: "remote-workspace-abc",
      cwd: "/home/dev/wt",
    },
  } as Workspace;

  const remoteStoryPendingHandle: Workspace = {
    ...base,
    id: "ws-remote-pending",
    kind: "story",
    cwd: "",
    location: {
      kind: "machine",
      machineId: "m-01",
      requestId: "req-1",
    },
  } as Workspace;

  const remoteGeneric: Workspace = {
    ...base,
    id: "ws-remote-generic",
    kind: "generic",
    cwd: "",
    location: {
      kind: "machine",
      machineId: "m-01",
      requestId: "req-1",
      handle: "remote-workspace-abc",
      cwd: "/home/dev/notes",
    },
  } as Workspace;

  it("remote story workspace short-circuits — folder + repo both reported as virtual", () => {
    const { result } = renderHook(() => useTabSourceRepo(remoteStory));
    expect(result.current).toEqual({
      folder: { kind: "folder", cwd: "remote://m-01/remote-workspace-abc" },
      repo: { root: "remote://m-01/remote-workspace-abc", isRoot: true },
    });
    expect(resolveRemoteMock).not.toHaveBeenCalled();
    expect(resolveLocalMock).not.toHaveBeenCalled();
  });

  it("remote story with no handle yet: folder null, repo undefined (still resolving)", () => {
    const { result } = renderHook(() => useTabSourceRepo(remoteStoryPendingHandle));
    expect(result.current.folder).toBe(null);
    expect(result.current.repo).toBeUndefined();
    expect(resolveRemoteMock).not.toHaveBeenCalled();
  });

  it("remote generic on a git worktree reports folder + repo (both virtual)", async () => {
    resolveRemoteMock.mockResolvedValueOnce({
      root: "remote://m-01/remote-workspace-abc",
      isRoot: true,
    });
    const { result } = renderHook(() => useTabSourceRepo(remoteGeneric));
    await waitFor(() => expect(result.current.repo).not.toBe(undefined));
    expect(resolveRemoteMock).toHaveBeenCalledWith("m-01", "remote-workspace-abc");
    expect(result.current).toEqual({
      folder: { kind: "folder", cwd: "remote://m-01/remote-workspace-abc" },
      repo: { root: "remote://m-01/remote-workspace-abc", isRoot: true },
    });
  });

  it("remote generic on a non-git worktree: folder stays, repo=null (Diff hides, Source stays)", async () => {
    resolveRemoteMock.mockResolvedValueOnce(null);
    const { result } = renderHook(() => useTabSourceRepo(remoteGeneric));
    await waitFor(() => expect(result.current.repo).toBe(null));
    expect(result.current.folder).toEqual({
      kind: "folder",
      cwd: "remote://m-01/remote-workspace-abc",
    });
  });

  it("remote resolver rejection keeps the folder shape and reports repo=null", async () => {
    resolveRemoteMock.mockRejectedValueOnce(new Error("disconnected"));
    const { result } = renderHook(() => useTabSourceRepo(remoteGeneric));
    await waitFor(() => expect(result.current.repo).toBe(null));
    expect(result.current.folder?.cwd).toBe("remote://m-01/remote-workspace-abc");
  });

  it("local generic reports folder immediately, then repo after the LOCAL resolver settles", async () => {
    resolveLocalMock.mockResolvedValueOnce({ root: "/Users/dev/wt", isRoot: true });
    const { result } = renderHook(() => useTabSourceRepo(localGeneric));
    expect(result.current.folder).toEqual({ kind: "folder", cwd: "/Users/dev/wt" });
    await waitFor(() => expect(result.current.repo).not.toBe(undefined));
    expect(resolveLocalMock).toHaveBeenCalledWith("/Users/dev/wt");
    expect(result.current.repo).toEqual({ root: "/Users/dev/wt", isRoot: true });
  });

  it("local generic on a non-git dir still reports folder; repo settles to null (degrades)", async () => {
    resolveLocalMock.mockResolvedValueOnce(null);
    const { result } = renderHook(() => useTabSourceRepo(localGeneric));
    await waitFor(() => expect(result.current.repo).toBe(null));
    expect(result.current.folder).toEqual({ kind: "folder", cwd: "/Users/dev/wt" });
  });

  it("epic kind is sourceCapable but NOT diffCapable — folder reports, resolver never runs", () => {
    const { result } = renderHook(() => useTabSourceRepo(localEpic));
    expect(result.current).toEqual({
      folder: { kind: "folder", cwd: "/Users/dev/epic-dir" },
      repo: null,
    });
    expect(resolveLocalMock).not.toHaveBeenCalled();
    expect(resolveRemoteMock).not.toHaveBeenCalled();
  });
});

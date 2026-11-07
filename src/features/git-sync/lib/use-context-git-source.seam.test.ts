// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { ContextRepoStatus, ContextWorktreeInfo } from "@/tauri/commands";
import { resetContextGitStoreForTests, useContextGitStore } from "../store/use-context-git-store";
import { useContextGitSource } from "./use-context-git-source";

const cmd = vi.hoisted(() => ({
  getPublishSet: vi.fn(),
  contextRepoStatus: vi.fn(),
  listManagedWorktrees: vi.fn(),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  ...cmd,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("./use-org-root", () => ({ useOrgRoot: () => "/clone" }));
vi.mock("./scope-publish-set", async (original) => ({
  ...(await original<typeof import("./scope-publish-set")>()),
  useTeamContentPrefix: () => "team/content",
}));
vi.mock("@/shared/hooks/use-window-focused", () => ({ useWindowVisible: () => true }));

const TEAM = "acme/context:team";
const row = (id: string): ContextWorktreeInfo =>
  ({
    kind: "context",
    id,
    name: id,
    orgSlug: "acme/context",
    team: "team",
    path: `/${id}`,
  }) as ContextWorktreeInfo;
const status = (id?: string): ContextRepoStatus => ({
  branch: id ?? "main",
  headSha: "1234567",
  ahead: 0,
  behind: 0,
  hasUpstream: true,
  upstream: "main",
  remoteProvider: null,
  conflicts: [],
  sync: { state: "ok", lastSyncedAt: null },
  ...(id
    ? { worktree: { id, name: id, path: `/${id}`, mergeBase: "1234567", uncommittedPaths: [] } }
    : {}),
});
const snapshot = () => useContextGitStore.getState().byTeam[TEAM];
const choose = (id: string) =>
  act(() =>
    useViewerStore.getState().setNavLens(TEAM, {
      source: id === "clone" ? { kind: "clone" } : { kind: "worktree", id },
    }),
  );
const tick = () => act(() => vi.advanceTimersByTimeAsync(0));

beforeEach(() => {
  vi.useFakeTimers();
  resetContextGitStoreForTests();
  useViewerStore.setState({ navLens: {}, navCollapsed: {} });
  useWorktreeRosterStore.setState({ rows: [row("cw_a"), row("cw_b")], error: null });
  cmd.contextRepoStatus.mockReset().mockImplementation((_root, id) => Promise.resolve(status(id)));
  cmd.listManagedWorktrees.mockReset().mockResolvedValue({ rows: [] });
  cmd.getPublishSet.mockReset().mockImplementation((_root, _paths, _pr, _commit, _prefix, id) => {
    const files = [{ path: `team/content/${id ?? "clone"}.md`, status: "modified" }];
    return Promise.resolve({ general: { files, count: 1 }, buckets: [] });
  });
});
afterEach(() => vi.useRealTimers());

it("reads the explicitly selected source and restores clone changes when cleared", async () => {
  renderHook(() => useContextGitSource(TEAM, "/clone/team/content", true));
  await tick();
  expect(snapshot().changes[0]?.absPath).toBe("/clone/team/content/clone.md");
  choose("cw_a");
  await tick();
  expect(snapshot().changes[0]?.absPath).toBe("/cw_a/team/content/cw_a.md");
  expect(snapshot().status?.worktree?.id).toBe("cw_a");
  choose("clone");
  await tick();
  expect(snapshot().changes[0]?.absPath).toBe("/clone/team/content/clone.md");
  expect(snapshot().status?.worktree).toBeUndefined();
});

it("waits for a fresh roster before declaring an explicitly chosen source missing", async () => {
  let finish!: (value: { rows: ContextWorktreeInfo[] }) => void;
  cmd.listManagedWorktrees.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  renderHook(() => useContextGitSource(TEAM, "/clone/team/content", true));
  await tick();
  choose("cw_new");
  await tick();
  expect(useViewerStore.getState().navLens[TEAM]?.source).toEqual({
    kind: "worktree",
    id: "cw_new",
  });
  expect(snapshot().loading).toBe(true);
  await act(async () => finish({ rows: [row("cw_new")] }));
  await tick();
  expect(snapshot().status?.worktree?.id).toBe("cw_new");
  expect(snapshot().sourceNote).toBeNull();
});

it("keeps the choice on a failed roster read and falls back only after confirmed removal", async () => {
  cmd.listManagedWorktrees.mockRejectedValueOnce(new Error("temporarily unavailable"));
  renderHook(() => useContextGitSource(TEAM, "/clone/team/content", true));
  choose("cw_new");
  await tick();
  expect(useViewerStore.getState().navLens[TEAM]?.source).toEqual({
    kind: "worktree",
    id: "cw_new",
  });
  choose("cw_gone");
  await tick();
  expect(useViewerStore.getState().navLens[TEAM]?.source).toEqual({ kind: "clone" });
  expect(snapshot().sourceNote).toContain("cw_gone no longer exists; showing your clone.");
});

it("restores an unchanged status when returning before the other source read finishes", async () => {
  let finish!: (value: ContextRepoStatus) => void;
  cmd.contextRepoStatus.mockImplementation((_root, id) =>
    id === "cw_b"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve(status(id)),
  );
  choose("cw_a");
  renderHook(() => useContextGitSource(TEAM, "/clone/team/content", true));
  await tick();
  expect(snapshot().status?.worktree?.id).toBe("cw_a");
  choose("cw_b");
  await tick();
  expect(snapshot().status).toBeNull();
  choose("cw_a");
  await tick();
  expect(snapshot().status?.worktree?.id).toBe("cw_a");
  await act(async () => finish(status("cw_b")));
  expect(snapshot().status?.worktree?.id).toBe("cw_a");
});

it.each([
  ["a missing", () => cmd.getPublishSet.mockResolvedValue(undefined)],
  ["a rejected", () => cmd.getPublishSet.mockRejectedValue(new Error("unavailable"))],
])(
  "shows no changes instead of throwing when the publish set read is %s",
  async (_name, arrange) => {
    arrange();
    renderHook(() => useContextGitSource(TEAM, "/clone/team/content", true));
    await tick();
    expect(snapshot().changes).toEqual([]);
  },
);

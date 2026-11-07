// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import type { ContextWorktreeInfo } from "@/tauri/commands";

import { useFileRedline, type FileRedlineArgs } from "./use-file-redline";

// An open Changes tab follows its own file: the real file-change counter the watcher bumps,
// the real redline seam, and the command answered here.

const cmd = vi.hoisted(() => ({ getFileRedline: vi.fn() }));
const events = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    events.set(name, callback);
    return () => events.delete(name);
  }),
}));
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, ...cmd };
});

const NAV = "/checkout";
const X = "/org/default/content/x.md";
const Y = "/org/default/content/y.md";

const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));
const write = (...paths: string[]) =>
  act(() => useFileRefreshStore.getState().bumpFileRefresh(NAV, new Set(paths)));
const readsOf = (rel: string) =>
  cmd.getFileRedline.mock.calls.filter((c: unknown[]) => c[1] === rel).length;

let after = "v1";

function open(absPath: string, extra: Partial<FileRedlineArgs> = {}) {
  return renderHook((props: Partial<FileRedlineArgs>) =>
    useFileRedline({ orgRoot: "/org", absPath, navRepoPath: NAV, ...extra, ...props }),
  );
}

beforeEach(() => {
  events.clear();
  vi.useFakeTimers();
  after = "v1";
  useWorktreeRosterStore.setState({ rows: [] });
  cmd.getFileRedline
    .mockReset()
    .mockImplementation(() =>
      Promise.resolve({ status: "modified", binary: false, before: "v0", after }),
    );
  useFileRefreshStore.setState({
    fileRefreshKeys: {},
    lastChangedContextFile: {},
    contentRefreshKeys: {},
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("an open Changes tab", () => {
  it.each([{ kinds: ["head"] }, { kinds: ["refs", "index"] }])(
    "updates a private baseline on $kinds without rewriting the file",
    async ({ kinds }) => {
      const privateRoot = "/home/users/42/scratch";
      const path = `${privateRoot}/orgs/acme/plans/content/idea.md`;
      const hook = open(path, {
        orgRoot: privateRoot,
        navRepoPath: `${privateRoot}/orgs/acme/plans/content`,
      });
      await tick(0);
      expect(cmd.getFileRedline).toHaveBeenLastCalledWith(
        privateRoot,
        "orgs/acme/plans/content/idea.md",
        undefined,
        undefined,
        undefined,
      );
      after = "committed";
      act(() => events.get("context-git-moved")?.({ payload: { repoPath: privateRoot, kinds } }));
      await tick(0);
      expect(cmd.getFileRedline).toHaveBeenCalledTimes(2);
      expect(hook.result.current.read).toMatchObject({ redline: { after: "committed" } });
    },
  );
  it("re-reads when its own file changes, and the other tabs don't", async () => {
    const x = open(X);
    open(Y);
    await tick(0);
    expect(readsOf("default/content/x.md")).toBe(1);
    expect(readsOf("default/content/y.md")).toBe(1);

    after = "v2";
    for (let i = 0; i < 6; i++) {
      write(X);
      await tick(50);
    }
    await tick(400);
    expect(readsOf("default/content/x.md")).toBe(2);
    expect(readsOf("default/content/y.md")).toBe(1);
    expect(x.result.current.read).toMatchObject({ redline: { after: "v2" } });
  });

  it("a commit's version of a file is read once and never again", async () => {
    open(X, { commitSha: "abc123" });
    await tick(0);
    write(X);
    await tick(5_000);
    expect(readsOf("default/content/x.md")).toBe(1);
  });

  it.each([{ commitSha: "abc123" }, { prNumber: 42 }])(
    "keeps a fixed version of a worktree file ahead of its working-tree source (%j)",
    async (version) => {
      useWorktreeRosterStore.setState({
        rows: [{ kind: "context", id: "cw_sprint", path: "/worktree" } as ContextWorktreeInfo],
      });
      cmd.getFileRedline.mockImplementation((_root, _path, _old, pr, commit, worktree) =>
        Promise.resolve({
          status: "modified",
          binary: false,
          before: "parent",
          after: !worktree && (pr || commit) ? "selected version" : "working file",
        }),
      );
      const tab = open("/worktree/default/content/x.md", version);
      await tick(0);
      expect(cmd.getFileRedline).toHaveBeenCalledWith(
        "/worktree",
        "default/content/x.md",
        undefined,
        version.prNumber,
        version.commitSha,
      );
      expect(tab.result.current.read).toMatchObject({ redline: { after: "selected version" } });
      write("/worktree/default/content/x.md");
      await tick(5000);
      expect(cmd.getFileRedline).toHaveBeenCalledTimes(1);
    },
  );

  it.each([false, true])(
    "refreshes worktree labels and baseline even when bytes stay the same (binary=%s)",
    async (binary) => {
      useWorktreeRosterStore.setState({
        rows: [{ kind: "context", id: "cw_sprint", path: "/worktree" } as ContextWorktreeInfo],
      });
      const path = `/worktree/default/content/x.${binary ? "png" : "md"}`;
      const response = {
        status: "modified",
        binary,
        before: binary ? "" : "v0",
        after: binary ? "" : "v1",
        beforeLabel: "main · aaaaaaa",
        afterLabel: "Sprint · uncommitted",
        baselineSha: "aaaaaaa",
      };
      cmd.getFileRedline.mockResolvedValue(response);
      const tab = open(path);
      await tick(0);
      cmd.getFileRedline.mockResolvedValue({
        ...response,
        beforeLabel: "main · bbbbbbb",
        afterLabel: "Sprint · ccccccc",
        baselineSha: "bbbbbbb",
      });
      write(path);
      await tick(400);
      expect(tab.result.current.read).toMatchObject({
        redline: {
          beforeLabel: "main · bbbbbbb",
          afterLabel: "Sprint · ccccccc",
          baselineSha: "bbbbbbb",
        },
      });
    },
  );

  it("a tab not on screen waits, then reads once when shown", async () => {
    const x = open(X, { active: false });
    await tick(0);
    expect(readsOf("default/content/x.md")).toBe(1);
    write(X);
    await tick(5_000);
    expect(readsOf("default/content/x.md")).toBe(1);
    x.rerender({ active: true });
    await tick(0);
    expect(readsOf("default/content/x.md")).toBe(2);
  });

  it("an unchanged answer keeps the same result, so nothing re-renders", async () => {
    const x = open(X);
    await tick(0);
    const first = x.result.current.read;
    write(X);
    await tick(400);
    expect(readsOf("default/content/x.md")).toBe(2);
    expect(x.result.current.read).toBe(first);
  });

  it("a re-read that fails mid-write keeps the last good diff", async () => {
    const x = open(X);
    await tick(0);
    const good = x.result.current.read;
    expect(good).toMatchObject({ redline: { after: "v1" } });
    cmd.getFileRedline.mockImplementationOnce(() => Promise.reject(new Error("index.lock exists")));
    write(X);
    await tick(400);
    expect(readsOf("default/content/x.md")).toBe(2);
    expect(x.result.current.read).toBe(good);
  });

  it("a file that never loaded shows the error", async () => {
    cmd.getFileRedline.mockImplementationOnce(() => Promise.reject(new Error("no such path")));
    const x = open(X);
    await tick(0);
    expect(x.result.current.read).toEqual({ error: "no such path" });
  });
});

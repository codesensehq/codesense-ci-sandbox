// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";

import { EPIC_REPOS_CHANGED_EVENT } from "@/tauri/events";
import { useEpicReposReassembly } from "./use-epic-repos-reassembly";

// Controllable `listen` registry (mirrors use-dashboard-stream.test).
const eventMock = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    listeners,
    listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
      let set = listeners.get(event);
      if (!set) {
        set = new Set();
        listeners.set(event, set);
      }
      set.add(handler);
      return () => set!.delete(handler);
    }),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMock.listen }));

const {
  assembleEpicWorktrees,
  deriveEpicScopeFromPath,
  resolveEpicCommandContext,
  withErrorToast,
  warn,
} = vi.hoisted(() => ({
  assembleEpicWorktrees: vi.fn(() =>
    Promise.resolve({ assembled: [], failures: [], branch: "", devEnvRuns: [] }),
  ),
  deriveEpicScopeFromPath: vi.fn(),
  resolveEpicCommandContext: vi.fn(),
  withErrorToast: vi.fn(),
  warn: vi.fn(),
}));
vi.mock("@/tauri/commands", () => ({ assembleEpicWorktrees }));
vi.mock("@/features/viewer/lib/epic-workspace-opener", () => ({
  deriveEpicScopeFromPath,
  resolveEpicCommandContext,
}));
vi.mock("@/shared/lib/with-error-toast", () => ({ withErrorToast }));
vi.mock("@/shared/lib/logger", () => ({ logger: { warn } }));

function emit(payload: unknown) {
  for (const h of [...(eventMock.listeners.get(EPIC_REPOS_CHANGED_EVENT) ?? [])]) h({ payload });
}

beforeEach(() => {
  eventMock.listeners.clear();
  vi.clearAllMocks();
  deriveEpicScopeFromPath.mockReturnValue({ orgSlug: "acme", team: "default" });
  resolveEpicCommandContext.mockReturnValue({
    codesenseHome: "/home",
    githubUserId: 42,
    orgSlug: "acme",
    team: "default",
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useEpicReposReassembly", () => {
  it("re-assembles (fire-and-forget) when the epic context resolves", () => {
    renderHook(() => useEpicReposReassembly());
    emit({ epicFilePath: "/ctx/orgs/acme/repo/default/content/ep/README.md", repos: ["a/b"] });

    expect(assembleEpicWorktrees).toHaveBeenCalledTimes(1);
    expect(assembleEpicWorktrees).toHaveBeenCalledWith({
      codesenseHome: "/home",
      githubUserId: 42,
      orgSlug: "acme",
      team: "default",
      epicFilePath: "/ctx/orgs/acme/repo/default/content/ep/README.md",
    });
    expect(withErrorToast).toHaveBeenCalledTimes(1);
    expect(warn).not.toHaveBeenCalled();
  });

  it("logs-and-skips (no assemble, no throw) when context is unresolved", () => {
    resolveEpicCommandContext.mockReturnValue(null);
    renderHook(() => useEpicReposReassembly());
    emit({ epicFilePath: "/nope/ep.md", repos: [] });

    expect(assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it("ignores a payload with no epicFilePath", () => {
    renderHook(() => useEpicReposReassembly());
    emit({ repos: ["a/b"] });

    expect(assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });

  it("coalesces a burst: same epic in flight assembles once", () => {
    renderHook(() => useEpicReposReassembly());
    // Two rapid events for the same epic (e.g. an agent's clear-then-set); the
    // second must not race a second git-worktree build while the first is live.
    const path = "/root/default/content/ep2/README.md";
    emit({ epicFilePath: path, repos: ["a/b"] });
    emit({ epicFilePath: path, repos: ["a/b", "c/d"] });

    expect(assembleEpicWorktrees).toHaveBeenCalledTimes(1);
  });
});

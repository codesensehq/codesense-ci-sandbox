// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  resolveTeamTargetContentRoot,
  useTargetFacts,
} from "@/features/context-mobility/context-target-facts";
import {
  personalContextId,
  teamAllContextId,
  teamRepoContextId,
  viewOnlyRepoContextId,
  type Context,
} from "@/shared/lib/context-model";
import type { Org } from "@/shared/types/domain";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";

import { resetContentFileIndexForTests } from "@/shared/lib/pipeline-graph/content-scan-queue";

const listContextFiles = vi.fn();
const toastError = vi.fn();
vi.mock("@/shared/lib/with-error-toast", () => ({
  toastError: (...a: unknown[]) => toastError(...a),
}));
// The index-changed event: tests publish a generation through `generation.emit`.
const generation = vi.hoisted(() => {
  const listeners: ((event: { payload: { generation: number } }) => void)[] = [];
  return {
    listeners,
    emit: (n: number) => listeners.forEach((l) => l({ payload: { generation: n } })),
  };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (_name: string, cb: (event: { payload: { generation: number } }) => void) => {
    generation.listeners.push(cb);
    return () => {};
  },
}));
vi.mock("@/tauri/commands", () => ({
  listContextFiles: (...a: unknown[]) => listContextFiles(...(a as [string])),
}));

const orgAcme: Org = {
  slug: "acme",
  display_name: "Acme",
  created_at: "2026-01-01T00:00:00Z",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [{ name: "default", source_repos: [{ owner: "acme", repo: "api" }] }],
  org_root: "/mock/acme",
};
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: (sel: (s: unknown) => unknown) => sel({ orgs: [orgAcme] }),
}));

const teamAll: Context = {
  id: teamAllContextId("acme", "default"),
  kind: "team-all",
  sharing: "shared",
  label: "All of default",
  teamName: "default",
};
const teamRepo: Context = {
  id: teamRepoContextId("acme", "default", { owner: "acme", repo: "api" }),
  kind: "team-repo",
  sharing: "shared",
  label: "default · api",
  teamName: "default",
  repoSlug: { owner: "acme", repo: "api" },
};
const personal: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Personal",
};

describe("resolveTeamTargetContentRoot — the target's content dir (SLW-PT-B3)", () => {
  it("resolves a team-all target to the team CONTENT dir (what the backend keys on)", () => {
    expect(resolveTeamTargetContentRoot(teamAll, [orgAcme])).toBe(
      pathToTeamContentDirSync("/mock/acme", "default"),
    );
  });

  it("resolves a team-repo target to the SAME team content dir (per-repo is a subtree)", () => {
    expect(resolveTeamTargetContentRoot(teamRepo, [orgAcme])).toBe(
      pathToTeamContentDirSync("/mock/acme", "default"),
    );
  });

  it("returns null for Personal + view-only (no synchronous content root)", () => {
    expect(resolveTeamTargetContentRoot(personal, [orgAcme])).toBeNull();
    const viewOnly: Context = {
      id: viewOnlyRepoContextId({ owner: "x", repo: "y" }),
      kind: "view-only-repo",
      sharing: "shared",
      label: "y",
      repoSlug: { owner: "x", repo: "y" },
    };
    expect(resolveTeamTargetContentRoot(viewOnly, [orgAcme])).toBeNull();
  });

  it("returns null when the org isn't loaded", () => {
    expect(resolveTeamTargetContentRoot(teamAll, [])).toBeNull();
  });
});

describe("useTargetFacts — lazy collision facts (SV-PBF-267)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetContentFileIndexForTests();
    generation.listeners.length = 0;
    listContextFiles.mockResolvedValue([
      { name: "plan.md", relativePath: "plan.md", fullPath: "/mock/acme/default/content/plan.md" },
    ]);
  });

  it("lists nothing on mount", () => {
    renderHook(() => useTargetFacts([teamAll, teamRepo]));
    expect(listContextFiles).not.toHaveBeenCalled();
  });

  it("prefetch lists each distinct root once (a team and its repo share one content root)", async () => {
    const { result } = renderHook(() => useTargetFacts([teamAll, teamRepo]));
    act(() => result.current.prefetch());
    act(() => result.current.prefetch());
    await waitFor(() => expect(result.current.targetCollision(teamAll, "plan.md")).toBe(true));
    expect(listContextFiles).toHaveBeenCalledTimes(1);
    expect(listContextFiles).toHaveBeenCalledWith(
      pathToTeamContentDirSync("/mock/acme", "default"),
    );
    expect(result.current.targetHas(teamRepo, "plan.md")).toBe(true);
    expect(result.current.targetHas(teamAll, "plan-2.md")).toBe(false);
  });

  it("lists a root when a predicate asks about it, even without a prefetch", async () => {
    const { result } = renderHook(() => useTargetFacts([teamAll]));
    expect(result.current.targetCollision(teamAll, "plan.md")).toBe(false);
    await waitFor(() => expect(result.current.targetCollision(teamAll, "plan.md")).toBe(true));
  });

  it("degrades to no-collision for a Personal target (no content root here)", () => {
    const { result } = renderHook(() => useTargetFacts([personal]));
    act(() => result.current.prefetch());
    expect(result.current.targetCollision(personal, "plan.md")).toBe(false);
    expect(listContextFiles).not.toHaveBeenCalled();
  });

  it("lists nothing for a hidden (inactive) host", () => {
    const { result } = renderHook(() => useTargetFacts([teamAll], false));
    act(() => result.current.prefetch());
    expect(listContextFiles).not.toHaveBeenCalled();
  });

  it("re-lists on a newer index generation, and only when asked again", async () => {
    const { result } = renderHook(() => useTargetFacts([teamAll]));
    act(() => result.current.prefetch());
    await waitFor(() => expect(result.current.targetCollision(teamAll, "plan.md")).toBe(true));
    listContextFiles.mockResolvedValue([
      { name: "new.md", relativePath: "new.md", fullPath: "/mock/acme/default/content/new.md" },
    ]);
    await act(async () => generation.emit(2));
    await waitFor(() => expect(result.current.targetHas(teamAll, "new.md")).toBe(true));
    expect(result.current.targetHas(teamAll, "plan.md")).toBe(false);
    expect(listContextFiles).toHaveBeenCalledTimes(2);
  });

  it("does nothing on an index change when no picker was ever opened", () => {
    renderHook(() => useTargetFacts([teamAll]));
    generation.emit(5);
    expect(listContextFiles).not.toHaveBeenCalled();
  });

  it("tells the user when the backend refuses a root that is not indexed", async () => {
    listContextFiles.mockRejectedValue(new Error("not indexed: /mock/acme/default/content"));
    const { result } = renderHook(() => useTargetFacts([teamAll]));
    act(() => result.current.prefetch());
    await waitFor(() => expect(toastError).toHaveBeenCalledTimes(1));
    expect(toastError.mock.calls[0][0]).toMatch(/name clashes/);
    expect(String(toastError.mock.calls[0][1])).toMatch(/isn't part of your synced context/);
  });
});

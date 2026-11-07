// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

// The extracted unit reads org/team context via imperative snapshots and runs the
// shared `openEpicTerminal` orchestration. Mock those boundaries so the test
// exercises the resolver + the call into `openEpicTerminal` without touching the
// real stores (the deps closures are lazy — never invoked while openEpicTerminal
// is a no-op spy). Spies are declared via `vi.hoisted` so the hoisted `vi.mock`
// factories can reference them.
const { openEpicTerminal, getCurrentUser, orgsRef, toastError } = vi.hoisted(() => ({
  openEpicTerminal: vi.fn().mockResolvedValue(undefined),
  getCurrentUser: vi.fn(),
  orgsRef: { orgs: [] as Array<{ slug: string; org_root: string }> },
  toastError: vi.fn(),
}));
vi.mock("./open-epic-terminal", () => ({ openEpicTerminal }));
vi.mock("@/shared/lib/get-current-user", () => ({ getCurrentUser: () => getCurrentUser() }));
vi.mock("@/shared/stores/org-store", () => ({ useOrgStore: { getState: () => orgsRef } }));
vi.mock("@/shared/lib/codesense-home", () => ({
  getCodesenseHome: () => "/root/.codesense",
}));
vi.mock("sonner", () => ({ toast: { error: (m: string) => toastError(m) } }));

// The tauri commands + stores that `wireEpicTerminalDeps` closes over — never
// called in these tests (openEpicTerminal is stubbed), but imported at module load.
vi.mock("@/tauri/commands", () => ({
  ensureEpicWorkspace: vi.fn(),
  assembleEpicWorktrees: vi.fn(),
}));
vi.mock("@/shared/stores/viewer-store", () => ({ useViewerStore: { getState: () => ({}) } }));
vi.mock("@/features/shell/stores/surface-sync-store", () => ({
  useSurfaceSyncStore: { getState: () => ({}) },
}));
vi.mock("@/features/shell/stores/epic-assembly-store", () => ({
  useEpicAssemblyStore: { getState: () => ({}) },
  formatEpicFailureSummary: vi.fn(),
}));
vi.mock("@/features/shell/hooks/use-epic-assembly-listener", () => ({ syncEpicChipEcho: vi.fn() }));

import {
  resolveEpicCommandContext,
  deriveEpicScopeFromPath,
  openEpicWorkspaceFor,
  type EpicOpenTarget,
} from "./epic-workspace-opener";

const TARGET: EpicOpenTarget = {
  teamKey: "acme:default",
  orgSlug: "acme",
  team: "default",
  epicDirName: "SW-EW",
  epicFilePath: "/ctx/epic-workspaces/SW-EW.md",
};

beforeEach(() => {
  vi.clearAllMocks();
  orgsRef.orgs = [{ slug: "acme", org_root: "/root" }];
  getCurrentUser.mockReturnValue({ github_user_id: 42 });
});

describe("resolveEpicCommandContext", () => {
  it("returns the shared context when org + user resolve", () => {
    expect(resolveEpicCommandContext({ orgSlug: "acme", team: "default" })).toEqual({
      codesenseHome: "/root/.codesense",
      githubUserId: 42,
      orgSlug: "acme",
      team: "default",
    });
  });

  it("returns null when the user is unauthenticated", () => {
    getCurrentUser.mockReturnValue(null);
    expect(resolveEpicCommandContext({ orgSlug: "acme", team: "default" })).toBeNull();
  });

  it("returns null when the org is not found", () => {
    orgsRef.orgs = [];
    expect(resolveEpicCommandContext({ orgSlug: "acme", team: "default" })).toBeNull();
  });

  it("returns null for the sentinel `_none` team", () => {
    expect(resolveEpicCommandContext({ orgSlug: "acme", team: "_none" })).toBeNull();
  });
});

describe("openEpicWorkspaceFor", () => {
  it("runs openEpicTerminal with {...ctx, epicFilePath} + a full deps object", async () => {
    await openEpicWorkspaceFor(TARGET);
    expect(openEpicTerminal).toHaveBeenCalledTimes(1);
    const [base, deps] = openEpicTerminal.mock.calls[0];
    expect(base).toEqual({
      codesenseHome: "/root/.codesense",
      githubUserId: 42,
      orgSlug: "acme",
      team: "default",
      epicFilePath: TARGET.epicFilePath,
    });
    // The 15-callback deps object is fully wired.
    expect(typeof deps.openIntegration).toBe("function");
    expect(typeof deps.armEpicOpen).toBe("function");
    expect(deps.inFlight).toBeInstanceOf(Set);
    expect(toastError).not.toHaveBeenCalled();
  });

  it("honest-degrades (toast, no spawn) when the context can't resolve", async () => {
    getCurrentUser.mockReturnValue(null);
    await openEpicWorkspaceFor(TARGET);
    expect(openEpicTerminal).not.toHaveBeenCalled();
    expect(toastError).toHaveBeenCalledWith(
      "Couldn't open epic terminal: missing org/team context.",
    );
  });
});

describe("deriveEpicScopeFromPath", () => {
  it("resolves {orgSlug, team}: canonical parser for team, org store for slug", () => {
    // orgsRef seeds `{ slug: "acme", org_root: "/root" }`; the path's org_root
    // (`/root`) matches it, and `default` is the team before `content`.
    expect(deriveEpicScopeFromPath("/root/default/content/_bmad-output/ep/README.md")).toEqual({
      orgSlug: "acme",
      team: "default",
    });
  });

  it("returns null when the path's org_root is not a known org", () => {
    expect(deriveEpicScopeFromPath("/other/default/content/ep/README.md")).toBeNull();
  });

  it("returns null for a non-canonical path (no content-layer marker)", () => {
    expect(deriveEpicScopeFromPath("/some/random/path/ep.md")).toBeNull();
  });

  it("tolerates a trailing slash divergence between store org_root and the path", () => {
    orgsRef.orgs = [{ slug: "acme", org_root: "/root/" }];
    expect(deriveEpicScopeFromPath("/root/default/content/ep/README.md")).toEqual({
      orgSlug: "acme",
      team: "default",
    });
  });
});

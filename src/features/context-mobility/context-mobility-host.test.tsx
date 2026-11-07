// SPDX-License-Identifier: MIT
import { resetContentFileIndexForTests } from "@/shared/lib/pipeline-graph/content-scan-queue";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useContextMoveRowSlot } from "@/features/context-mobility/context-mobility-host";
import { personalContextId, teamAllContextId, teamRepoContextId } from "@/shared/lib/context-model";
import type { SourceRepoSlug } from "@/shared/types/domain";
import type { Org } from "@/shared/types/domain";

// The gate sheet + backend are stubbed — this suite is about the WIRING (the ⋯
// action mounts on a content row with the right source context), not the merged
// components' internals.
const moveContentBetweenContexts = vi.fn().mockResolvedValue({ kind: "moved" });
// SLW-PT-B3: the target-facts pre-listing. Default: the target already holds
// 'plan.md' so the §P collision gate can fire from the slot.
const listContextFiles = vi.fn(async (_rootPath: string) => [
  { name: "plan.md", relativePath: "plan.md", fullPath: "/mock/acme/default/content/plan.md" },
]);
vi.mock("@/tauri/commands", () => ({
  moveContentBetweenContexts: (...a: unknown[]) => moveContentBetweenContexts(...(a as [])),
  removeContentFromContext: vi.fn().mockResolvedValue({ kind: "removed" }),
  listContextFiles: (...a: unknown[]) => listContextFiles(...(a as [string])),
}));

// SLW-PT-B3: the file-refresh store the onMoved wiring bumps.
const bumpFileRefresh = vi.fn();
const fileRefreshState = { fileRefreshKeys: {} as Record<string, number>, bumpFileRefresh };
vi.mock("@/shared/stores/file-refresh-store", () => ({
  useFileRefreshStore: Object.assign(
    (sel: (s: typeof fileRefreshState) => unknown) => sel(fileRefreshState),
    { getState: () => fileRefreshState },
  ),
}));

// ---- host dependency mocks -------------------------------------------------
// SW-XS-15: the row's source is the team's own content ("All of <Team>") for the
// view's team key — the nav always browses team content.
const harness = { teamKey: "acme:default" };

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

// Mutable org-store snapshot so a test can swap in a multi-team org (SLW-PT-B6
// exercises cross-team targets, which a single-team fixture can't reach).
const orgStoreState = {
  orgs: [orgAcme] as Org[],
  activeOrgSlug: "acme" as string | null,
};
const setActiveOrg = vi.fn();
const setManageTeamsIntent = vi.fn();
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: (sel: (s: unknown) => unknown) =>
    sel({ orgs: orgStoreState.orgs, activeOrgSlug: orgStoreState.activeOrgSlug, setActiveOrg }),
}));
vi.mock("@/shared/stores/use-active-org", () => ({
  useActiveOrg: () => orgStoreState.orgs[0] ?? null,
}));
vi.mock("@/shared/stores/ui-store", () => ({
  useUiStore: (sel: (s: unknown) => unknown) => sel({ setManageTeamsIntent }),
}));

vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ github_user_id: 7 }),
}));

const TEAM_KEY = "acme:default";
const ACME_API: SourceRepoSlug = { owner: "acme", repo: "api" };

function RowHarness({ refreshRepoPath }: { refreshRepoPath?: string } = {}) {
  const slot = useContextMoveRowSlot(harness.teamKey, refreshRepoPath);
  return <div data-testid="row">{slot("/abs/acme/default/plan.md", "plan.md")}</div>;
}

beforeEach(() => {
  resetContentFileIndexForTests();
  vi.clearAllMocks();
  fileRefreshState.fileRefreshKeys = {};
  orgStoreState.orgs = [orgAcme];
  orgStoreState.activeOrgSlug = "acme";
  listContextFiles.mockResolvedValue([
    { name: "plan.md", relativePath: "plan.md", fullPath: "/mock/acme/default/content/plan.md" },
  ]);
  harness.teamKey = TEAM_KEY;
  // The host lists targets only while its team is the one on screen.
  warmScopes.activate(TEAM_KEY);
});

describe("useContextMoveRowSlot — the per-file ⋯ action, live-wired", () => {
  it("mounts the ⋯ action on a content row and opens the picker with the active pivot as source", () => {
    render(<RowHarness />);
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    // The picker opens targeting the file, with the pivot's team-all as source
    // (so the source's own bucket is NOT offered as a target).
    expect(screen.getByTestId("move-target-sheet")).toBeInTheDocument();
    expect(screen.getByTestId("move-target-header")).toHaveTextContent("Move 'plan.md' to…");
    expect(
      screen.queryByTestId(`move-target-option-${teamAllContextId("acme", "default")}`),
    ).toBeNull();
    // The scratch space IS offered as a target (a different context from the source).
    expect(screen.getByTestId(`move-target-option-${personalContextId()}`)).toBeInTheDocument();
  });

  // SLW-PT-B3 (companion): the host now supplies target collision facts, so
  // choosing a target that already holds the doc drives the §P collision gate +
  // keep-both step from the REAL slot. Before the fix these were undefined and
  // the gate could never fire.
  it("drives the collision keep-both gate when the chosen target already holds the doc", async () => {
    render(<RowHarness refreshRepoPath="/nav/repo" />);
    // Mounting lists nothing; opening the ⋯ menu lists the target (the team content
    // dir holds 'plan.md').
    expect(listContextFiles).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    await waitFor(() =>
      expect(listContextFiles).toHaveBeenCalledWith("/mock/acme/default/content"),
    );
    fireEvent.click(screen.getByTestId("move-action-move"));
    // Pick the same team's per-repo target (shares the team content dir → the
    // doc collides there).
    fireEvent.click(
      screen.getByTestId(`move-target-option-${teamRepoContextId("acme", "default", ACME_API)}`),
    );
    // The collision gate + its keep-both choice appear (proof targetCollision is
    // wired through the slot).
    expect(await screen.findByTestId("move-gate-collision")).toBeInTheDocument();
    expect(screen.getByTestId("move-gate-collision-keep-both")).toBeInTheDocument();
  });

  // SLW-PT-B3 (companion): a completed move bumps the file-refresh store so the
  // moved doc updates in place (the host passed no onMoved before).
  it("bumps the nav file-refresh after a completed move (onMoved wiring)", async () => {
    render(<RowHarness refreshRepoPath="/nav/repo" />);
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    // Move to the scratch space (no collision → a clean move that runs the backend).
    fireEvent.click(screen.getByTestId(`move-target-option-${personalContextId()}`));
    // shared→scratch: no privacy/sole-copy/collision gate, so
    // the sheet executes the move immediately and calls onMoved.
    await waitFor(() => expect(moveContentBetweenContexts).toHaveBeenCalled());
    await waitFor(() => expect(bumpFileRefresh).toHaveBeenCalledWith("/nav/repo"));
  });
});

// SLW-PT-B6: the collision facts must cover the picker's CROSS-TEAM targets, not
// just the active team's own contexts. The host used to pre-list
// the active team's own contexts only (active-team scoped), so a sibling team the
// picker offered was never listed → `targetCollision` read false → the §P
// Keep-both/Replace/Cancel chooser was silently skipped (overwrite risk). These
// drive the REAL host→picker→controller→facts wiring: a cross-team target that
// already holds the doc must surface the collision gate; one that doesn't must
// not; and the active-team path must not regress.
describe("useContextMoveRowSlot — cross-team collision facts (SLW-PT-B6)", () => {
  // One org, three teams. team-a is the source; team-b already holds 'plan.md';
  // team-c is empty. team-b/team-c are siblings the picker offers but the OLD
  // active-team-scoped pre-listing never covered.
  const orgMultiTeam: Org = {
    ...orgAcme,
    teams: [
      { name: "team-a", source_repos: [] },
      { name: "team-b", source_repos: [] },
      { name: "team-c", source_repos: [] },
    ],
  };

  function selectTeamA() {
    harness.teamKey = "acme:team-a";
    warmScopes.activate("acme:team-a");
  }

  beforeEach(() => {
    orgStoreState.orgs = [orgMultiTeam];
    selectTeamA();
    // 'plan.md' lives ONLY at team-b's content dir; team-a/team-c are empty.
    listContextFiles.mockImplementation(async (root: string) =>
      root === "/mock/acme/team-b/content"
        ? [{ name: "plan.md", relativePath: "plan.md", fullPath: `${root}/plan.md` }]
        : [],
    );
  });

  it("surfaces the collision gate for a CROSS-TEAM target that already holds the doc", async () => {
    render(<RowHarness refreshRepoPath="/nav/repo" />);
    // Opening the menu lists the sibling team's content root (the bug: it never was).
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    await waitFor(() => expect(listContextFiles).toHaveBeenCalledWith("/mock/acme/team-b/content"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    // Pick "All of team-b" — a different team that already holds 'plan.md'.
    fireEvent.click(screen.getByTestId(`move-target-option-${teamAllContextId("acme", "team-b")}`));
    // The §P chooser fires before any disk change (proof the cross-team fact resolved).
    expect(await screen.findByTestId("move-gate-collision")).toBeInTheDocument();
    expect(screen.getByTestId("move-gate-collision-keep-both")).toBeInTheDocument();
    // Keep-both lands a renamed copy rather than overwriting.
    fireEvent.click(screen.getByTestId("move-gate-collision-keep-both"));
    await waitFor(() => expect(moveContentBetweenContexts).toHaveBeenCalled());
    expect(moveContentBetweenContexts.mock.calls[0][0]).toMatchObject({
      toLogicalName: expect.stringMatching(/plan/),
    });
  });

  it("does NOT surface the collision gate for a non-colliding CROSS-TEAM target", async () => {
    render(<RowHarness refreshRepoPath="/nav/repo" />);
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    await waitFor(() => expect(listContextFiles).toHaveBeenCalledWith("/mock/acme/team-c/content"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    // "All of team-c" is empty → a clean shared→shared move, no chooser.
    fireEvent.click(screen.getByTestId(`move-target-option-${teamAllContextId("acme", "team-c")}`));
    await waitFor(() => expect(moveContentBetweenContexts).toHaveBeenCalled());
    expect(screen.queryByTestId("move-gate-collision")).toBeNull();
    // A clean move keeps the original name (no keep-both rename).
    expect(moveContentBetweenContexts.mock.calls[0][0].toLogicalName).toBeUndefined();
  });
});

// SV-PBF-267 PR2-AC4: a mounted team view lists nothing; a picker lists each distinct
// root once; an fs bump without a picker lists nothing.
describe("useContextMoveRowSlot — lazy target listing (SV-PBF-267)", () => {
  const manyRepos: SourceRepoSlug[] = [
    { owner: "acme", repo: "api" },
    { owner: "acme", repo: "web" },
    { owner: "acme", repo: "docs" },
  ];
  const tenTeams: Org = {
    ...orgAcme,
    teams: Array.from({ length: 10 }, (_, i) => ({
      name: `team-${i}`,
      // The first team carries 3 source repos: they share ONE content root.
      source_repos: i === 0 ? manyRepos : [],
    })),
  };

  beforeEach(() => {
    orgStoreState.orgs = [tenTeams];
    harness.teamKey = "acme:team-0";
    warmScopes.activate("acme:team-0");
    listContextFiles.mockResolvedValue([]);
  });

  it("lists 0 roots on mount and on an fs bump, then 1 per distinct root once the picker opens", async () => {
    const { rerender } = render(<RowHarness refreshRepoPath="/nav/repo" />);
    expect(listContextFiles).not.toHaveBeenCalled();
    // An fs bump with no picker open lists nothing.
    fileRefreshState.fileRefreshKeys = { "/nav/repo": 1 };
    rerender(<RowHarness refreshRepoPath="/nav/repo" />);
    expect(listContextFiles).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    await waitFor(() => expect(listContextFiles).toHaveBeenCalledTimes(10));
    const roots = listContextFiles.mock.calls.map((call) => call[0]);
    expect(new Set(roots).size).toBe(10);
    // Opening the picker again lists nothing new (same index generation).
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    expect(listContextFiles).toHaveBeenCalledTimes(10);
  });
});

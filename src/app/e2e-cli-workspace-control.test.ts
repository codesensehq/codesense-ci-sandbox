// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { isMountedTier, warmScopes } from "@/shared/lib/warm-scopes";
import { teamHasSatisfiedClone } from "@/features/attachment/lib/team-clone-gate";
import {
  resolveMainTreeSurfaces,
  teamPaneVisible,
} from "@/features/home-base/lib/home-base-routing";
import { syntheticRepoKey, workspaceReposForTeam } from "@/shared/lib/build-synthetic-repo";
import {
  createCliWorkspaceFixturePlan,
  createFixtureForegroundLayout,
} from "./e2e-cli-workspace-control";

describe("WebDriver CLI-workspace fixture control", () => {
  it("creates stable isolated stories across two epics with no sensitive payload", () => {
    const plan = createCliWorkspaceFixturePlan({
      root: "/isolated/qh1/cli",
      cwdA: "/isolated/qh1/cli/a",
      cwdB: "/isolated/qh1/cli/b",
      cwdC: "/isolated/qh1/cli/c",
    });
    expect(plan.teamKey).toBe("qh1:cli");
    expect(plan.workspaces.map(({ id, parentEpic, cwd }) => ({ id, parentEpic, cwd }))).toEqual([
      { id: "qh1-story-a", parentEpic: "qh1-epic-a", cwd: "/isolated/qh1/cli/a" },
      { id: "qh1-story-b", parentEpic: "qh1-epic-a", cwd: "/isolated/qh1/cli/b" },
      { id: "qh1-story-c", parentEpic: "qh1-epic-b", cwd: "/isolated/qh1/cli/c" },
    ]);
    expect(JSON.stringify(plan)).not.toMatch(/command|output|credential|token/i);
  });

  it("satisfies every App route gate after canonical team entry", () => {
    const plan = createCliWorkspaceFixturePlan({
      root: "/isolated/qh1/cli",
      cwdA: "/isolated/qh1/cli/a",
      cwdB: "/isolated/qh1/cli/b",
      cwdC: "/isolated/qh1/cli/c",
    });
    const repoPath = syntheticRepoKey(plan.orgSlug, plan.teamName, plan.sourceRepo);
    const workspaceRepos = {
      [repoPath]: {
        repoPath,
        repoName: "cli",
        contextRepoPath: plan.root,
        teamContentPath: plan.root,
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
        localPath: plan.root,
      },
    };
    const activeTeamHasClone = teamHasSatisfiedClone(
      workspaceReposForTeam(workspaceRepos, plan.teamKey),
    );

    expect(activeTeamHasClone).toBe(true);
    warmScopes.activate(plan.teamKey);
    expect(isMountedTier(warmScopes.tierOf(plan.teamKey))).toBe(true);
    warmScopes.forget(plan.teamKey);
    expect(
      resolveMainTreeSurfaces({
        openedTeamsCount: 1,
        activeTeamName: plan.teamName,
        activeTeamOpened: true,
      }),
    ).toEqual({ showTeamEmptyState: false });
    expect(
      teamPaneVisible({
        settingsPanelOpen: false,
        isActiveTeam: true,
      }),
    ).toBe(true);
  });

  it("materializes one deterministic foreground mosaic leaf per fixture workspace", () => {
    const ids = ["qh1-story-a", "qh1-story-b", "qh1-story-c"];
    const layout = createFixtureForegroundLayout(ids);
    const leaves = (node: NonNullable<typeof layout.root>): string[] =>
      node.kind === "leaf" ? [node.workspaceId] : node.children.flatMap(leaves);

    expect(layout).toMatchObject({ preset: "columns", direction: "row" });
    expect(layout.root).not.toBeNull();
    expect(leaves(layout.root!)).toEqual(ids);
  });
});

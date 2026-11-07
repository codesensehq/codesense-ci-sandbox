// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { runPipelineScopeRecovery } from "./run-scope-recovery";
import type { RepoState } from "@/shared/stores/config-store";

// T.4 Phase 5: scope recovery is team-keyed. Repos use synthetic
// `workspace:org:team:owner/repo` keys so `parseSyntheticRepoKey` can
// derive the owning team. teamKey = `${orgSlug}:${teamName}`.
const R1 = "workspace:acme:default:acme/api";
const TEAM_DEFAULT = "acme:default";
const R2 = "workspace:acme:eng:acme/web";
const TEAM_ENG = "acme:eng";

function repo(overrides: Partial<RepoState> = {}): RepoState {
  return {
    repoPath: R1,
    repoName: "repo-one",
    contextRepoPath: "/ctx/r1",
    // PTS-2: teamContentPath required (recoverScopeForRepo early-returns on
    // empty). Use a per-repo content dir so tests that touch multiple
    // repos can disambiguate.
    teamContentPath: "/org/_team/default/content",
    hasRemote: false,
    remoteUrl: null,
    mainDoc: "",
    ...overrides,
  };
}

describe("runPipelineScopeRecovery", () => {
  it("is silent and applies no change when every scope is still valid", async () => {
    const findScope = vi.fn().mockResolvedValue("in-progress");
    const setPipelineScope = vi.fn();
    const notify = vi.fn();

    const result = await runPipelineScopeRecovery({
      repos: [repo()],
      pipelineScope: { [TEAM_DEFAULT]: "in-progress" },
      setPipelineScope,
      findScope,
      notify,
    });

    expect(result.recovered).toEqual([]);
    expect(setPipelineScope).not.toHaveBeenCalled();
    expect(notify).not.toHaveBeenCalled();
  });

  it("recovers one team and fires a specific toast naming the team", async () => {
    const findScope = vi.fn().mockResolvedValue("in-progress");
    const setPipelineScope = vi.fn();
    const notify = vi.fn();

    const result = await runPipelineScopeRecovery({
      repos: [repo()],
      pipelineScope: { [TEAM_DEFAULT]: "in-progress/deleted-dir" },
      setPipelineScope,
      findScope,
      notify,
    });

    expect(result.recovered).toEqual([
      { teamKey: TEAM_DEFAULT, from: "in-progress/deleted-dir", to: "in-progress" },
    ]);
    expect(setPipelineScope).toHaveBeenCalledWith(TEAM_DEFAULT, "in-progress");
    expect(notify).toHaveBeenCalledTimes(1);
    // T.4 Phase 8 review (Patch 9): toast names the TEAM (not an arbitrary
    // member repo) — `parseTeamKey("acme:default").teamName === "default"`.
    expect(notify.mock.calls[0][0]).toContain("default");
    expect(notify.mock.calls[0][0]).toContain("in-progress");
  });

  it("coalesces multi-team recoveries into one toast", async () => {
    const findScope = vi.fn().mockImplementation(async (_teamContent, _pipeRoot, s) => {
      if (s === "x/y") return "x";
      if (s === "a/b/c") return "";
      return s;
    });
    const setPipelineScope = vi.fn();
    const notify = vi.fn();

    const result = await runPipelineScopeRecovery({
      repos: [
        repo({ repoPath: R1, repoName: "one" }),
        repo({
          repoPath: R2,
          repoName: "two",
          contextRepoPath: "/ctx/r2",
          teamContentPath: "/org/_team/eng/content",
        }),
      ],
      pipelineScope: { [TEAM_DEFAULT]: "x/y", [TEAM_ENG]: "a/b/c" },
      setPipelineScope,
      findScope,
      notify,
    });

    expect(result.recovered).toHaveLength(2);
    expect(setPipelineScope).toHaveBeenCalledTimes(2);
    expect(notify).toHaveBeenCalledTimes(1);
    expect(notify.mock.calls[0][0]).toMatch(/2 teams/);
  });

  it("skips teams with empty scope without calling the scanner", async () => {
    const findScope = vi.fn().mockResolvedValue("");
    const setPipelineScope = vi.fn();

    await runPipelineScopeRecovery({
      repos: [repo()],
      pipelineScope: { [TEAM_DEFAULT]: "" },
      setPipelineScope,
      findScope,
    });

    expect(findScope).not.toHaveBeenCalled();
    expect(setPipelineScope).not.toHaveBeenCalled();
  });

  it("skips teams missing from config without aborting others", async () => {
    const findScope = vi.fn().mockResolvedValue("");
    const setPipelineScope = vi.fn();
    const notify = vi.fn();

    await runPipelineScopeRecovery({
      repos: [repo()],
      // "ghost:team" has no matching repo in `repos` → skipped.
      pipelineScope: { "ghost:team": "a", [TEAM_DEFAULT]: "x" },
      setPipelineScope,
      findScope,
      notify,
    });

    expect(findScope).toHaveBeenCalledTimes(1);
    // PTS-2: recoverScopeForRepo passes (teamContentPath, "", scopePath).
    expect(findScope).toHaveBeenCalledWith("/org/_team/default/content", "", "x");
  });

  it("logs and continues when findNearestValidScope rejects for one team", async () => {
    const findScope = vi.fn().mockRejectedValueOnce(new Error("boom")).mockResolvedValueOnce("a");
    const setPipelineScope = vi.fn();
    const notify = vi.fn();

    const result = await runPipelineScopeRecovery({
      repos: [
        repo({ repoPath: R1, repoName: "one" }),
        repo({
          repoPath: R2,
          repoName: "two",
          contextRepoPath: "/ctx/r2",
          teamContentPath: "/org/_team/eng/content",
        }),
      ],
      pipelineScope: { [TEAM_DEFAULT]: "broken", [TEAM_ENG]: "a/b" },
      setPipelineScope,
      findScope,
      notify,
    });

    expect(result.recovered).toHaveLength(1);
    expect(result.recovered[0].teamKey).toBe(TEAM_ENG);
    expect(setPipelineScope).toHaveBeenCalledTimes(1);
    expect(setPipelineScope).toHaveBeenCalledWith(TEAM_ENG, "a");
  });
});

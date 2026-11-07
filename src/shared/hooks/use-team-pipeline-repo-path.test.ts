// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  resolveTeamPipelineRepoPath,
  useTeamPipelineRepoPath,
} from "./use-team-pipeline-repo-path";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";
const org: Org = {
  slug: "acme",
  display_name: "Acme",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  org_root: "/attached/context",
  teams: [{ name: "planning", source_repos: [] }],
};
afterEach(cleanup);
beforeEach(() => {
  useOrgStore.setState({
    orgs: [],
    workspaceRepos: {},
    stateLoaded: false,
    discoveryComplete: false,
  });
  useViewerStore.setState({ teamPipelineRepoPath: {} });
});
describe("team navigation identity", () => {
  it("resolves content without a source or focused workspace", () => {
    expect(resolveTeamPipelineRepoPath("acme:planning", [org])).toBe("team:acme:planning");
    useOrgStore.setState({ orgs: [org] });
    expect(renderHook(() => useTeamPipelineRepoPath(null, "acme:planning")).result.current).toBe(
      "team:acme:planning",
    );
  });
  it("retains content identity through adding, reordering and removing sources", () => {
    useOrgStore.setState({ orgs: [org] });
    const { result } = renderHook(() => useTeamPipelineRepoPath(null, "acme:planning"));
    const sources = [
      { owner: "Acme", repo: "Api" },
      { owner: "acme", repo: "web" },
    ];
    for (const source_repos of [sources, sources.slice().reverse(), []]) {
      act(() =>
        useOrgStore.setState({ orgs: [{ ...org, teams: [{ ...org.teams[0], source_repos }] }] }),
      );
      expect(result.current).toBe("team:acme:planning");
    }
  });
  it("ignores stale source hints until the registered team is discovered", () => {
    useViewerStore.setState({
      teamPipelineRepoPath: { "acme:planning": "workspace:other:wrong:acme/api" },
    });
    const { result } = renderHook(() => useTeamPipelineRepoPath(null, "acme:planning"));
    expect(result.current).toBe("");
    act(() => useOrgStore.setState({ orgs: [org], stateLoaded: true, discoveryComplete: true }));
    expect(result.current).toBe("team:acme:planning");
    act(() => useOrgStore.setState({ orgs: [] }));
    expect(result.current).toBe("");
  });
  it("rejects absent, removed and sentinel teams", () => {
    for (const key of ["acme:missing", "acme:_none", "_no_team:_none", "bad"]) {
      expect(resolveTeamPipelineRepoPath(key, [org])).toBe("");
    }
  });
});

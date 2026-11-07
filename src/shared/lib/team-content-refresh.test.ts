// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";
import { useOrgStore } from "@/shared/stores/org-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import type { Org } from "@/shared/types/domain";
import { bumpKeyForContextFileChange } from "./build-synthetic-repo";
import { bumpFileRefreshForSourceRepo } from "./refresh-for-source-repo";
const org: Org = {
  slug: "acme",
  display_name: "Acme",
  org_root: "/ctx",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [
    { name: "parent", source_repos: [] },
    { name: "child", parent_team: "parent", source_repos: [] },
    { name: "private", source_repos: [] },
  ],
};
beforeEach(() => {
  seedFileRefresh();
  useOrgStore.setState({ orgs: [org], workspaceRepos: {} });
});
describe("team-owned content refresh", () => {
  it("routes an external event without any source or workspace subscriber", () => {
    const key = bumpKeyForContextFileChange({
      repo_path: "",
      file_path: "/ctx/child/content/readme.md",
    });
    expect(key).toBe("team:acme:child");
    bumpFileRefreshForSourceRepo(key!, new Set(["/ctx/child/content/readme.md"]));
    expect(useFileRefreshStore.getState().contentRefreshKeys[key!]).toBe(1);
    expect(useFileRefreshStore.getState().lastChangedContextFile[key!]).toEqual(
      new Set(["/ctx/child/content/readme.md"]),
    );
    expect(useFileRefreshStore.getState().fileRefreshKeys["team:acme:private"]).toBeUndefined();
  });
  it("routes an inherited event to its inheriting team", () => {
    expect(
      bumpKeyForContextFileChange({
        repo_path: "",
        file_path: "/ctx/parent/content/a.md",
        inherited_by: "/ctx/child/content",
      }),
    ).toBe("team:acme:child");
  });
  it("rejects sibling-prefix paths and preserves counter-only refresh", () => {
    expect(
      bumpKeyForContextFileChange({ repo_path: "", file_path: "/ctx/child/content-archive/a.md" }),
    ).toBeNull();
    bumpFileRefreshForSourceRepo("team:acme:child", undefined, true);
    expect(useFileRefreshStore.getState().fileRefreshKeys["team:acme:child"]).toBe(1);
    expect(useFileRefreshStore.getState().contentRefreshKeys["team:acme:child"]).toBeUndefined();
  });
});

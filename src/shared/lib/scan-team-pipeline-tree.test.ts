// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({ scan: vi.fn(), load: vi.fn(), save: vi.fn() }));
vi.mock("@/tauri/commands", async () => ({
  ...(await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands")),
  scanPipelineTree: api.scan,
  loadWorkspaceConfig: api.load,
  saveWorkspaceConfig: api.save,
}));
import { scanTeamPipelineTree } from "./scan-team-pipeline-tree";
import { useOrgStore } from "@/shared/stores/org-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { setCodesenseHomeForTests } from "@/shared/lib/codesense-home";
import { buildTree } from "./pipeline-graph/test-fixtures/derived-tree";
beforeEach(() => {
  setCodesenseHomeForTests("/settings");
  useOrgStore.setState({
    discoveryComplete: true,
    workspaceRepos: {},
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        org_root: "/attached",
        created_at: "",
        remote_url: null,
        config_flags: {},
        people: [],
        teams: [{ name: "planning", source_repos: [] }],
      },
    ],
  });
  useConfigStore.setState({ teamMainDocPreferences: {}, repos: [] });
  api.scan.mockReset().mockResolvedValue(buildTree("content"));
  api.load
    .mockReset()
    .mockResolvedValue({ repos: { "workspace:acme:planning:acme/api": { mainDoc: "Epic.md" } } });
  api.save.mockReset().mockResolvedValue(undefined);
});
it("an unopened zero-source team's imperative scan uses its persisted team convention", async () => {
  expect((await scanTeamPipelineTree("acme:planning"))?.dirName).toBe("content");
  expect(api.scan).toHaveBeenCalledWith("/attached/planning/content", "", "", "Epic.md");
  expect(api.save.mock.calls[0][1].repos["team:acme:planning"]).toEqual({ mainDoc: "Epic.md" });
  expect(useOrgStore.getState().workspaceRepos).toEqual({});
});
it("optional preference failure keeps default content classification available", async () => {
  api.load.mockRejectedValueOnce(new Error("settings unavailable"));
  expect((await scanTeamPipelineTree("acme:planning"))?.dirName).toBe("content");
  expect(api.scan).toHaveBeenCalledWith("/attached/planning/content", "", "", "");
});

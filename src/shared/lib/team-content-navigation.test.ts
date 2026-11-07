// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
import { teamContentNavigationPath } from "./team-content-context";
import type { Org } from "@/shared/types/domain";
const org: Org = {
  slug: "acme",
  display_name: "Acme",
  org_root: "/attached/plans",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [
    { name: "planning", parent_team: "parent", source_repos: [] },
    { name: "parent", source_repos: [] },
    { name: "private", source_repos: [] },
  ],
};
it("maps an ancestor's real file path into merged own-root navigation coordinates", () => {
  expect(
    teamContentNavigationPath(
      "acme:planning",
      "/attached/plans/parent/content/nested/guide.md",
      [org],
      "parent",
    ),
  ).toBe("/attached/plans/planning/content/nested/guide.md");
  expect(
    teamContentNavigationPath("acme:planning", "/attached/plans/private/content/nested/guide.md", [
      org,
    ]),
  ).toBeNull();
  expect(
    teamContentNavigationPath("acme:planning", "/attached/plans/parent/content-archive/guide.md", [
      org,
    ]),
  ).toBeNull();
});

vi.mock("@/tauri/commands", async () => ({
  ...(await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands")),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveAppState: vi.fn().mockResolvedValue(undefined),
}));
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useTeamSurfaceStore } from "@/features/viewer/lib/team-surface-store";
it("reopening an inherited tab keeps its physical file and reveals its logical ancestor folders", async () => {
  const key = "acme:planning";
  const root = "/attached/plans/planning/content";
  const path = "/attached/plans/parent/content/nested/guide.md";
  useTeamSurfaceStore.setState({
    trees: {
      [key]: {
        dirPath: "",
        dirName: "content",
        absoluteDirPath: root,
        stories: [],
        docs: [{ fileName: "guide.md", filePath: path, sourceTeam: "parent" }],
        subEpics: [],
        orderError: null,
        mainDoc: null,
      },
    },
  });
  useOrgStore.setState({ orgs: [org], discoveryComplete: true, workspaceRepos: {} });
  useViewerStore.setState({
    openTabs: {},
    activeTab: {},
    drillPaths: { [key]: `${root}/other` },
    pipelineScope: { [key]: "other" },
    statusNavExpandedPaths: {},
    revealCounter: {},
  });
  useViewerStore.getState().openFile(key, "team:acme:planning", path);
  await vi.waitFor(() => expect(useViewerStore.getState().revealCounter[key]).toBe(1));
  const state = useViewerStore.getState();
  expect(state.activeTab[key]).toBe(path);
  expect(state.drillPaths[key]).toBe(root);
  expect(state.pipelineScope[key]).toBe("");
  expect(state.statusNavExpandedPaths[key]).toContain(`${root}/nested`);
  expect(state.revealCounter[key]).toBe(1);
});

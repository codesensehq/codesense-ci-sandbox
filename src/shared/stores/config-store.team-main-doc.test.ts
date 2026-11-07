// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import type { WorkspaceConfig } from "@/shared/lib/workspace-config";
const api = vi.hoisted(() => ({ load: vi.fn(), save: vi.fn() }));
vi.mock("@/tauri/commands", async () => ({
  ...(await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands")),
  loadWorkspaceConfig: api.load,
  saveWorkspaceConfig: api.save,
}));
import {
  useConfigStore,
  flushConfigForProfileSwitch,
  resetWorkspaceConfigView,
  bindWorkspaceConfigPrincipal,
  applyWorkspaceConfig,
} from "./config-store";
import { useOrgStore } from "./org-store";
import { setCodesenseHomeForTests } from "@/shared/lib/codesense-home";
import type { Org } from "@/shared/types/domain";
const source = "workspace:acme:planning:acme/api";
const org: Org = {
  slug: "acme",
  display_name: "Acme",
  org_root: "/attached",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [
    { name: "planning", source_repos: [{ owner: "acme", repo: "api" }] },
    { name: "other", source_repos: [] },
  ],
};
let disk: WorkspaceConfig;
beforeEach(async () => {
  await flushConfigForProfileSwitch().catch(() => {});
  resetWorkspaceConfigView();
  setCodesenseHomeForTests("/private/settings");
  disk = { repos: { [source]: { mainDoc: "Epic.md" } } };
  api.load.mockReset().mockImplementation(async () => structuredClone(disk));
  api.save.mockReset().mockImplementation(async (_home, config) => {
    for (const [id, fields] of Object.entries((config as WorkspaceConfig).repos)) {
      disk.repos[id] = { ...disk.repos[id], ...structuredClone(fields) };
    }
  });
  useConfigStore.setState({ teamMainDocPreferences: {}, repos: [] });
  useOrgStore.setState({
    orgs: [org],
    discoveryComplete: true,
    workspaceRepos: {
      [source]: {
        repoPath: source,
        repoName: "api",
        contextRepoPath: "/attached/planning/repos/acme/api",
        teamContentPath: "/attached/planning/content",
        mainDoc: "Epic.md",
        hasRemote: false,
        remoteUrl: null,
      },
    },
  });
});
it("persists one-time migration and rehydrates it after returning to zero sources", async () => {
  await useConfigStore.getState().ensureTeamMainDocPreference("acme:planning");
  expect(disk.repos["team:acme:planning"]).toEqual({ mainDoc: "Epic.md" });
  useOrgStore.setState({
    orgs: [{ ...org, teams: [{ ...org.teams[0], source_repos: [] }] }],
    workspaceRepos: {},
  });
  useConfigStore.setState({ teamMainDocPreferences: {} });
  await useConfigStore.getState().ensureTeamMainDocPreference("acme:planning");
  expect(useConfigStore.getState().teamMainDocPreferences["acme:planning"].mainDoc).toBe("Epic.md");
  expect(useOrgStore.getState().workspaceRepos).toEqual({});
  expect(useConfigStore.getState().repos).toEqual([]);
});
it("serializes concurrent source and explicit team writes without dropping either", async () => {
  await Promise.all([
    useConfigStore.getState().setTeamMainDoc("acme:planning", ""),
    useConfigStore.getState().setMainDoc(source, "Source.md"),
  ]);
  expect(disk.repos["team:acme:planning"]).toEqual({ mainDoc: "" });
  expect(disk.repos[source]).toEqual({ mainDoc: "Source.md" });
});
it("late preference loads retain captured team ownership", async () => {
  let finish!: (value: WorkspaceConfig) => void;
  api.load.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const a = useConfigStore.getState().ensureTeamMainDocPreference("acme:planning");
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  bindWorkspaceConfigPrincipal("42");
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "other" });
  const b = useConfigStore.getState().ensureTeamMainDocPreference("acme:other");
  finish(structuredClone(disk));
  await Promise.all([a, b]);
  expect(useConfigStore.getState().teamMainDocPreferences["acme:planning"].mainDoc).toBe("Epic.md");
  expect(useConfigStore.getState().teamMainDocPreferences["acme:other"].mainDoc).toBe("");
});
it("keeps optional discovery failure nonblocking and does not commit a failed explicit choice", async () => {
  api.load.mockRejectedValueOnce(new Error("settings unavailable"));
  await expect(
    useConfigStore.getState().ensureTeamMainDocPreference("acme:planning"),
  ).rejects.toThrow("settings unavailable");
  await expect(flushConfigForProfileSwitch()).resolves.toBeUndefined();
  await useConfigStore.getState().setTeamMainDoc("acme:planning", "Team.md");
  api.save.mockRejectedValueOnce(new Error("disk full"));
  await expect(
    useConfigStore.getState().setTeamMainDoc("acme:planning", "Failed.md"),
  ).rejects.toThrow("disk full");
  expect(useConfigStore.getState().teamMainDocPreferences["acme:planning"].mainDoc).toBe("Team.md");
});

it("persists both an edit and a quick return to the cached value across an org switch", async () => {
  bindWorkspaceConfigPrincipal("42");
  disk.repos["team:acme:planning"] = { mainDoc: "Epic.md" };
  applyWorkspaceConfig(structuredClone(disk));
  let finish!: () => void;
  api.save.mockImplementationOnce(async (_home, patch: WorkspaceConfig) => {
    await new Promise<void>((resolve) => {
      finish = resolve;
    });
    Object.assign(disk.repos, structuredClone(patch.repos));
  });
  const first = useConfigStore.getState().setTeamMainDoc("acme:planning", "First.md");
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  const second = useConfigStore.getState().setTeamMainDoc("acme:planning", "Epic.md");
  useOrgStore.setState({ activeOrgSlug: "other", activeTeamName: "dev" });
  bindWorkspaceConfigPrincipal("42");
  finish();
  await Promise.all([first, second]);
  expect(api.save.mock.calls.map(([, patch]) => patch.repos["team:acme:planning"].mainDoc)).toEqual(
    ["First.md", "Epic.md"],
  );
  expect(disk.repos["team:acme:planning"].mainDoc).toBe("Epic.md");
});

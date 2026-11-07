// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@/test-utils";
import { installMockBackend } from "@/app/mock/install";
import { flushRefreshDebounce } from "@/app/refresh-debounce";
import { bumpKeyForContextFileChange } from "@/shared/lib/build-synthetic-repo";
import { bumpFileRefreshForSourceRepo } from "@/shared/lib/refresh-for-source-repo";
import { createKeyedDebounce, type KeyedDebounce } from "@/shared/lib/keyed-debounce";
import { teamAllContextId } from "@/shared/lib/context-model";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { logger } from "@/shared/lib/logger";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { useConfigStore } from "@/shared/stores/config-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { resetContextGitStoreForTests } from "@/features/git-sync/store/use-context-git-store";
import { useWorktreeRosterStore } from "../stores/worktree-roster-store";
import { TeamWorkspaceView } from "./team-workspace-view";
import type { Org } from "@/shared/types/domain";

const team = "acme/plans:planning";
const identity = teamAllContextId("acme/plans", "planning");
const root = "/attached/plans/planning/content";
const planning = `${root}/_bmad-output/planning`;
const addedFolder = `${planning}/connected-experience`;
const sibling = `${planning}/existing.md`;
const addedDoc = `${addedFolder}/walkthrough.md`;
const addedHtml = `${planning}/PIPELINE.html`;
const org: Org = {
  slug: "acme/plans",
  display_name: "Plans",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  org_root: "/attached/plans",
  teams: [
    { name: "planning", source_repos: [] },
    { name: "other", source_repos: [] },
  ],
};
let published = false;
const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
let refresh: KeyedDebounce<string>;
const changedPaths = new Map<string, Set<string>>();

type Internals = { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };

function scanTree() {
  return buildTree("content", {
    absoluteDirPath: root,
    subEpics: [
      buildTree("_bmad-output", {
        dirPath: "_bmad-output",
        absoluteDirPath: `${root}/_bmad-output`,
        subEpics: [
          buildTree("planning", {
            dirPath: "_bmad-output/planning",
            absoluteDirPath: planning,
            docs: [
              { fileName: "existing.md", filePath: sibling },
              ...(published ? [{ fileName: "PIPELINE.html", filePath: addedHtml }] : []),
            ],
            subEpics: published
              ? [
                  buildTree("connected-experience", {
                    dirPath: "_bmad-output/planning/connected-experience",
                    absoluteDirPath: addedFolder,
                    docs: [{ fileName: "walkthrough.md", filePath: addedDoc }],
                  }),
                ]
              : [],
          }),
        ],
      }),
    ],
  });
}

// Drive App's production owner mapping and refresh helpers, leaving scan/queue/view intact.
function contentChanged(filePath: string) {
  const key = bumpKeyForContextFileChange({ repo_path: "", file_path: filePath });
  expect(key).not.toBeNull();
  const paths = changedPaths.get(key!) ?? new Set<string>();
  paths.add(filePath);
  changedPaths.set(key!, paths);
  refresh.schedule(key!, key!);
}

beforeEach(() => {
  published = false;
  calls.length = 0;
  changedPaths.clear();
  warmScopes.forget(team);
  warmScopes.activate(team);
  fileContentCache.clear();
  resetContextGitStoreForTests();
  useWorktreeRosterStore.setState({ rows: [] });
  useFileRefreshStore.setState({
    fileRefreshKeys: {},
    contentRefreshKeys: {},
    lastChangedContextFile: {},
  });
  installMockBackend();
  const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
  const original = internals.invoke.bind(internals);
  internals.invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === "scan_pipeline_tree") {
      expect(args?.teamContentPath).toBe(root);
      return scanTree();
    }
    if (command === "read_file")
      return args?.path === addedDoc
        ? "# Published walkthrough\n\nThe newly published planning content."
        : "# Existing plan\n\nThe selected sibling stays open.";
    if (command === "list_directory") return [];
    return original(command, args);
  };
  useOrgStore.setState({
    orgs: [org],
    stateLoaded: true,
    discoveryComplete: true,
    activeOrgSlug: org.slug,
    activeTeamName: "planning",
    workspaceRepos: {},
    suspendedWorkspaceRepos: {},
  });
  useConfigStore.setState({ initialized: true, repos: [] });
  useViewerStore.setState({
    stateLoaded: true,
    scopeRecoveryComplete: true,
    openedTeams: [team],
    openTabs: {},
    activeTab: {},
    workspaces: {},
    activeWorkspace: {},
    teamPipelineRepoPath: {},
    layoutMode: "viewer",
    terminalVisible: false,
    rightPaneOpen: false,
    pipelineScope: {},
    drillPaths: {},
    navCollapsed: {},
    navLens: {},
    tabDiff: {},
    statusNavPinnedPaths: {},
    statusNavExpandedPaths: { [team]: [`${root}/_bmad-output`, planning] },
  });
  refresh = createKeyedDebounce<string>(
    300,
    (key) => {
      const paths = changedPaths.get(key);
      changedPaths.delete(key);
      flushRefreshDebounce(key, paths, bumpFileRefreshForSourceRepo, logger);
    },
    { leading: true },
  );
});

afterEach(async () => {
  refresh.cancelAll();
  cleanup();
  await act(async () => {
    await Promise.resolve();
  });
  warmScopes.forget(team);
});

describe("published content through the owning team's native navigation", () => {
  it("refreshes an expanded folder, preserves selection, and opens the new document", async () => {
    render(<TeamWorkspaceView teamKey={team} />);
    fireEvent.click(await screen.findByText("existing"));
    await screen.findByRole("heading", { name: "Existing plan" });
    expect(screen.queryByText("connected-experience")).toBeNull();
    const scansBefore = calls.filter((call) => call.command === "scan_pipeline_tree").length;

    published = true;
    act(() => contentChanged("/attached/plans/other/content/unrelated.md"));
    expect(useFileRefreshStore.getState().fileRefreshKeys[identity]).toBeUndefined();
    act(() => {
      contentChanged(addedDoc);
      contentChanged(addedHtml);
    });
    const folder = await screen.findByText("connected-experience");
    await screen.findByText("PIPELINE");
    expect(calls.filter((call) => call.command === "scan_pipeline_tree").length).toBeGreaterThan(
      scansBefore,
    );
    expect(useViewerStore.getState().activeTab[team]).toBe(sibling);
    expect(screen.getByRole("heading", { name: "Existing plan" })).toBeVisible();
    expect(screen.getByText("existing")).toBeVisible();
    expect(useViewerStore.getState().statusNavExpandedPaths[team]).toContain(planning);
    fireEvent.click(folder);
    fireEvent.click(await screen.findByText("walkthrough"));
    await screen.findByRole("heading", { name: "Published walkthrough" });
    expect(useViewerStore.getState().activeTab[team]).toBe(addedDoc);
    expect(calls).toContainEqual({ command: "read_file", args: { path: addedDoc } });
    expect(useViewerStore.getState().navLens[team]?.source?.kind ?? "clone").toBe("clone");
    expect(useViewerStore.getState().pipelineScope[team] ?? "").toBe("");
  });

  it("keeps the drilled scope and active filter while newly eligible rows refresh", async () => {
    useViewerStore.setState({
      pipelineScope: { [team]: "_bmad-output/planning" },
      drillPaths: { [team]: planning },
    });
    render(<TeamWorkspaceView teamKey={team} />);
    await screen.findByText("existing");
    const filter = screen.getByRole("textbox", { name: "Filter files in the nav" });
    fireEvent.change(filter, { target: { value: "walkthrough" } });
    await waitFor(() => expect(screen.queryByText("existing")).toBeNull());

    published = true;
    act(() => contentChanged(addedDoc));
    expect(await screen.findByText("walkthrough")).toBeVisible();
    expect(screen.getByText("connected-experience")).toBeVisible();
    expect(screen.queryByText("PIPELINE")).toBeNull();
    expect(screen.queryByText("existing")).toBeNull();
    expect(filter).toHaveValue("walkthrough");
    expect(useViewerStore.getState().pipelineScope[team]).toBe("_bmad-output/planning");
    expect(useViewerStore.getState().drillPaths[team]).toBe(planning);

    fireEvent.change(filter, { target: { value: "" } });
    expect(await screen.findByText("existing")).toBeVisible();
    expect(await screen.findByText("PIPELINE")).toBeVisible();
  });
});

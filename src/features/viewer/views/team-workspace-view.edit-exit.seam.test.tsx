// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { useOrgStore } from "@/shared/stores/org-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";
import { TeamWorkspaceView } from "./team-workspace-view";

const key = "acme/plans:planning";
const root = "/attached/plans/planning/content";
const org: Org = {
  slug: "acme/plans",
  display_name: "Plans",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  org_root: "/attached/plans",
  teams: [{ name: "planning", source_repos: [] }],
};
let diskText = "# Shared handbook\n\nPlan without a source repository.";
const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
type Internals = { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };

beforeEach(() => {
  calls.length = 0;
  diskText = "# Shared handbook\n\nPlan without a source repository.";
  fileContentCache.clear();
  useFileRefreshStore.setState({
    fileRefreshKeys: {},
    contentRefreshKeys: {},
    lastChangedContextFile: {},
  });
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  installMockBackend();
  const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
  const original = internals.invoke.bind(internals);
  internals.invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === "scan_pipeline_tree")
      return {
        dirPath: "",
        dirName: "content",
        absoluteDirPath: String(args?.teamContentPath),
        stories: [],
        docs: [
          { fileName: "team-handbook.md", filePath: `${root}/team-handbook.md`, sourceTeam: null },
        ],
        subEpics: [],
        orderError: null,
        mainDoc: null,
      };
    if (command === "create_file") return undefined;
    if (command === "read_file")
      return String(args?.path).endsWith("sibling.md") ? "# Sibling page" : diskText;
    if (command === "write_content_file") {
      diskText = String(args?.content);
      return undefined;
    }
    return original(command, args);
  };
  useOrgStore.setState({
    orgs: [org],
    stateLoaded: true,
    activeOrgSlug: org.slug,
    activeTeamName: "planning",
    workspaceRepos: {},
    suspendedWorkspaceRepos: {},
  });
  useConfigStore.setState({ initialized: true, repos: [] });
  useViewerStore.setState({
    stateLoaded: true,
    scopeRecoveryComplete: true,
    openedTeams: [key],
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
  });
});
afterEach(async () => {
  cleanup();
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
});

vi.mock("@/shared/components/code-editor", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/components/code-editor")>();
  return {
    ...actual,
    CodeEditor: ({
      value,
      onChange,
      ariaLabel,
    }: {
      value: string;
      onChange?: (text: string) => void;
      ariaLabel?: string;
    }) => (
      <textarea aria-label={ariaLabel} value={value} onChange={(e) => onChange?.(e.target.value)} />
    ),
  };
});

async function openHandbook() {
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  fireEvent.click((await screen.findAllByText("team-handbook"))[0]);
  await screen.findByRole("heading", { name: "Shared handbook" });
}

async function editHandbook() {
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  return screen.findByRole("textbox", { name: "Editor for team-handbook.md" });
}

async function expectHandbook(title = "Shared handbook") {
  expect(await screen.findByRole("heading", { name: title })).toBeVisible();
  expect(useViewerStore.getState().activeTab[key]).toBe(`${root}/team-handbook.md`);
  expect(screen.queryByTestId("context-editor-inset")).toBeNull();
}

describe("editor exit receives current markdown", () => {
  it("Done without edits remounts the cached page after a path refresh", async () => {
    await openHandbook();
    await editHandbook();
    fireEvent.click(screen.getByRole("button", { name: "Done editing" }));
    await expectHandbook();
    expect(
      calls.filter((c) => c.command === "read_file" && c.args?.path === `${root}/team-handbook.md`)
        .length,
    ).toBeGreaterThan(1);
    expect(calls.some((c) => c.command === "write_content_file")).toBe(false);
  });

  it("Discard remounts the disk copy, never the unsaved buffer", async () => {
    await openHandbook();
    fireEvent.change(await editHandbook(), { target: { value: "# Unsaved draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Done editing" }));
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    await expectHandbook();
    expect(screen.queryByRole("heading", { name: "Unsaved draft" })).toBeNull();
    expect(calls.some((c) => c.command === "write_content_file")).toBe(false);
  });

  it("save, visit another tab, return and Done preserves the saved file and a second clean exit", async () => {
    await openHandbook();
    fireEvent.change(await editHandbook(), { target: { value: "# Saved handbook" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(diskText).toBe("# Saved handbook"));
    act(() => useViewerStore.getState().openFile(key, root, `${root}/sibling.md`));
    await screen.findByRole("heading", { name: "Sibling page" });
    act(() => useViewerStore.getState().openFile(key, root, `${root}/team-handbook.md`));
    expect(await screen.findByRole("textbox", { name: "Editor for team-handbook.md" })).toHaveValue(
      "# Saved handbook",
    );
    // Model the existing idle prefetch having already cached the saved bytes.
    fileContentCache.set(`${root}/team-handbook.md`, diskText, 0);
    fireEvent.click(screen.getByRole("button", { name: "Done editing" }));
    await expectHandbook("Saved handbook");
    await editHandbook();
    fireEvent.click(screen.getByRole("button", { name: "Done editing" }));
    await expectHandbook("Saved handbook");
    expect(calls.filter((c) => c.command === "write_content_file")).toHaveLength(1);
  });
});

// SPDX-License-Identifier: MIT
import { resetContentFileIndexForTests } from "@/shared/lib/pipeline-graph/content-scan-queue";
import { registerFrame } from "@/features/dashboard/lib/frame-lifecycle";
import { NativeTextMenu } from "@/features/shell/native-text-menu";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
} from "@testing-library/react";
import { useShortcutHandler } from "@/shared/hooks/use-shortcut-handler";
import { useUiStore } from "@/shared/stores/ui-store";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { useOrgStore } from "@/shared/stores/org-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { teamAllContextId } from "@/shared/lib/context-model";
import {
  resetContextGitStoreForTests,
  useContextGitStore,
} from "@/features/git-sync/store/use-context-git-store";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";
import type { Org } from "@/shared/types/domain";
import { useTeamSurfaceStore } from "../lib/team-surface-store";
import { TeamWorkspaceView } from "./team-workspace-view";
import { useWorktreeRosterStore } from "../stores/worktree-roster-store";
import type { ContextWorktreeInfo } from "@/tauri/commands";

vi.mock("@/features/git-sync/components/redline-prose", () => ({
  RedlineProse: () => <div>Redline content</div>,
}));
vi.mock("@/features/viewer/components/context-editor-inset", () => ({
  ContextEditorInset: ({ onExit }: { onExit: () => void }) => (
    <button onClick={onExit}>Done editing</button>
  ),
}));
vi.mock("@/features/dashboard/components/html-viewer", () => ({
  HtmlViewer: () => <div>HTML preview</div>,
}));
vi.mock("@/features/dashboard/components/image-viewer", () => ({
  ImageViewer: () => <div>Image preview</div>,
}));
const viewerDispatch = vi.hoisted(() => vi.fn());
vi.mock("@/shared/lib/render-viewer", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/render-viewer")>();
  return {
    ...actual,
    renderViewer: (...args: Parameters<typeof actual.renderViewer>) => {
      viewerDispatch(args[0], args[1]);
      return actual.renderViewer(...args);
    },
  };
});

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
let empty = false;
let scanError: string | null = null;
let privateScanError: string | null = null;
let privateScanRead: (() => Promise<unknown>) | undefined;
let fileText = "";
let fileTexts: Record<string, string> = {};
let inherited = false;
let privateConfig = false;
let nestedPrivate = false;
let indexRead: () => Promise<unknown> = async () => [];
let finishIndex: (() => void) | undefined;
const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
type Internals = { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };

/** ⌘P is the registry's file quick-open: press it through the app-root shortcut handler. */
function pressQuickOpen() {
  const handler = renderHook(() => useShortcutHandler());
  fireEvent.keyDown(document, { key: "p", metaKey: true });
  handler.unmount();
}

beforeEach(() => {
  resetContentFileIndexForTests();
  // The team is the shown one, with no tree cached from an earlier test.
  warmScopes.forget(key);
  warmScopes.activate(key);
  useUiStore.setState({ fileQuickOpenOpen: false });
  resetContextGitStoreForTests();
  fileContentCache.clear();
  fileTexts = {};
  calls.length = 0;
  empty = false;
  scanError = null;
  privateScanError = null;
  privateScanRead = undefined;
  fileText = "# Shared handbook\n\nPlan without a source repository.";
  viewerDispatch.mockClear();
  inherited = false;
  privateConfig = false;
  nestedPrivate = false;
  useWorktreeRosterStore.setState({ rows: [] });
  indexRead = async () => [];
  finishIndex = undefined;
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  installMockBackend();
  const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
  const original = internals.invoke.bind(internals);
  internals.invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === "resolve_private_nav_source")
      return {
        repoRoot: "/home/users/42/scratch",
        contentPath: "/home/users/42/scratch/orgs/acme/plans/content",
        prefix: "orgs/acme/plans/content",
        baselineStarted: false,
        baselineError: null,
      };
    if (command === "list_scratch_space") return { files: [], allOrgs: [], syncedOrgs: [] };
    if (command === "list_context_files") return indexRead();
    if (command === "list_directory" && String(args?.dirPath).includes("/scratch/"))
      return privateConfig
        ? [
            {
              name: "settings.json",
              path: `${args?.dirPath}/settings.json`,
              is_dir: false,
              size: null,
              extension: "json",
            },
          ]
        : [];
    if (command === "get_file_redline")
      return { status: "modified", binary: false, before: "# Before", after: "# After" };
    if (command === "scan_pipeline_tree" && scanError) throw new Error(scanError);
    if (command === "scan_pipeline_tree" && String(args?.teamContentPath).includes("/scratch/")) {
      if (privateScanError) throw new Error(privateScanError);
      if (privateScanRead) return privateScanRead();
    }
    if (command === "scan_pipeline_tree")
      return {
        dirPath: "",
        dirName: "content",
        absoluteDirPath: String(args?.teamContentPath),
        stories: [],
        docs: empty
          ? []
          : [
              {
                fileName: String(args?.teamContentPath).includes("/scratch/")
                  ? "secret.md"
                  : "team-handbook.md",
                filePath: inherited
                  ? "/attached/plans/parent/content/team-handbook.md"
                  : String(args?.teamContentPath).includes("/scratch/")
                    ? `${args?.teamContentPath}/secret.md`
                    : `${root}/team-handbook.md`,
                sourceTeam: inherited ? "parent" : null,
              },
            ],
        subEpics:
          nestedPrivate && String(args?.teamContentPath).includes("/scratch/")
            ? [
                {
                  dirPath: "drafts",
                  dirName: "drafts",
                  absoluteDirPath: `${args?.teamContentPath}/drafts`,
                  stories: [],
                  docs: [
                    { fileName: "draft.md", filePath: `${args?.teamContentPath}/drafts/draft.md` },
                  ],
                  subEpics: [],
                  orderError: null,
                  mainDoc: null,
                },
                {
                  dirPath: "archive",
                  dirName: "archive",
                  absoluteDirPath: `${args?.teamContentPath}/archive`,
                  stories: [],
                  docs: [
                    {
                      fileName: "archived-plan.md",
                      filePath: `${args?.teamContentPath}/archive/archived-plan.md`,
                    },
                  ],
                  subEpics: [],
                  orderError: null,
                  mainDoc: null,
                },
              ]
            : [],
        orderError: null,
        mainDoc: null,
      };
    if (command === "create_file") return undefined;
    if (command === "get_http_api_port") return 9123;
    if (command === "read_file") return fileTexts[String(args?.path)] ?? fileText;
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
    navLens: {},
    tabDiff: {},
    statusNavPinnedPaths: {},
  });
});

function selectPrivate() {
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Me", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  useViewerStore.getState().setNavLens(key, { source: { kind: "private" } });
}
afterEach(async () => {
  cleanup();
  finishIndex?.();
  vi.useRealTimers();
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
});

describe("team content with no source repository", () => {
  it("Find in document from the native text menu reaches the centre viewer and keeps search focus", async () => {
    render(
      <TooltipProvider>
        <NativeTextMenu />
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    fireEvent.click((await screen.findAllByText("team-handbook"))[0]);
    const heading = await screen.findByRole("heading", { name: "Shared handbook" });
    const range = document.createRange();
    range.selectNodeContents(heading);
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(range);
    fireEvent.contextMenu(heading);
    fireEvent.click(await screen.findByRole("menuitem", { name: /Find in document/ }));
    const search = await screen.findByPlaceholderText("Find in document...");
    await act(async () => new Promise(requestAnimationFrame));
    expect(search).toHaveFocus();
    window.getSelection()!.removeAllRanges();
  });
  it.each(["png", "html"])(
    "serves an open worktree %s from its own root after changing the nav source",
    async (extension) => {
      const path = `/context-worktree/planning/content/example.${extension}`;
      useWorktreeRosterStore.setState({
        rows: [
          {
            kind: "context",
            id: "cw_sprint",
            name: "Sprint",
            path: "/context-worktree",
            launchCwd: "/context-worktree/planning/content",
            orgSlug: org.slug,
            team: "planning",
            agents: [],
          } as unknown as ContextWorktreeInfo,
        ],
      });
      useViewerStore.setState({
        openTabs: { [key]: [{ path, name: `example.${extension}` }] },
        activeTab: { [key]: path },
      });
      render(
        <TooltipProvider>
          <TeamWorkspaceView teamKey={key} />
        </TooltipProvider>,
      );
      await waitFor(() => {
        expect(viewerDispatch).toHaveBeenCalledWith(
          expect.objectContaining({
            filePath: path,
            teamContentPath: "/context-worktree/planning/content",
          }),
          expect.anything(),
        );
      });
      expect(useViewerStore.getState().navLens[key]?.source?.kind ?? "clone").toBe("clone");
    },
  );

  it("publishes its scanned tree for the shell's surfaces", async () => {
    useTeamSurfaceStore.setState({ trees: {} });
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    await screen.findAllByText("team-handbook");
    const published = useTeamSurfaceStore.getState().trees[key];
    expect(published?.docs?.map((d) => d.fileName)).toEqual(["team-handbook.md"]);
  });
  it("quick-opens the inherited effective file using its physical parent path", async () => {
    inherited = true;
    useOrgStore.setState({
      orgs: [
        {
          ...org,
          teams: [
            { ...org.teams[0], parent_team: "parent" },
            { name: "parent", source_repos: [] },
          ],
        },
      ],
    });
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    await screen.findAllByText("team-handbook");
    pressQuickOpen();
    const query = await screen.findByPlaceholderText("Search files...");
    fireEvent.change(query, { target: { value: "team-handbook" } });
    const palette = query.closest('[role="dialog"]') ?? query.parentElement!.parentElement!;
    await waitFor(() => expect(calls.some((c) => c.command === "list_context_files")).toBe(true));
    fireEvent.click(
      Array.from(palette.querySelectorAll("button")).find((button) =>
        button.textContent?.includes("team-handbook"),
      )!,
    );
    await waitFor(() =>
      expect(useViewerStore.getState().activeTab[key]).toBe(
        "/attached/plans/parent/content/team-handbook.md",
      ),
    );
    expect(useOrgStore.getState().workspaceRepos).toEqual({});
  });
  it.each([false, true])(
    "quick-open Enter receives the ranked file after the own index arrives (deliberate=%s)",
    async (deliberate) => {
      let resolveIndex!: (files: unknown[]) => void;
      indexRead = () =>
        new Promise((resolve) => {
          resolveIndex = resolve;
        });
      render(
        <TooltipProvider>
          <TeamWorkspaceView teamKey={key} />
        </TooltipProvider>,
      );
      await screen.findAllByText("team-handbook");
      pressQuickOpen();
      const query = await screen.findByPlaceholderText("Search files...");
      fireEvent.change(query, { target: { value: "handbook" } });
      await waitFor(() => expect(resolveIndex).toBeDefined());
      if (deliberate) fireEvent.keyDown(query, { key: "ArrowDown" });
      await act(async () =>
        resolveIndex([
          { name: "handbook.md", relativePath: "handbook.md", fullPath: `${root}/handbook.md` },
        ]),
      );
      fireEvent.keyDown(query, { key: "Enter" });
      await waitFor(() =>
        expect(useViewerStore.getState().activeTab[key]).toBe(
          `${root}/${deliberate ? "team-handbook" : "handbook"}.md`,
        ),
      );
    },
  );

  it("times out quick-open, ignores the late result for that attempt, and reopens from the shared cache", async () => {
    indexRead = () =>
      new Promise((resolve) => {
        finishIndex = () =>
          resolve([
            {
              name: "late-only.md",
              relativePath: "late-only.md",
              fullPath: `${root}/late-only.md`,
            },
          ]);
      });
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    await screen.findAllByText("team-handbook");
    vi.useFakeTimers();
    pressQuickOpen();
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(calls.some((c) => c.command === "list_context_files")).toBe(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10000);
    });
    expect(screen.getByRole("alert")).toHaveTextContent("timed out");
    await act(async () => {
      finishIndex?.();
      for (let i = 0; i < 10; i++) await Promise.resolve();
    });
    expect(screen.queryByText("late-only")).toBeNull();
    fireEvent.keyDown(screen.getByPlaceholderText("Search files..."), { key: "Escape" });
    pressQuickOpen();
    await act(async () => {
      for (let i = 0; i < 15; i++) await Promise.resolve();
    });
    // The late listing is valid for this index generation, so the shared cache serves
    // the reopen: no alert, and no second backend listing.
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("late-only")).toBeInTheDocument();
    expect(calls.filter((c) => c.command === "list_context_files")).toHaveLength(1);
  });
  it("tells the user when quick-open's root is not indexed, instead of an empty list", async () => {
    indexRead = async () => {
      throw `not indexed: ${root}`;
    };
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    await screen.findAllByText("team-handbook");
    pressQuickOpen();
    expect(await screen.findByRole("alert")).toHaveTextContent(/isn't part of your synced context/);
  });
  it("offers existing content creation in a genuinely empty team", async () => {
    empty = true;
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "New file" }));
    fireEvent.change(screen.getByRole("textbox", { name: /name/i }), {
      target: { value: "idea.md" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    await waitFor(() =>
      expect(calls.find((c) => c.command === "create_file")?.args?.path).toBe(`${root}/idea.md`),
    );
    expect(screen.queryByText("Connect your code")).toBeNull();
  });
  it("reports scan failure and retries without covering the workspace", async () => {
    scanError = "Content folder cannot be read";
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(scanError);
    scanError = null;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await screen.findAllByText("team-handbook");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("a team shown again after its view unmounted keeps its tree and never shows the loading frame", async () => {
    const view = render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    await screen.findAllByText("team-handbook");
    view.unmount();
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    expect(screen.queryByText("Loading team context…")).toBeNull();
    expect(screen.getAllByText("team-handbook").length).toBeGreaterThan(0);
  });

  it("scans the registered root, browses and opens plain documents without a workspace", async () => {
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    const rows = await screen.findAllByText("team-handbook");
    expect(calls.find((c) => c.command === "scan_pipeline_tree")?.args?.teamContentPath).toBe(root);
    fireEvent.click(rows[0]);
    await screen.findByRole("heading", { name: "Shared handbook", level: 1 });
    expect(useViewerStore.getState().activeTab[key]).toBe(`${root}/team-handbook.md`);
    expect(useViewerStore.getState().terminalVisible).toBe(false);
    expect(
      calls.filter((c) =>
        /^(add_source_repo|open_workspace|hydrate_worktree|ensure_.*workspace|context_repo_fetch)/.test(
          c.command,
        ),
      ),
    ).toEqual([]);
    expect(useOrgStore.getState().workspaceRepos).toEqual({});
    expect(screen.queryByText("Connect your code")).toBeNull();
  });
  it("keeps document tabs through first source attachment and removal", async () => {
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    fireEvent.click((await screen.findAllByText("team-handbook"))[0]);
    await waitFor(() =>
      expect(useViewerStore.getState().activeTab[key]).toBe(`${root}/team-handbook.md`),
    );
    for (const source_repos of [[{ owner: "acme", repo: "api" }], []]) {
      act(() =>
        useOrgStore.setState({ orgs: [{ ...org, teams: [{ ...org.teams[0], source_repos }] }] }),
      );
      expect(useViewerStore.getState().activeTab[key]).toBe(`${root}/team-handbook.md`);
      expect(
        calls
          .filter((c) => c.command === "scan_pipeline_tree")
          .every((c) => c.args?.teamContentPath === root),
      ).toBe(true);
    }
  });
});

it.each(["clone", "private"] as const)(
  "hides a %s file's Changes across a source switch and restores them on return",
  async (kind) => {
    if (kind === "private") selectPrivate();
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    const name = kind === "private" ? "secret" : "team-handbook";
    fireEvent.click((await screen.findAllByText(name))[0]);
    await screen.findByRole("heading", { name: "Shared handbook", level: 1 });
    await waitFor(() => expect(useContextGitStore.getState().byTeam[key]?.loading).toBe(false));
    const file = useViewerStore.getState().activeTab[key];
    const change: NavChange = {
      absPath: file,
      relPath: `${name}.md`,
      kind: "modified",
      added: 3,
      removed: 1,
    };
    act(() => {
      useContextGitStore.getState().put(key, {
        ...useContextGitStore.getState().byTeam[key],
        changes: [change],
        byPath: new Map([[file, change]]),
      });
    });
    const ownSnapshot = useContextGitStore.getState().byTeam[key];
    expect(screen.getByTestId("diff-side-strip")).toHaveTextContent("+3 −1 lines");
    expect(useViewerStore.getState().tabDiff[key]?.[file]).toBeFalsy();
    fireEvent.click(screen.getByTestId("diff-side-changes"));
    await screen.findByText("Redline content");

    act(() => {
      if (kind === "clone") selectPrivate();
      else useViewerStore.getState().setNavLens(key, { source: { kind: "clone" } });
    });
    await waitFor(() =>
      expect(useContextGitStore.getState().byTeam[key]?.source?.kind).toBe(
        kind === "clone" ? "private" : "clone",
      ),
    );
    expect(screen.queryByTestId("diff-side-strip")).toBeNull();
    expect(screen.queryByText("Redline content")).toBeNull();
    await screen.findByRole("heading", { name: "Shared handbook", level: 1 });
    expect(useViewerStore.getState().tabDiff[key]?.[file]).toBe(true);

    act(() => useViewerStore.getState().setNavLens(key, { source: { kind } }));
    await waitFor(() => {
      expect(useContextGitStore.getState().byTeam[key]?.source?.kind).toBe(kind);
      expect(useContextGitStore.getState().byTeam[key]?.loading).toBe(false);
    });
    act(() => useContextGitStore.getState().put(key, ownSnapshot));
    expect(screen.getByTestId("diff-side-strip")).toHaveTextContent("+3 −1 lines");
    expect(screen.getByTestId("diff-side-changes")).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByTestId("diff-side-strip")).toHaveTextContent(
      kind === "private" ? "vs HEAD · local only" : "vs origin",
    );
    await screen.findByText("Redline content");
  },
);

it.each(["clone", "private"] as const)(
  "refreshes a retained %s viewer from its own root while the other file is active",
  async (kind) => {
    const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
    const cloneFile = `${root}/team-handbook.md`;
    const privateFile = `${privateRoot}/secret.md`;
    const first = kind === "clone" ? cloneFile : privateFile;
    const second = kind === "clone" ? privateFile : cloneFile;
    const cloneKey = teamAllContextId(org.slug, "planning");
    const firstRoot = kind === "clone" ? cloneKey : privateRoot;
    const secondRoot = kind === "clone" ? privateRoot : cloneKey;
    fileTexts[first] = "# Original retained text";
    fileTexts[second] = "# Other file";
    useFileRefreshStore.setState({ contentRefreshKeys: {}, lastChangedContextFile: {} });
    if (kind === "private") selectPrivate();
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    fireEvent.click((await screen.findAllByText(kind === "clone" ? "team-handbook" : "secret"))[0]);
    await screen.findByRole("heading", { name: "Original retained text" });
    act(() => {
      if (kind === "clone") selectPrivate();
      else useViewerStore.getState().setNavLens(key, { source: { kind: "clone" } });
    });
    fireEvent.click((await screen.findAllByText(kind === "clone" ? "secret" : "team-handbook"))[0]);
    await screen.findByRole("heading", { name: "Other file" });

    // Advance the active root first. Its counter must never become the hidden file's stamp.
    act(() => useFileRefreshStore.getState().bumpFileRefresh(secondRoot));
    await waitFor(() => expect(fileContentCache.get(second)?.refreshKey).toBe(1));
    fileTexts[first] = "# Updated retained text";
    const changed = new Set([first]);
    act(() => useFileRefreshStore.getState().bumpFileRefresh(firstRoot, changed));
    act(() => {
      useViewerStore.getState().setNavLens(key, { source: { kind } });
      useViewerStore.getState().setActiveTab(key, root, first);
    });
    await screen.findByRole("heading", { name: "Updated retained text" });
    const inputs = viewerDispatch.mock.calls
      .filter(([common]) => common.filePath === first)
      .at(-1)!;
    expect(inputs[0]).toMatchObject({
      teamContentPath: kind === "clone" ? root : privateRoot,
      refreshKey: 1,
    });
    expect(inputs[1].changedPaths).toEqual(changed);
  },
);

it("switches the actual scanner to the private tree and restores the team tree without publishing private files to the shell", async () => {
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Me", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await screen.findAllByText("team-handbook");
  act(() => useViewerStore.getState().setNavLens(key, { source: { kind: "private" } }));
  await screen.findByText("secret");
  expect(
    calls.some(
      (c) =>
        c.command === "scan_pipeline_tree" &&
        c.args?.teamContentPath === "/home/users/42/scratch/orgs/acme/plans/content",
    ),
  ).toBe(true);
  expect(useTeamSurfaceStore.getState().trees[key]?.docs?.map((d) => d.fileName)).toEqual([
    "team-handbook.md",
  ]);
  // Quick Open must use the same private tree as Files, including its physical paths.
  pressQuickOpen();
  const query = await screen.findByPlaceholderText("Search files...");
  const palette = query.closest('[role="dialog"]') ?? query.parentElement!.parentElement!;
  await waitFor(() => expect(palette.textContent).toContain("secret"));
  expect(palette.textContent).not.toContain("team-handbook");
  fireEvent.click(
    Array.from(palette.querySelectorAll("button")).find((button) =>
      button.textContent?.includes("secret"),
    )!,
  );
  const privatePath = "/home/users/42/scratch/orgs/acme/plans/content/secret.md";
  await waitFor(() => expect(useViewerStore.getState().activeTab[key]).toBe(privatePath));
  act(() => useViewerStore.getState().setTabDiff(key, privatePath, true));
  await waitFor(() =>
    expect(
      calls.some(
        (c) =>
          c.command === "get_file_redline" &&
          c.args?.repoPath === "/home/users/42/scratch" &&
          c.args?.filePath === "orgs/acme/plans/content/secret.md",
      ),
    ).toBe(true),
  );

  privateConfig = true;
  act(() =>
    useFileRefreshStore
      .getState()
      .bumpFileRefresh(
        "/home/users/42/scratch/orgs/acme/plans/content",
        new Set(["/home/users/42/scratch/orgs/acme/plans/content/settings.json"]),
      ),
  );
  await screen.findByText("settings.json");
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  await screen.findAllByText("team-handbook");
  // An open private Changes tab retains its own root after the nav switches back to clone.
  expect(useViewerStore.getState().activeTab[key]).toBe(privatePath);
  expect(
    calls
      .filter((c) => c.command === "get_file_redline")
      .every((c) => c.args?.repoPath === "/home/users/42/scratch"),
  ).toBe(true);
});

it("falls back to clone with a visible note when the selected private profile disappears", async () => {
  useLocalProfileStore.setState({
    initialized: true,
    status: { active: null, profiles: [], privateAuthority: null },
  });
  useViewerStore.getState().setNavLens(key, { source: { kind: "private" } });
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await screen.findByText(
    "Private isn't available: you're not signed in to a profile. Showing your clone.",
  );
  expect(useViewerStore.getState().navLens[key].source).toEqual({ kind: "clone" });
  await screen.findAllByText("team-handbook");
});

it("drills into a private folder and resets folder scope when returning to clone", async () => {
  nestedPrivate = true;
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Me", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  useViewerStore.getState().setNavLens(key, { source: { kind: "private" } });
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await screen.findByText("secret");
  fireEvent.click(await screen.findByRole("button", { name: "Drill into drafts" }));
  await screen.findByText("draft");
  expect(screen.queryByText("secret")).toBeNull();
  expect(useViewerStore.getState().pipelineScope[key]).toBe("drafts");
  expect(useViewerStore.getState().drillPaths[key]).toBe(
    "/home/users/42/scratch/orgs/acme/plans/content/drafts",
  );
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  await screen.findAllByText("team-handbook");
  expect(useViewerStore.getState().pipelineScope[key]).toBe("");
  expect(useViewerStore.getState().drillPaths[key]).toBe("");
});

it("keeps an open team document's metadata tied to the org when the nav selects Private", async () => {
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Me", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  fireEvent.click((await screen.findAllByText("team-handbook"))[0]);
  fireEvent.click(await screen.findByRole("button", { name: "Document metadata" }));
  await waitFor(() => expect(calls.some((c) => c.command === "get_metadata_model")).toBe(true));
  act(() => useViewerStore.getState().setNavLens(key, { source: { kind: "private" } }));
  await screen.findByText("secret");
  const metadataCalls = calls.filter((c) =>
    ["get_metadata_model", "list_source_repos_for_team"].includes(c.command),
  );
  expect(metadataCalls.every((c) => c.args?.orgRoot === "/attached/plans")).toBe(true);
  expect(useViewerStore.getState().activeTab[key]).toBe(`${root}/team-handbook.md`);
});

it.each(["html", "png", "md"])(
  "keeps a private %s file's common viewer root after the nav returns to clone",
  async (extension) => {
    selectPrivate();
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={key} />
      </TooltipProvider>,
    );
    await screen.findByText("secret");
    const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
    const path = `${privateRoot}/preview.${extension}`;
    act(() => useViewerStore.getState().openFile(key, root, path));
    await waitFor(() =>
      expect(viewerDispatch).toHaveBeenCalledWith(
        expect.objectContaining({ filePath: path, teamContentPath: privateRoot }),
        expect.anything(),
      ),
    );
    viewerDispatch.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
    await screen.findAllByText("team-handbook");
    expect(viewerDispatch).toHaveBeenCalledWith(
      expect.objectContaining({ filePath: path, teamContentPath: privateRoot }),
      expect.anything(),
    );
    expect(
      viewerDispatch.mock.calls
        .filter(([props]) => props.filePath === path)
        .every(([props]) => props.teamContentPath === privateRoot),
    ).toBe(true);
  },
);

it("opens a root-relative Markdown link inside its private content root", async () => {
  selectPrivate();
  fileText = "# Private link test\n\n[Private destination](/content/target.md)";
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await screen.findByText("secret");
  const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
  act(() => useViewerStore.getState().openFile(key, root, `${privateRoot}/root-links.md`));
  const link = await screen.findByRole("link", { name: "Private destination" });
  await act(async () => {
    fireEvent.click(link);
  });
  expect(useViewerStore.getState().activeTab[key]).toBe(`${privateRoot}/target.md`);
});

it("shows a private scan error with Retry and a working Source picker", async () => {
  selectPrivate();
  privateScanError = "Private folder cannot be read";
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(privateScanError);
  expect(screen.getByTestId("left-nav-lens-button")).toBeVisible();
  privateScanError = null;
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  await screen.findByText("secret");
  expect(screen.queryByRole("alert")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  await screen.findAllByText("team-handbook");
});

it("can leave Private through the Source picker after its initial scan times out", async () => {
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await screen.findAllByText("team-handbook");
  let finish!: () => void;
  privateScanRead = () =>
    new Promise((resolve) => {
      finish = () =>
        resolve({
          dirPath: "",
          dirName: "",
          absoluteDirPath: "/home/users/42/scratch/orgs/acme/plans/content",
          stories: [],
          docs: [],
          subEpics: [],
          orderError: null,
          mainDoc: null,
        });
    });
  vi.useFakeTimers();
  act(() => selectPrivate());
  await act(async () => {
    await Promise.resolve();
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(200);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(11_000);
  });
  expect(screen.getByRole("alert")).toHaveTextContent(/timed out/i);
  expect(screen.getByRole("button", { name: "Retry" })).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  await act(async () => {
    finish();
    await vi.advanceTimersByTimeAsync(200);
  });
  vi.useRealTimers();
  await screen.findAllByText("team-handbook");
  expect(useViewerStore.getState().navLens[key].source).toEqual({ kind: "clone" });
});

it("hides clone pins in Private and preserves them when the source is cleared", async () => {
  useViewerStore.setState({ statusNavPinnedPaths: { [key]: ["team-handbook.md"] } });
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await screen.findByTestId("pinned-row");
  act(() => selectPrivate());
  await screen.findByText("secret");
  expect(screen.queryByTestId("pinned-row")).toBeNull();
  expect(screen.queryByRole("button", { name: "Unpin" })).toBeNull();
  expect(useViewerStore.getState().statusNavPinnedPaths[key]).toEqual(["team-handbook.md"]);
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  await screen.findByTestId("pinned-row");
  expect(useViewerStore.getState().statusNavPinnedPaths[key]).toEqual(["team-handbook.md"]);
});

it("refreshes the private read view when a retained editor exits under the clone source", async () => {
  selectPrivate();
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  fireEvent.click(await screen.findByText("secret"));
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  await screen.findByRole("button", { name: "Done editing" });
  fireEvent.click(screen.getByRole("button", { name: "Clear Private" }));
  await screen.findAllByText("team-handbook");
  const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
  const before = useFileRefreshStore.getState().contentRefreshKeys[privateRoot] ?? 0;
  fireEvent.click(screen.getByRole("button", { name: "Done editing" }));
  expect(useFileRefreshStore.getState().contentRefreshKeys[privateRoot]).toBe(before + 1);
  expect(useFileRefreshStore.getState().lastChangedContextFile[privateRoot]).toEqual(
    new Set([`${privateRoot}/secret.md`]),
  );
});

it("Quick Open reveals a private file in another folder while drilled in", async () => {
  nestedPrivate = true;
  selectPrivate();
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
  fireEvent.click(await screen.findByRole("button", { name: "Drill into drafts" }));
  await screen.findByText("draft");
  const revealBefore = useViewerStore.getState().revealCounter[key] ?? 0;
  pressQuickOpen();
  const query = await screen.findByPlaceholderText("Search files...");
  fireEvent.change(query, { target: { value: "archived-plan" } });
  const palette = query.closest('[role="dialog"]') ?? query.parentElement!.parentElement!;
  const option = Array.from(palette.querySelectorAll("button")).find((button) =>
    button.textContent?.includes("archived-plan"),
  );
  expect(option).toBeDefined();
  fireEvent.click(option!);
  await waitFor(() =>
    expect(useViewerStore.getState().activeTab[key]).toBe(
      `${privateRoot}/archive/archived-plan.md`,
    ),
  );
  expect(useViewerStore.getState().drillPaths[key]).toBe(privateRoot);
  expect(useViewerStore.getState().pipelineScope[key]).toBe("");
  expect(useViewerStore.getState().statusNavExpandedPaths[key]).toContain(`${privateRoot}/archive`);
  expect(useViewerStore.getState().revealCounter[key]).toBeGreaterThan(revealBefore);
  await screen.findByText("archived-plan");
});

it("closing a shared private HTML path retires only the owning team's frame", async () => {
  const path = "/home/users/42/scratch/orgs/acme/plans/content/shared.html";
  useViewerStore.setState({
    openTabs: { [key]: [{ path, name: "Shared" }] },
    activeTab: { [key]: path },
  });
  const view = render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  await waitFor(() =>
    expect(viewerDispatch).toHaveBeenCalledWith(
      expect.objectContaining({ filePath: path }),
      expect.anything(),
    ),
  );
  const posts = [vi.fn(), vi.fn()];
  const off = [key, "acme:other"].map((teamKey, i) =>
    registerFrame({
      frameId: teamKey,
      dashboard: path,
      teamKey,
      documentPath: path,
      slotId: "centre",
      post: posts[i],
      hide: vi.fn(),
    }),
  );
  try {
    act(() => useViewerStore.getState().closeTab(key, "", path, { user: true }));
    await waitFor(() => expect(posts[0]).toHaveBeenCalled());
    expect(posts[0].mock.calls[0][0].type).toBe("codesense:flush");
    expect(posts[1]).not.toHaveBeenCalled();
    expect(viewerDispatch).toHaveBeenCalledWith(
      expect.objectContaining({ filePath: path, hostTeamKey: key }),
      expect.anything(),
    );
  } finally {
    view.unmount();
    off.forEach((dispose) => dispose());
  }
});

it("keeps a dirty retained team's publications out of B's stories subscription after switching and later A updates", async () => {
  const { LiveHub } = await import("@/features/dashboard/lib/live-hub");
  const { buildTree, buildStoryRef } =
    await import("@/shared/lib/pipeline-graph/test-fixtures/derived-tree");
  const bKey = "acme/plans:other";
  const bView = "team:acme/plans/other";
  const postB = vi.fn();
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  let subscriptionId = "";
  let bRev = 0;
  const subscriber = new LiveHub({
    read: async (topic, viewKey, _since, options) => {
      subscriptionId = options!.subscriptionId;
      return { topic, viewKey, rev: 0, snapshot: [] };
    },
    listen: async (event, callback) => {
      listeners.set(event, callback);
      return () => {
        listeners.delete(event);
      };
    },
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  const b = subscriber.attach("B", bView, postB, true, true);
  await b.subscribe("stories", undefined, { viewKey: bView });
  const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
  const original = internals.invoke.bind(internals);
  const title = "A initial";
  const { useStatusTaxonomyStore } = await import("@/shared/stores/status-taxonomy-store");
  const originalTaxonomy = useStatusTaxonomyStore.getState().states;
  const taxonomyOf = (label: string) =>
    originalTaxonomy.map((state) => (state.id === "todo" ? { ...state, label } : state));
  const published: Array<{ teamKey: string; rows: Array<Record<string, unknown>> }> = [];
  internals.invoke = async (command, args) => {
    if (command === "load_status_taxonomy")
      return {
        states: taxonomyOf(args?.team === "other" ? "B taxonomy" : "A taxonomy"),
        warnings: [],
      };
    if (command === "scan_pipeline_tree")
      return buildTree("content", {
        absoluteDirPath: root,
        stories: [
          {
            ...buildStoryRef("A.md", {
              absolutePath: `${root}/A.md`,
              storyId: "A-1",
              status: "todo",
            }),
            openTags: { title: [title] },
          },
        ],
      });
    if (command === "nav_model_publish") {
      const publication = args as unknown as (typeof published)[number];
      published.push(publication);
      if (publication.teamKey === bKey)
        listeners.get("codesense:subscription-delta")?.({
          payload: {
            subscriptionId,
            baseRev: bRev,
            delta: {
              topic: "stories",
              viewKey: bView,
              rev: ++bRev,
              upserts: publication.rows,
              removes: [],
            },
          },
        });
      return null;
    }
    return original(command, args);
  };
  useOrgStore.setState({
    orgs: [{ ...org, teams: [...org.teams, { name: "other", source_repos: [] }] }],
  });
  useViewerStore.getState().setEditingDirtyPaths(key, [`${root}/A.md`]);
  const view = render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  try {
    await waitFor(() =>
      expect(
        published.some((p) => p.teamKey === key && p.rows.some((r) => r.title === title)),
      ).toBe(true),
    );
    await act(async () => {
      useOrgStore.setState({ activeTeamName: "other" });
    });
    expect(postB).not.toHaveBeenCalled();
    expect(published.filter((p) => p.teamKey === bKey)).toEqual([]);
    // App hydrates the singleton for the newly active team B; A keeps its own taxonomy.
    act(() => useStatusTaxonomyStore.getState().setStates(taxonomyOf("B taxonomy")));
    await act(async () => {
      await Promise.resolve();
    });
    const aRows = published.filter((p) => p.teamKey === key).flatMap((p) => p.rows);
    expect(aRows.some((r) => r.storyId === "A-1" && r.label === "A taxonomy")).toBe(true);
    expect(aRows.some((r) => r.label === "B taxonomy")).toBe(false);
    expect(postB).not.toHaveBeenCalled();
    expect(published.filter((p) => p.teamKey === bKey)).toEqual([]);
  } finally {
    view.unmount();
    useStatusTaxonomyStore.getState().setStates(originalTaxonomy);
    useViewerStore.getState().setEditingDirtyPaths(key, []);
    b.dispose();
    subscriber.dispose();
  }
});

it("keeps team merge marks in the shared files projection when the native Source switches to Private", async () => {
  const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
  const original = internals.invoke.bind(internals);
  const publications: Array<{ teamKey: string; files: { mergePaths: string[] } }> = [];
  internals.invoke = async (command, args) => {
    if (command === "list_content_merge_marks") return ["settings.json"];
    if (command === "nav_model_publish") {
      publications.push(args as unknown as (typeof publications)[number]);
      return null;
    }
    return original(command, args);
  };
  const view = render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={key} />
    </TooltipProvider>,
  );
  try {
    await waitFor(() =>
      expect(publications.some((p) => p.files.mergePaths.includes("settings.json"))).toBe(true),
    );
    const before = publications.length;
    selectPrivate();
    await screen.findByText("secret");
    await act(async () => {
      await Promise.resolve();
    });
    // A Private switch never overwrites the team-wide file metadata.
    expect(publications.slice(before).filter((p) => p.files.mergePaths.length === 0)).toEqual([]);
  } finally {
    view.unmount();
    internals.invoke = original;
  }
});

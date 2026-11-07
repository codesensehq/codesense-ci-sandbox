// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@/test-utils";

import { NavFileActionsOwner } from "./nav-file-actions-owner";
import { openNativeMenu } from "@/features/shell/native-menu-openers";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useOrgStore } from "@/shared/stores/org-store";
import { useScratchTeamUseStore } from "@/shared/stores/scratch-team-use-store";

// SW-VE-6: the file-lifecycle context menu. An OWN-scope file exposes the
// mutating gestures (New file / Move to / Rename / Delete); an INHERITED
// (parent-owned) file is read-only — every mutating item is suppressed and
// replaced with the "owned upstream" note (closing SW-CP-14's ungated gap).

vi.mock("@/shared/lib/send-to-terminal", () => ({ sendTextToTerminal: vi.fn() }));

const mockStatusNavExpandedPaths: Record<string, string[]> = {};
vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "/repo",
  getActiveTeamKey: () => "/repo",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        navLens: {},
        statusNavExpandedPaths: mockStatusNavExpandedPaths,
        toggleStatusNavExpanded: vi.fn(),
      }),
    {
      subscribe: () => () => {},
      getState: () => ({
        toggleStatusNavExpanded: vi.fn(),
        readViewState: () => undefined,
        captureViewState: () => {},
        // A successful move re-points open tabs at the new path (#2067).
        remapTabPath: vi.fn(),
      }),
    },
  ),
}));

const {
  createScratchFile,
  createFile,
  moveContentFile,
  openManagementTab,
  getScratchTeamUse,
  toastSuccess,
} = vi.hoisted(() => ({
  getScratchTeamUse: vi.fn(async () => true),
  toastSuccess: vi.fn(),
  createScratchFile: vi.fn(async () => "/scratch/orgs/alderco/content/layers/idea.md"),
  createFile: vi.fn(async () => undefined),
  moveContentFile: vi.fn(async () => undefined),
  openManagementTab: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    listDirectory: () => Promise.resolve([]),
    createScratchFile,
    createFile,
    moveContentFile,
    getScratchTeamUse,
  };
});
vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab }));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: toastSuccess, error: vi.fn() }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  // Active team `default`; `platform` is its parent (inherited → read-only).
  useOrgStore.setState({ activeOrgSlug: "alderco", activeTeamName: "default" });
  useScratchTeamUseStore.setState({ byTeam: {}, saving: {} });
  getScratchTeamUse.mockResolvedValue(true);
});

function renderNav(expanded: string[] = ["/org/default/content/layers"], hosted = false) {
  mockStatusNavExpandedPaths["/repo"] = expanded;
  const tree = buildTree("root", {
    absoluteDirPath: "/org/default/content",
    subEpics: [
      buildTree("layers", {
        dirPath: "layers",
        absoluteDirPath: "/org/default/content/layers",
        docs: [
          // SW-XS-20: own files carry no sourceTeam; this folder mixes own files and
          // a parent team's file, which lives at its real path in the parent.
          { fileName: "own-ws.md", filePath: "/org/default/content/layers/own-ws.md" },
          {
            fileName: "override.md",
            filePath: "/org/default/content/layers/override.md",
            overridesTeam: "platform",
          },
          {
            fileName: "inherited.md",
            filePath: "/org/platform/content/layers/inherited.md",
            sourceTeam: "platform",
          },
        ],
      }),
      // A folder only the parent team has: it sits at the team's place, and its
      // file is the parent's. Not expanded, so it only feeds the Move-to picker.
      buildTree("parent-dir", {
        dirPath: "parent-dir",
        absoluteDirPath: "/org/default/content/parent-dir",
        docs: [
          {
            fileName: "parent-doc.md",
            filePath: "/org/platform/content/parent-dir/parent-doc.md",
            sourceTeam: "platform",
          },
        ],
      }),
    ],
  });
  return render(
    <NavFileActionsOwner
      renderNative={hosted ? () => null : undefined}
      tree={tree}
      repoPath="/repo"
      selectedPath={null}
      onFileSelect={vi.fn()}
      onRelateDrop={vi.fn()}
      provenance={{ glyphMap: new Map(), isLightTheme: false }}
    />,
  );
}

function openMenuFor(label: string) {
  const row = screen.getByText(label).closest('[role="listitem"]')!;
  fireEvent.contextMenu(row);
}

describe("PipelineStatusNav — SW-VE-6 lifecycle menu", () => {
  it("exposes the full mutating group on an OWN-scope file", () => {
    renderNav();
    openMenuFor("own-ws");
    expect(screen.getByText(/New file/)).toBeInTheDocument();
    expect(screen.getByText(/Move to/)).toBeInTheDocument();
    expect(screen.getByText(/Rename/)).toBeInTheDocument();
    expect(screen.getByText(/^Delete$/)).toBeInTheDocument();
    // never a false read-only note on an own-scope file
    expect(screen.queryByText(/Read-only/)).toBeNull();
  });

  it("keeps every gesture on an INHERITED file, under a note naming its team (SW-XS-20)", () => {
    renderNav();
    openMenuFor("inherited");
    expect(screen.getByTestId("nav-menu-inherited-note")).toHaveTextContent(
      "From platform · edits save there",
    );
    expect(screen.getByText("Copy into default")).toBeInTheDocument();
    expect(screen.getByText(/New file/)).toBeInTheDocument();
    expect(screen.getByText(/Move to/)).toBeInTheDocument();
    expect(screen.getByText(/Rename/)).toBeInTheDocument();
    expect(screen.getByText(/^Delete$/)).toBeInTheDocument();
    expect(screen.queryByText(/Read-only/)).toBeNull();
  });

  it("opens the New file prompt when New file… is clicked (own scope)", () => {
    renderNav();
    openMenuFor("own-ws");
    fireEvent.click(screen.getByText(/New file/));
    expect(screen.getByText(/Create a new empty file/)).toBeInTheDocument();
  });

  it("SLW-PT-21: New file asks Who can use it — the team by default", () => {
    renderNav();
    openMenuFor("own-ws");
    fireEvent.click(screen.getByText(/New file/));
    const who = screen.getByRole("combobox", { name: "Who can use it" });
    expect(who).toHaveValue("team");
    expect(screen.getByRole("option", { name: "default" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Only me" })).toBeInTheDocument();
  });

  it("SLW-PT-21: an Only me file is created in the scratch space, never in the team", async () => {
    renderNav();
    openMenuFor("own-ws");
    fireEvent.click(screen.getByText(/New file/));
    fireEvent.change(screen.getByRole("combobox", { name: "Who can use it" }), {
      target: { value: "only-me" },
    });
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "idea.md" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    await vi.waitFor(() =>
      expect(createScratchFile).toHaveBeenCalledWith(
        "alderco",
        "default",
        "/org/default/content/layers",
        "idea.md",
      ),
    );
    expect(createFile).not.toHaveBeenCalled();
    await vi.waitFor(() =>
      expect(openManagementTab).toHaveBeenCalledWith(
        "/scratch/orgs/alderco/content/layers/idea.md",
      ),
    );
  });

  it("SLW-PT-21: when the team's private items are off, Only me says it won't apply there", async () => {
    getScratchTeamUse.mockResolvedValue(false);
    renderNav();
    openMenuFor("own-ws");
    fireEvent.click(screen.getByText(/New file/));
    fireEvent.change(screen.getByRole("combobox", { name: "Who can use it" }), {
      target: { value: "only-me" },
    });
    await vi.waitFor(() =>
      expect(screen.getByTestId("who-can-use-it-hint")).toHaveTextContent(
        "private items are off for default, so it won't apply there",
      ),
    );
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "idea.md" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    await vi.waitFor(() =>
      expect(toastSuccess).toHaveBeenCalledWith(
        expect.stringContaining("private items are off for default, so it won't apply there"),
      ),
    );
  });

  it("moves an own file into a parent-only folder without a confirm (it lands in the team)", async () => {
    renderNav();
    openMenuFor("own-ws");
    fireEvent.click(screen.getByText(/Move to/));
    // drive the picker via its input (the dir name also appears as a nav folder
    // header, so a bare text-click is ambiguous).
    const input = screen.getByPlaceholderText("Move to folder…");
    fireEvent.change(input, { target: { value: "parent-dir" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await vi.waitFor(() =>
      expect(moveContentFile).toHaveBeenCalledWith(
        "/org/default/content/layers/own-ws.md",
        "/org/default/content/parent-dir/own-ws.md",
      ),
    );
    expect(screen.queryByText(/for every team that inherits it/)).toBeNull();
  });
});

// SW-XS-20: the confirms for a parent team's file name that team and the reach
// of the change; deleting an own file that overrides one says it comes back.
describe("PipelineStatusNav — SW-XS-20 confirm wording", () => {
  it("deleting an inherited file: every team that inherits it loses it", () => {
    renderNav();
    openMenuFor("inherited");
    fireEvent.click(screen.getByText(/^Delete$/));
    expect(
      screen.getByText(/Delete from platform\? Every team that inherits it loses it too\./),
    ).toBeInTheDocument();
  });

  it("renaming an inherited file renames it in the parent", () => {
    renderNav();
    openMenuFor("inherited");
    fireEvent.click(screen.getByText(/Rename/));
    expect(
      screen.getByText("This renames it in platform for every team that inherits it."),
    ).toBeInTheDocument();
  });

  it("moving an inherited file moves it in the parent, after a confirm", () => {
    renderNav();
    openMenuFor("inherited");
    fireEvent.click(screen.getByText(/Move to/));
    const input = screen.getByPlaceholderText("Move to folder…");
    fireEvent.change(input, { target: { value: "parent-dir" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(
      screen.getByText("This moves it in platform for every team that inherits it."),
    ).toBeInTheDocument();
    expect(moveContentFile).not.toHaveBeenCalled();
  });

  it("deleting an own file that overrides a parent's: the parent version shows instead", () => {
    renderNav();
    openMenuFor("override");
    fireEvent.click(screen.getByText(/^Delete$/));
    expect(screen.getByText(/The platform version will show here instead\./)).toBeInTheDocument();
  });
});

// SW-VE-7: drag-drop. Directory rows are marked for OS-drop hit-testing and act
// as internal-drag MOVE targets (reusing VE-6's cross-scope warning); the
// story-relation drag path stays intact (separate channel).
describe("PipelineStatusNav — SW-VE-7 drag-drop ingest", () => {
  it("marks directory rows with data-nav-drop-dir for OS-drop hit-testing (AC-2)", () => {
    const { container } = renderNav();
    const marked = Array.from(container.querySelectorAll("[data-nav-drop-dir]")).map((el) =>
      el.getAttribute("data-nav-drop-dir"),
    );
    // both folder-header dirs are drop targets; no file row is marked
    expect(marked).toContain("/org/default/content/layers");
    expect(marked).toContain("/org/default/content/parent-dir");
    expect(marked).not.toContain("/org/default/content/layers/own-ws.md");
  });

  it("an internal file drag onto a directory row triggers a MOVE, not a relate (AC-3/AC-6)", async () => {
    renderNav();
    const dt = {
      setData: vi.fn(),
      getData: vi.fn().mockReturnValue("/org/default/content/layers/own-ws.md"),
      effectAllowed: "",
      dropEffect: "",
    };
    const sourceRow = screen.getByText("own-ws").closest('[role="listitem"]')!;
    const destDirRow = screen.getByText("parent-dir").closest('[role="listitem"]')!;
    // start dragging the file (sets the shared nav drag ref), then drop on the
    // directory row → the MOVE path (into the team's copy of that folder).
    fireEvent.dragStart(sourceRow, { dataTransfer: dt });
    fireEvent.dragOver(destDirRow, { dataTransfer: dt });
    fireEvent.drop(destDirRow, { dataTransfer: dt });
    await vi.waitFor(() =>
      expect(moveContentFile).toHaveBeenCalledWith(
        "/org/default/content/layers/own-ws.md",
        "/org/default/content/parent-dir/own-ws.md",
      ),
    );
  });
});

// SW-XS-20: a file inherited from a parent team is not a drag source (a drop on a
// folder would move the parent's file into a folder the parent may not have).
describe("PipelineStatusNav — SW-XS-20 inherited files are not dragged", () => {
  it("makes an own file draggable but not an inherited one", () => {
    renderNav();
    const own = screen.getByText("own-ws").closest('[role="listitem"]')!;
    const inherited = screen.getByText("inherited").closest('[role="listitem"]')!;
    expect(own.closest('[draggable="true"]')).not.toBeNull();
    expect(inherited.closest('[draggable="true"]')).toBeNull();
  });

  it("marks an own file that overrides a parent's file", () => {
    renderNav();
    const row = screen.getByText("override").closest('[role="listitem"]')!;
    expect(row.querySelector('[data-testid="shadowed-badge"]')?.getAttribute("aria-label")).toBe(
      "Overrides platform's version",
    );
    expect(row.querySelector('[data-testid="scope-badge-inherited"]')).toBeNull();
  });
});

it("keeps the native owner and inherited Delete dialog when a dashboard fills the slot", async () => {
  renderNav(undefined, true);
  expect(screen.queryByText("own-ws")).toBeNull();
  act(() => {
    openNativeMenu({
      action: "delete",
      teamKey: "/repo",
      row: { kind: "file", path: "/org/platform/content/layers/inherited.md" },
      contentRoot: "/builtin-panes",
      anchorRect: { x: 0, y: 0, width: 10, height: 24 },
      owner: {},
      onClose: vi.fn(),
    });
  });
  await waitFor(() => expect(screen.getByText("Delete file?")).toBeInTheDocument());
  expect(screen.getByText(/platform/)).toBeInTheDocument();
});

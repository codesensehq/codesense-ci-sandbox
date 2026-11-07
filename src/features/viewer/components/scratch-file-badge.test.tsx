// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@/test-utils";

import type { FileEntry, ScratchOverlayFile } from "@/tauri/commands";
import type { Workspace } from "@/shared/stores/viewer-store";

// SLW-PT-20 AC-3 seam: the REAL inset + REAL worktree tree, with only the Tauri
// commands stubbed. What the backend reports the scratch overlay put in the worktree
// is what the tree rows and the open file's header show; "Use team's" calls the
// backend for this worktree and the view follows the re-read.

const listDirectoryMock = vi.fn<(p: string) => Promise<FileEntry[]>>();
const listScratchMock = vi.fn<(cwd: string) => Promise<ScratchOverlayFile[]>>();
const setUseTeamMock = vi.fn<(cwd: string, logical: string, useTeam: boolean) => Promise<void>>();
const moveMock = vi.fn(async (_p: unknown) => ({ kind: "moved" as const }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  listDirectory: (p: string) => listDirectoryMock(p),
  readFile: () => Promise.resolve("# team rules\n"),
  writeWorktreeFile: () => Promise.resolve(),
  lazyAttachWorktreeWatch: () => Promise.resolve(1),
  detachWorktreeWatch: () => Promise.resolve(),
  detectTools: () =>
    Promise.resolve({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
  listScratchOverlayFiles: (cwd: string) => listScratchMock(cwd),
  setScratchFileUseTeam: (cwd: string, logical: string, useTeam: boolean) =>
    setUseTeamMock(cwd, logical, useTeam),
  listScratchSpace: () => Promise.resolve({ orgSlug: null, exists: false, files: [], allOrgs: [] }),
  moveContentBetweenContexts: (p: unknown) => moveMock(p),
}));

import { SourceEditorInset } from "./source-editor-inset";
import { scratchBadgeLabel } from "./scratch-file-badge";

const ws: Workspace = {
  id: "w1",
  kind: "story",
  cwd: "/wt",
  status: "ready",
  storyId: "SLW-PT-20",
  storyFilePath: "/repo/story.md",
  sourceRepoPath: "/repo",
};

function file(name: string): FileEntry {
  return { name, path: `/wt/${name}`, is_dir: false, extension: "md", size: 12 };
}

const replacing: ScratchOverlayFile = {
  path: "CLAUDE.md",
  logical: "CLAUDE.md",
  origin: "scratch",
  replacesTeam: true,
  usingTeam: false,
  orgSlug: "acme/ctx",
  team: "default",
  inertKind: null,
};
const mineOnly: ScratchOverlayFile = {
  path: "notes.md",
  logical: "notes.md",
  origin: "scratch",
  replacesTeam: false,
  usingTeam: false,
  orgSlug: "acme/ctx",
  team: "default",
  inertKind: null,
};
const carried: ScratchOverlayFile = {
  path: "prompt.md",
  logical: "prompts/prompt.md",
  origin: "allOrgs",
  replacesTeam: false,
  usingTeam: false,
  orgSlug: "acme/ctx",
  team: "default",
  inertKind: null,
};

beforeEach(() => {
  listDirectoryMock.mockReset();
  listDirectoryMock.mockResolvedValue([
    file("CLAUDE.md"),
    file("notes.md"),
    file("prompt.md"),
    file("README.md"),
  ]);
  listScratchMock.mockReset();
  listScratchMock.mockResolvedValue([replacing, mineOnly, carried]);
  setUseTeamMock.mockReset();
  setUseTeamMock.mockResolvedValue(undefined);
});

describe("From private badge (SLW-PT-20)", () => {
  it("labels each overlay kind in the product's words", () => {
    expect(scratchBadgeLabel(mineOnly)).toBe("From private");
    expect(scratchBadgeLabel(carried)).toBe("From private · all orgs");
    expect(scratchBadgeLabel(replacing)).toBe("From private · replaces team's");
  });

  it("badges exactly the tree rows the overlay put in this worktree", async () => {
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    const row = (name: string) => screen.findByTestId(`worktree-row-/wt/${name}`);
    await waitFor(() => expect(listScratchMock).toHaveBeenCalledWith("/wt"));
    const claude = await row("CLAUDE.md");
    await waitFor(() =>
      expect(within(claude).getByTestId("scratch-tree-badge")).toHaveAttribute(
        "aria-label",
        "From private · replaces team's",
      ),
    );
    expect(within(await row("notes.md")).getByTestId("scratch-tree-badge")).toHaveAttribute(
      "aria-label",
      "From private",
    );
    expect(within(await row("prompt.md")).getByTestId("scratch-tree-badge")).toHaveAttribute(
      "aria-label",
      "From private · all orgs",
    );
    expect(within(await row("README.md")).queryByTestId("scratch-tree-badge")).toBeNull();
  });

  it("the open file's header carries the badge, and Use team's swaps the team's file back in", async () => {
    render(<SourceEditorInset workspace={ws} seedRelPath="CLAUDE.md" onClose={vi.fn()} />);
    const header = await screen.findByTestId("scratch-header-badge");
    expect(header).toHaveTextContent("From private · replaces team's");

    // After the switch the backend reports the team's file in place, scratch copy set aside.
    listScratchMock.mockResolvedValue([{ ...replacing, usingTeam: true }, mineOnly, carried]);
    fireEvent.click(screen.getByTestId("scratch-use-team"));
    await waitFor(() => expect(setUseTeamMock).toHaveBeenCalledWith("/wt", "CLAUDE.md", true));
    await waitFor(() => expect(screen.getByTestId("scratch-use-mine")).toBeInTheDocument());
    expect(screen.getByTestId("scratch-header-badge")).toHaveTextContent(
      "Team's file · your private copy is set aside",
    );
    const claudeRow = await screen.findByTestId("worktree-row-/wt/CLAUDE.md");
    expect(within(claudeRow).queryByTestId("scratch-tree-badge")).toBeNull();

    // And back: "Use mine" restores the scratch file for this team.
    fireEvent.click(screen.getByTestId("scratch-use-mine"));
    await waitFor(() => expect(setUseTeamMock).toHaveBeenCalledWith("/wt", "CLAUDE.md", false));
  });

  it("a file the team does not have offers no Use team's", async () => {
    render(<SourceEditorInset workspace={ws} seedRelPath="notes.md" onClose={vi.fn()} />);
    const header = await screen.findByTestId("scratch-header-badge");
    expect(header).toHaveTextContent("From private");
    expect(screen.queryByTestId("scratch-use-team")).toBeNull();
  });

  it("a From private row's right-click menu acts in the worktree's own org (SLW-PT-22)", async () => {
    const { useLocalProfileStore } = await import("@/shared/stores/local-profile-store");
    useLocalProfileStore.setState({
      initialized: true,
      status: {
        active: { principal: "42", displayName: "Ada", machineOwner: null },
        profiles: [],
        privateAuthority: null,
      },
    } as never);
    listScratchMock.mockResolvedValue([
      { ...mineOnly, logical: "prompts/notes.md", inertKind: "prompt" },
      carried,
    ]);
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    const row = await screen.findByTestId("worktree-row-/wt/notes.md");
    await waitFor(() => expect(within(row).getByTestId("scratch-tree-badge")).toBeInTheDocument());
    fireEvent.contextMenu(row);
    expect(await screen.findByText("Share with default…")).toBeInTheDocument();
    expect(screen.getByText("Copy to another org…")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Use in all orgs"));
    await waitFor(() => expect(moveMock).toHaveBeenCalledTimes(1));
    expect(moveMock.mock.calls[0][0]).toMatchObject({
      from: { kind: "personal", orgSlug: "acme/ctx" },
      to: { kind: "scratchAllOrgs" },
      logicalName: "prompts/notes.md",
    });
  });
});

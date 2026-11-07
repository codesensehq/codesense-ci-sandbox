// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { selectActiveTerminalTab, selectWorkspaceTabs, useViewerStore } from "./viewer-store";
import type { Workspace } from "./viewer-store";
import { useOrgStore } from "./org-store";
import { buildRootWorkspaceId } from "@/shared/lib/team-key";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

// T.2: per-source-repo root id (replaces legacy ROOT_WORKSPACE_ID = "root").
const ROOT_WORKSPACE_ID = buildRootWorkspaceId("test/repo");

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

const repo = "acme:default";

installTerminalLayoutHarness();

const alive = (...sessions: string[]) => sessions.map((id) => terminalHost().contains(id));

function reset() {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  vi.clearAllMocks();
  useViewerStore.setState({
    workspaces: {
      [repo]: [
        { id: ROOT_WORKSPACE_ID, kind: "root", cwd: "", status: "ready" },
        {
          id: "30-10",
          kind: "story",
          cwd: "/wt/30-10",
          status: "ready",
          storyTitle: "Story 30.10",
        } as Workspace,
      ],
    },
    activeWorkspace: { [repo]: "30-10" },
    softClosedWorkspaces: {},
  });
}

// PT-10: workspace-× is "close, keep running" — no confirm dialog. requestClose
// is now a thin pass-through to closeWorkspace + the kind guard.
describe("requestCloseWorkspace (PT-10 pass-through)", () => {
  beforeEach(reset);

  it("AC-9/16: closes silently with no shadow when there are no terminals", () => {
    useViewerStore.getState().requestCloseWorkspace(repo, "30-10");

    expect(
      useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10"),
    ).toBeUndefined();
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeUndefined();
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe(ROOT_WORKSPACE_ID);
  });

  it("AC-1/15/6: keeps PTYs alive and populates the shadow when terminals are live", () => {
    seedTerminalTabs(repo, "30-10", ["s1", "s2"]);

    useViewerStore.getState().requestCloseWorkspace(repo, "30-10");

    // Workspace leaves the visible bar...
    expect(
      useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10"),
    ).toBeUndefined();
    // ...but its terminals keep running in their host (override of SW-EW-1 AC-8)...
    expect(alive("s1", "s2")).toEqual([true, true]);
    // ...and the shadow retains the full record.
    const shadow = useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"];
    expect(shadow?.workspace.id).toBe("30-10");
  });

  it("closes the root workspace too (root is now user-closable — SW-GS-2a, 2026-06-24)", () => {
    useViewerStore.getState().requestCloseWorkspace(repo, ROOT_WORKSPACE_ID);
    expect(
      useViewerStore.getState().workspaces[repo].find((w) => w.id === ROOT_WORKSPACE_ID),
    ).toBeUndefined();
  });
});

describe("reopenSoftClosedWorkspace (PT-10 AC-4/AC-17)", () => {
  beforeEach(reset);

  it("restores the record + tab list, clears the shadow, spawns no new PTY", () => {
    seedTerminalTabs(repo, "30-10", ["s1", "s2", "s3"]);
    useViewerStore.getState().closeWorkspace(repo, "30-10");
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeDefined();

    useViewerStore.getState().reopenSoftClosedWorkspace(repo, "30-10");

    // Tabs restored verbatim (the survived PTYs reattach on remount).
    expect(
      selectWorkspaceTabs(useViewerStore.getState(), repo, "30-10").map((t) => t.sessionId),
    ).toEqual(["s1", "s2", "s3"]);
    // Record back in the bar + focused; shadow cleared.
    expect(useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10")).toBeDefined();
    expect(useViewerStore.getState().activeWorkspace[repo]).toBe("30-10");
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeUndefined();
    // No terminal ended across the close→reopen round-trip.
    expect(alive("s1", "s2", "s3")).toEqual([true, true, true]);
    expect(terminalHost().opened).toHaveLength(0);
  });

  it("restores the active-tab selection (faithful round-trip)", () => {
    seedTerminalTabs(repo, "30-10", ["s1", "s2"], { active: "s2" });
    useViewerStore.getState().closeWorkspace(repo, "30-10");
    useViewerStore.getState().reopenSoftClosedWorkspace(repo, "30-10");

    expect(selectActiveTerminalTab(useViewerStore.getState(), repo, "30-10")).toBe("s2");
  });

  it("no-ops when there is no shadow entry", () => {
    expect(() => useViewerStore.getState().reopenSoftClosedWorkspace(repo, "nope")).not.toThrow();
  });
});

describe("reopenSoftClosedWorkspace — SW-FX-35 tray pair coherence (AC-3)", () => {
  beforeEach(reset);

  it("restores into a visible tray even when layoutMode drifted to 'viewer' while minimized", () => {
    seedTerminalTabs(repo, "30-10", ["s1"]);
    useViewerStore.setState({
      layoutMode: "shared",
      lastTerminalMode: "shared",
    });
    useViewerStore.getState().closeWorkspace(repo, "30-10"); // minimize
    // User switches to the file viewer while the workspace is minimized —
    // closeWorkspace leaves layoutMode untouched, so this is the cold case.
    useViewerStore.setState({ layoutMode: "viewer" });

    useViewerStore.getState().reopenSoftClosedWorkspace(repo, "30-10");

    const s = useViewerStore.getState();
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).not.toBe("viewer");
  });

  it("AC-4: restore respects lastTerminalMode === 'console'", () => {
    seedTerminalTabs(repo, "30-10", ["s1"]);
    useViewerStore.setState({
      layoutMode: "viewer",
      lastTerminalMode: "console",
    });
    useViewerStore.getState().closeWorkspace(repo, "30-10");
    useViewerStore.getState().reopenSoftClosedWorkspace(repo, "30-10");
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });
});

describe("restore → remove-tile round-trip (PT-13 AC-6)", () => {
  beforeEach(reset);

  it("minimize → restore → remove tile (minimize again) keeps PTYs alive, never kills", () => {
    seedTerminalTabs(repo, "30-10", ["s1", "s2"]);
    // Minimize, then restore-by-drag (same action as click): the record returns
    // to the foreground set so its tile mounts and reattaches.
    useViewerStore.getState().closeWorkspace(repo, "30-10");
    useViewerStore.getState().reopenSoftClosedWorkspace(repo, "30-10");
    expect(useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10")).toBeDefined();
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeUndefined();

    // Removing the restored tile = EW-11's onCloseTile → requestCloseWorkspace →
    // closeWorkspace (minimize): back to a dimmed card, PTYs survive, no kill.
    useViewerStore.getState().closeWorkspace(repo, "30-10");
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeDefined();
    expect(alive("s1", "s2")).toEqual([true, true]);
  });
});

describe("killTerminalsForSoftClosedWorkspace (PT-10 AC-7)", () => {
  beforeEach(reset);

  it("ends every minimized terminal in its host then drops the shadow entry", () => {
    seedTerminalTabs(repo, "30-10", ["s1", "s2"]);
    useViewerStore.getState().closeWorkspace(repo, "30-10");

    useViewerStore.getState().killTerminalsForSoftClosedWorkspace(repo, "30-10");

    expect(alive("s1", "s2")).toEqual([false, false]);
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeUndefined();
  });
});

describe("closeWorkspace hard-close (PT-10 AC-21)", () => {
  beforeEach(reset);

  it("killTerminals option ends the terminals, closes, and creates NO shadow", () => {
    seedTerminalTabs(repo, "30-10", ["s1", "s2"]);

    useViewerStore.getState().closeWorkspace(repo, "30-10", { killTerminals: true });

    expect(alive("s1", "s2")).toEqual([false, false]);
    expect(
      useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10"),
    ).toBeUndefined();
    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeUndefined();
  });
});

describe("soft-close invariants (PT-10 AC-14/AC-19)", () => {
  beforeEach(reset);

  it("AC-14: a closed-but-running workspace stays closed (never auto-reopens) until an explicit reopen", () => {
    seedTerminalTabs(repo, "30-10", ["s1"]);
    useViewerStore.getState().closeWorkspace(repo, "30-10");

    // No mechanism re-adds it implicitly — switching workspaces, re-reading
    // state, etc. leave the shadow intact and the workspace out of the bar.
    useViewerStore.getState().switchWorkspace(repo, ROOT_WORKSPACE_ID);

    expect(useViewerStore.getState().softClosedWorkspaces[repo]?.["30-10"]).toBeDefined();
    expect(
      useViewerStore.getState().workspaces[repo].find((w) => w.id === "30-10"),
    ).toBeUndefined();
  });

  it("AC-19: the shadow slice is untouched by unrelated actions (no spurious entries)", () => {
    // A non-close action (e.g. an auth-overlay-driven switch) must not create
    // or mutate shadow entries — only close/reopen/kill touch the slice.
    useViewerStore.getState().switchWorkspace(repo, ROOT_WORKSPACE_ID);
    expect(useViewerStore.getState().softClosedWorkspaces[repo]).toBeUndefined();
  });
});

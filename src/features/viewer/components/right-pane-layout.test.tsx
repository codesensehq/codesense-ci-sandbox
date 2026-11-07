// SPDX-License-Identifier: MIT
import { render, screen } from "@/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  defaultRightPaneLayout,
  migrateLegacyRightPaneState,
  reconcileLayoutToBuiltins,
  INBOX_PANEL_ID,
  MAIN_SECTION_ID,
  SCRATCH_SECTION_ID,
  type RightPaneLayout as PaneLayout,
} from "@/features/shell/right-pane/right-pane-layout";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { registerPanel } from "@/features/shell/right-pane/panel-registry";
import { Inbox } from "lucide-react";
import { RightPaneLayout } from "./right-pane-layout";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  checkPathExists: vi.fn().mockResolvedValue(true),
  detectTools: vi
    .fn()
    .mockResolvedValue({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
  // SW-WA-E14: the Activity board now rides in the default layout; its chrome badge
  // + panel read the attention board via these — stub them so the host render is inert.
  listAttentionBoard: vi
    .fn()
    .mockResolvedValue({ youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] }),
  // The needs-you count reads the same merged YOU-OWE list (incl. pending approvals).
  wvListPendingApprovals: vi.fn().mockResolvedValue([]),
  getLeaseStatus: vi.fn().mockResolvedValue({ held: false }),
}));

vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => <div data-testid="scratch-note-panel">Scratch</div>,
}));

const defaultProps = { repoPath: "/test-repo", onClose: vi.fn() };

beforeEach(() => {
  vi.clearAllMocks();
  registerPanel({
    id: INBOX_PANEL_ID,
    title: "Inbox",
    icon: Inbox,
    render: () => <div data-testid="inbox-pane">Inbox</div>,
  });
  // A configured user (≥1 org) on the normal default (landing on Activity).
  useOrgStore.setState({ orgs: [{ slug: "acme" } as unknown as Org] });
  useViewerStore.setState({ rightPaneLayout: defaultRightPaneLayout() });
});

describe("RightPaneLayout (SW-RP-1 host)", () => {
  it("renders the Live section (Inbox) over Notes, with no retired Pipeline/Board tabs", () => {
    render(<RightPaneLayout {...defaultProps} />);
    // Inbox is the active/visible body; the retired tabs are gone.
    expect(screen.getByRole("button", { name: /Inbox/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Pipeline/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Board/ })).toBeNull();
    expect(screen.getByTestId("inbox-pane")).toBeInTheDocument();
    expect(screen.getByTestId("scratch-note-panel")).toBeInTheDocument();
    // SW-LN-B40: no truncated flow strip above the tabs.
    expect(screen.queryByTestId("right-pane-flow-strip")).toBeNull();
  });

  it("has no per-section close button (SW-RP-2: pane closes via ⌘J / View → Panels)", () => {
    render(<RightPaneLayout {...defaultProps} />);
    expect(screen.queryByLabelText("Close right pane")).toBeNull();
  });
});

// The seam is the host a fresh profile mounts: nothing configured, the store at its boot
// default.
describe("RightPaneLayout first run", () => {
  const sectionOf = (layout: PaneLayout, id: string) => layout.sections.find((s) => s.id === id)!;

  beforeEach(() => {
    // A fresh profile: no orgs, the boot default layout.
    useOrgStore.setState({ orgs: [] });
    useViewerStore.setState({ rightPaneLayout: defaultRightPaneLayout() });
  });

  it("a fresh profile opens on the Inbox over Notes", () => {
    render(<RightPaneLayout {...defaultProps} />);

    const layout = useViewerStore.getState().rightPaneLayout;
    expect(layout.sections.map((s) => s.id)).toEqual([MAIN_SECTION_ID, SCRATCH_SECTION_ID]);
    expect(sectionOf(layout, MAIN_SECTION_ID).collapsed).toBe(false);
    expect(sectionOf(layout, SCRATCH_SECTION_ID).collapsed).toBe(false);
    expect(screen.getByTestId("inbox-pane")).toBeVisible();
    expect(screen.getByTestId("scratch-note-panel")).toBeInTheDocument();
    expect(screen.queryByText("Setup")).toBeNull();
  });

  it("a returning user's saved layout is respected, even with nothing configured", () => {
    // An arrangement the user chose: Live collapsed, Notes open.
    const saved = defaultRightPaneLayout();
    sectionOf(saved, MAIN_SECTION_ID).collapsed = true;
    const snapshot = JSON.parse(JSON.stringify(saved));
    useViewerStore.setState({ rightPaneLayout: saved });

    render(<RightPaneLayout {...defaultProps} />);

    expect(useViewerStore.getState().rightPaneLayout).toEqual(snapshot);
    expect(screen.getByTestId("scratch-note-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("inbox-pane")).toBeNull();
  });

  it("a saved layout naming retired panels renders no ghost tab", () => {
    useViewerStore.setState({
      rightPaneLayout: {
        sections: [
          {
            id: MAIN_SECTION_ID,
            group: {
              panelIds: ["pipeline", "running-agents", "board", "weft-board"],
              activePanelId: "running-agents",
            },
            collapsed: false,
            size: 0.6,
          },
          {
            id: "rps-setup",
            label: "Setup",
            group: { panelIds: ["triggers", "workflow-library"], activePanelId: "triggers" },
            collapsed: false,
            size: 0.4,
          },
        ],
      },
    });

    useViewerStore.setState({
      rightPaneLayout: reconcileLayoutToBuiltins(
        migrateLegacyRightPaneState({ rightPaneLayout: useViewerStore.getState().rightPaneLayout }),
      ),
    });
    render(<RightPaneLayout {...defaultProps} />);

    for (const name of [/^Agents/, /^Triggers/, /^Workflows/, /^Pipeline/, /^Board/]) {
      expect(screen.queryByRole("button", { name })).toBeNull();
    }
    expect(screen.queryByText("Setup")).toBeNull();
    expect(screen.getByRole("button", { name: /Inbox/ })).toBeInTheDocument();
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { NAV_AXES, SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";

import { ShortcutsSection } from "./shortcuts-section";

// ---------- Mocks ----------

vi.mock("@/tauri/commands", () => ({
  loadAppState: () => Promise.resolve("{}"),
  saveAppState: () => Promise.resolve(),
  rebuildMenu: () => Promise.resolve(),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn() },
}));

const mockToast = vi.fn();
vi.mock("sonner", () => ({
  toast: (...args: unknown[]) => mockToast(...args),
}));

// ---------- Setup ----------

beforeEach(() => {
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
  mockToast.mockReset();
});

// ---------- Tests ----------

describe("ShortcutsSection", () => {
  it("renders the section header and all registry actions", () => {
    render(<ShortcutsSection query="" />);
    expect(screen.getByText("Shortcuts")).toBeInTheDocument();
    // One row per action
    for (const action of SHORTCUT_ACTIONS) {
      expect(screen.getByTestId(`shortcut-row-${action.id}`)).toBeInTheDocument();
    }
  });

  it("renders both SW-SN-6 shortcuts as user-configurable rows (goto-heading + rebound switch-org)", () => {
    render(<ShortcutsSection query="" />);
    // Both appear in Settings → Shortcuts, so both are rebindable by the user.
    const gotoHeading = screen.getByTestId("shortcut-row-nav.goto-heading");
    expect(gotoHeading).toBeInTheDocument();
    expect(gotoHeading).toHaveTextContent("Go to Heading");
    const switchOrg = screen.getByTestId("shortcut-row-nav.switch-org");
    expect(switchOrg).toBeInTheDocument();
    expect(switchOrg).toHaveTextContent("Switch Context Repo");
    // Go to Heading is discoverable by label search.
    render(<ShortcutsSection query="heading" />);
    expect(screen.getAllByTestId("shortcut-row-nav.goto-heading").length).toBeGreaterThan(0);
  });

  it("filters rows by the query (case-insensitive substring on label)", () => {
    render(<ShortcutsSection query="terminal" />);
    // Only viewer.toggle-terminal matches
    expect(screen.getByTestId("shortcut-row-viewer.toggle-terminal")).toBeInTheDocument();
    expect(screen.queryByTestId("shortcut-row-settings.open")).toBeNull();
  });

  it("renders an empty-state message when the query matches nothing", () => {
    render(<ShortcutsSection query="zzz-no-match-zzz" />);
    expect(screen.getByTestId("settings-shortcuts-empty")).toBeInTheDocument();
    // Reset-all button hidden when list is empty
    expect(screen.queryByTestId("shortcuts-reset-all")).toBeNull();
  });

  it("announces the visible count for screen readers when query is set", () => {
    render(<ShortcutsSection query="switch" />);
    const count = screen.getByTestId("settings-search-count-shortcuts");
    // "Switch Context Repo" + "Switch Team", plus the SW-SN-3 "Open Note Switcher"
    // (its label contains "Switch"). Switch Workspace was retired in SW-EW-33
    // (dup of the workspace palette, now ⌘K post-SW-FX-49). SW-OC-28 retired
    // SW-RM-UI-1's "Switch Machine" with the per-team machine picker.
    expect(count).toHaveTextContent("3 shortcuts shown");
  });

  it("Reset all button fires a sonner confirm toast with a Reset action", async () => {
    const user = userEvent.setup();
    render(<ShortcutsSection query="" />);
    await user.click(screen.getByTestId("shortcuts-reset-all"));
    expect(mockToast).toHaveBeenCalledTimes(1);
    const [message, opts] = mockToast.mock.calls[0];
    expect(message).toMatch(/Reset all/i);
    expect(opts).toMatchObject({ action: { label: "Reset" } });

    // Invoking the toast action callback fires resetAll on the store
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "CmdOrCtrl+Shift+." },
      stateLoaded: true,
    });
    opts.action.onClick();
    expect(useShortcutsStore.getState().shortcuts).toEqual({});
  });
});

describe("ShortcutsSection — nav-axis grouping (OC-17)", () => {
  it("renders each nav axis as a labelled cluster containing its rows", () => {
    render(<ShortcutsSection query="" />);
    // Story Workspace axis: labelled header + both cycle rows inside its cluster.
    const cluster = screen.getByTestId("shortcuts-nav-axis-story-workspace");
    expect(within(cluster).getByText("Story Workspace")).toBeInTheDocument();
    expect(
      within(cluster).getByTestId("shortcut-row-viewer.cycle-workspace-next"),
    ).toBeInTheDocument();
    expect(
      within(cluster).getByTestId("shortcut-row-viewer.cycle-workspace-prev"),
    ).toBeInTheDocument();
  });

  it("places every NAV_AXES id inside a nav cluster (not the flat rest list)", () => {
    render(<ShortcutsSection query="" />);
    for (const axis of NAV_AXES) {
      const cluster = screen.getByTestId(
        `shortcuts-nav-axis-${axis.label.toLowerCase().replace(/\s+/g, "-")}`,
      );
      for (const id of axis.actionIds) {
        expect(within(cluster).getByTestId(`shortcut-row-${id}`)).toBeInTheDocument();
      }
    }
  });

  it("keeps non-nav actions out of every nav cluster", () => {
    render(<ShortcutsSection query="" />);
    const navIds = new Set(NAV_AXES.flatMap((a) => a.actionIds));
    const nonNav = SHORTCUT_ACTIONS.find((a) => !navIds.has(a.id));
    expect(nonNav).toBeDefined();
    const row = screen.getByTestId(`shortcut-row-${nonNav!.id}`);
    expect(row.closest('[data-testid^="shortcuts-nav-axis-"]')).toBeNull();
  });

  it("a rebind flows through to the grouped row (drift-proof, resolved via registry)", () => {
    render(<ShortcutsSection query="" />);
    const current = () =>
      screen.getByTestId("shortcut-row-viewer.cycle-workspace-next-current").textContent;
    // Default binding is shown before any override.
    expect(current()).toBe(formatKeystrokeDisplay("CmdOrCtrl+Alt+]"));
    // Rebind via the store; the grouped row re-resolves through getEffectiveShortcut.
    act(() => {
      useShortcutsStore.setState({
        shortcuts: { "viewer.cycle-workspace-next": "CmdOrCtrl+Alt+P" },
        stateLoaded: true,
      });
    });
    expect(current()).toBe(formatKeystrokeDisplay("CmdOrCtrl+Alt+P"));
  });

  it("omits the flat rest list when a query matches only nav actions", () => {
    render(<ShortcutsSection query="Focus Pane" />);
    // The Pane Focus axis is shown as a cluster…
    expect(screen.getByTestId("shortcuts-nav-axis-pane-focus")).toBeInTheDocument();
    expect(screen.getByTestId("shortcut-row-viewer.focus-pane-left")).toBeInTheDocument();
    // …and no non-nav rows remain, so the trailing flat list is not rendered.
    expect(screen.queryByTestId("shortcuts-rest")).toBeNull();
  });

  it("renders no nav clusters when a query matches only non-nav actions", () => {
    render(<ShortcutsSection query="Export" />);
    expect(screen.queryAllByTestId(/^shortcuts-nav-axis-/)).toHaveLength(0);
    const rest = screen.getByTestId("shortcuts-rest");
    expect(within(rest).getByTestId("shortcut-row-viewer.export-pdf")).toBeInTheDocument();
  });
});

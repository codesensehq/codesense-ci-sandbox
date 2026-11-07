// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";

import { setDynamicShortcutActions, type ShortcutAction } from "@/shared/lib/shortcuts/registry";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";

import { ShortcutsSection } from "./shortcuts-section";

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
  toast: Object.assign((...args: unknown[]) => mockToast(...args), { error: vi.fn() }),
}));

const INBOX = "dashboard:team:acme:dev/inbox";
function command(id: string, label: string, chord: string): ShortcutAction {
  return {
    id: `${INBOX}/${id}`,
    label,
    defaultKeystroke: chord,
    scope: "global",
    command: {
      dashboard: INBOX,
      dashboardTitle: "Inbox",
      dashboardScope: "team",
      humanOnly: false,
      active: true,
    },
    run: () => {},
  };
}
const ARCHIVE = `${INBOX}/archive`;

beforeEach(() => {
  vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
  mockToast.mockReset();
  setDynamicShortcutActions([
    command("archive", "Archive thread", "CmdOrCtrl+B"),
    command("hide", "Hide thread", "CmdOrCtrl+H"),
    command("next", "Next thread", "CmdOrCtrl+Alt+Shift+N"),
  ]);
});
afterEach(() => {
  setDynamicShortcutActions([]);
  vi.restoreAllMocks();
});

describe("dashboard commands in Settings → Shortcuts", () => {
  it("leads with Conflicts and groups the dashboard's commands under its name after the built-ins", () => {
    render(<ShortcutsSection query="" />);
    const conflicts = screen.getByTestId("shortcuts-conflicts");
    expect(within(conflicts).getByText("Conflicts · 1")).toBeInTheDocument();
    expect(within(conflicts).getByText("Inbox · wants ⌘B, used by Toggle Sidebar")).toBeVisible();
    const group = screen.getByTestId(`shortcuts-dashboard-${INBOX}`);
    expect(within(group).getByText("Inbox")).toBeInTheDocument();
    expect(screen.getByTestId(`shortcut-row-${ARCHIVE}-note`)).toHaveTextContent(
      "Wants ⌘B. See Conflicts.",
    );
    expect(screen.getByTestId(`shortcut-row-${INBOX}/hide-note`)).toHaveTextContent(
      "Inbox asked for ⌘H. macOS uses ⌘H for Hide, so pick another with Edit.",
    );
    expect(screen.getByTestId(`shortcut-row-${INBOX}/next-current`)).toHaveTextContent("⌘⇧⌥N");
    const order = [...document.querySelectorAll("[data-testid]")].map((e) =>
      e.getAttribute("data-testid"),
    );
    expect(order.indexOf("shortcuts-conflicts")).toBeLessThan(order.indexOf("shortcuts-rest"));
    expect(order.indexOf("shortcuts-rest")).toBeLessThan(
      order.indexOf(`shortcuts-dashboard-${INBOX}`),
    );
  });

  it("searching the dashboard's name keeps its whole group", () => {
    render(<ShortcutsSection query="inbox" />);
    const group = screen.getByTestId(`shortcuts-dashboard-${INBOX}`);
    expect(within(group).getAllByTestId(/-current$/)).toHaveLength(3);
  });

  it("takes the chord at once, unbinds the built-in, and Undo puts both back", () => {
    render(<ShortcutsSection query="" />);
    fireEvent.click(screen.getByTestId(`shortcut-conflict-${ARCHIVE}-take`));
    expect(useShortcutsStore.getState().shortcuts).toEqual({
      [ARCHIVE]: "CmdOrCtrl+B",
      "viewer.toggle-nav": "",
    });
    expect(screen.queryByTestId("shortcuts-conflicts")).toBeNull();
    expect(screen.getByTestId("shortcut-row-viewer.toggle-nav-current")).toHaveTextContent(
      "No shortcut",
    );
    expect(screen.getByTestId("shortcut-row-viewer.toggle-nav-note")).toHaveTextContent(
      "⌘B now runs Inbox → Archive thread. Reset gives ⌘B back to Toggle Sidebar.",
    );
    const [message, options] = mockToast.mock.calls[0];
    expect(message).toBe("⌘B now runs Archive thread. Toggle Sidebar has no shortcut.");
    act(() => options.action.onClick());
    expect(useShortcutsStore.getState().shortcuts).toEqual({});
  });

  it("Reset on the previous owner gives the chord back and the command returns to Conflicts", () => {
    useShortcutsStore.setState({
      shortcuts: { [ARCHIVE]: "CmdOrCtrl+B", "viewer.toggle-nav": "" },
    });
    render(<ShortcutsSection query="" />);
    fireEvent.click(screen.getByTestId("shortcut-row-viewer.toggle-nav-reset"));
    expect(useShortcutsStore.getState().shortcuts).toEqual({});
    expect(mockToast).toHaveBeenCalledWith("⌘B is back on Toggle Sidebar.");
    expect(screen.getByTestId(`shortcut-conflict-${ARCHIVE}`)).toBeInTheDocument();
  });

  it("Edit then ⌫ stores no shortcut; a conflicting chord offers the same take-over", () => {
    render(<ShortcutsSection query="" />);
    const next = `${INBOX}/next`;
    fireEvent.click(screen.getByTestId(`shortcut-row-${next}-edit`));
    expect(screen.getByTestId(`shortcut-row-${next}-capture`)).toHaveTextContent(
      "Press keys, or ⌫ to remove",
    );
    fireEvent.keyDown(screen.getByTestId(`shortcut-row-${next}-capture`), { key: "Backspace" });
    expect(useShortcutsStore.getState().shortcuts[next]).toBe("");
    expect(screen.getByTestId(`shortcut-row-${next}-current`)).toHaveTextContent("No shortcut");

    fireEvent.click(screen.getByTestId(`shortcut-row-${next}-edit`));
    fireEvent.keyDown(screen.getByTestId(`shortcut-row-${next}-capture`), {
      key: "j",
      code: "KeyJ",
      metaKey: true,
    });
    expect(screen.getByTestId(`shortcut-row-${next}-error`)).toHaveTextContent(
      '⌘J is used by "Toggle Right Pane"',
    );
    fireEvent.click(screen.getByTestId(`shortcut-row-${next}-take`));
    expect(useShortcutsStore.getState().shortcuts).toMatchObject({
      [next]: "CmdOrCtrl+J",
      "viewer.toggle-right-pane": "",
    });
  });
});

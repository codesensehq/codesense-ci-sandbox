// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { SHORTCUT_ACTIONS, type ShortcutAction } from "@/shared/lib/shortcuts/registry";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";

import { ShortcutRow } from "./shortcut-row";

// ---------- Mocks ----------

vi.mock("@/tauri/commands", () => ({
  loadAppState: () => Promise.resolve("{}"),
  saveAppState: () => Promise.resolve(),
  rebuildMenu: () => Promise.resolve(),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn() },
}));

// ---------- Helpers ----------

function getAction(id: string): ShortcutAction {
  const a = SHORTCUT_ACTIONS.find((x) => x.id === id);
  if (!a) throw new Error(`no action ${id}`);
  return a;
}

beforeEach(() => {
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
});

// ---------- Tests ----------

describe("ShortcutRow", () => {
  it("renders the action label + current keystroke (default) + Edit + Reset (disabled)", () => {
    const action = getAction("settings.open");
    render(<ShortcutRow action={action} />);

    expect(screen.getByText("Open Settings")).toBeInTheDocument();
    // Default is CmdOrCtrl+, → display "⌘,"
    const current = screen.getByTestId("shortcut-row-settings.open-current");
    expect(current).toHaveTextContent("⌘,");
    // Reset is disabled when no override exists
    expect(screen.getByTestId("shortcut-row-settings.open-reset")).toBeDisabled();
  });

  it("displays the override (not the default) when one is set", () => {
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "CmdOrCtrl+Shift+." },
      stateLoaded: true,
    });
    render(<ShortcutRow action={getAction("settings.open")} />);
    expect(screen.getByTestId("shortcut-row-settings.open-current")).toHaveTextContent("⌘⇧.");
    expect(screen.getByTestId("shortcut-row-settings.open-reset")).not.toBeDisabled();
  });

  it("Edit button enters capture mode; clicking Cancel exits", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);

    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));
    expect(screen.getByTestId("shortcut-row-settings.open-capture")).toBeInTheDocument();

    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));
    expect(screen.queryByTestId("shortcut-row-settings.open-capture")).toBeNull();
  });

  it("capture keystroke saves via setShortcut and exits edit mode", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);

    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));
    const capture = screen.getByTestId("shortcut-row-settings.open-capture");

    act(() => {
      fireEvent.keyDown(capture, { key: ".", metaKey: true, shiftKey: true });
    });

    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBe("CmdOrCtrl+Shift+.");
    expect(screen.queryByTestId("shortcut-row-settings.open-capture")).toBeNull();
  });

  it("Escape exits capture without saving", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);

    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));
    act(() => {
      fireEvent.keyDown(screen.getByTestId("shortcut-row-settings.open-capture"), {
        key: "Escape",
      });
    });

    expect(useShortcutsStore.getState().shortcuts).toEqual({});
    expect(screen.queryByTestId("shortcut-row-settings.open-capture")).toBeNull();
  });

  it("capture ignores bare modifier presses (waits for the real key)", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);
    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));

    // Bare modifiers should NOT save
    const capture = screen.getByTestId("shortcut-row-settings.open-capture");
    act(() => {
      fireEvent.keyDown(capture, { key: "Meta", metaKey: true });
    });
    act(() => {
      fireEvent.keyDown(capture, { key: "Shift", shiftKey: true });
    });
    expect(useShortcutsStore.getState().shortcuts).toEqual({});
    // Capture mode still active
    expect(screen.getByTestId("shortcut-row-settings.open-capture")).toBeInTheDocument();
  });

  it("reserved keystroke shows inline error and does not save", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);
    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));

    act(() => {
      fireEvent.keyDown(screen.getByTestId("shortcut-row-settings.open-capture"), {
        key: "q",
        metaKey: true,
      });
    });

    const err = screen.getByTestId("shortcut-row-settings.open-error");
    expect(err).toHaveTextContent(/Quit/);
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBeUndefined();
  });

  it("conflicting keystroke shows inline error naming the conflicting action", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);
    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));

    // SW-SN-6: CmdOrCtrl+Shift+O is now the default for nav.goto-heading
    // (nav.switch-org moved to CmdOrCtrl+Shift+G).
    act(() => {
      fireEvent.keyDown(screen.getByTestId("shortcut-row-settings.open-capture"), {
        key: "o",
        metaKey: true,
        shiftKey: true,
      });
    });

    const err = screen.getByTestId("shortcut-row-settings.open-error");
    expect(err).toHaveTextContent(/Go to Heading/);
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBeUndefined();
  });

  it("rejects bare Tab (no modifier) with an inline error — would break keyboard nav", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);
    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));

    act(() => {
      fireEvent.keyDown(screen.getByTestId("shortcut-row-settings.open-capture"), {
        key: "Tab",
      });
    });

    expect(screen.getByTestId("shortcut-row-settings.open-error")).toHaveTextContent(
      /keyboard navigation/,
    );
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBeUndefined();
  });

  it("accepts Tab with a modifier (CmdOrCtrl+Alt+Tab)", async () => {
    const user = userEvent.setup();
    render(<ShortcutRow action={getAction("settings.open")} />);
    await user.click(screen.getByTestId("shortcut-row-settings.open-edit"));

    act(() => {
      fireEvent.keyDown(screen.getByTestId("shortcut-row-settings.open-capture"), {
        key: "Tab",
        metaKey: true,
        altKey: true,
      });
    });

    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBe("CmdOrCtrl+Alt+Tab");
  });

  it("Reset button returns the action to its default and clears the override", async () => {
    const user = userEvent.setup();
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "CmdOrCtrl+Shift+." },
      stateLoaded: true,
    });
    render(<ShortcutRow action={getAction("settings.open")} />);
    expect(screen.getByTestId("shortcut-row-settings.open-current")).toHaveTextContent("⌘⇧.");

    await user.click(screen.getByTestId("shortcut-row-settings.open-reset"));

    expect(useShortcutsStore.getState().shortcuts).toEqual({});
    expect(screen.getByTestId("shortcut-row-settings.open-current")).toHaveTextContent("⌘,");
    expect(screen.getByTestId("shortcut-row-settings.open-reset")).toBeDisabled();
  });
});

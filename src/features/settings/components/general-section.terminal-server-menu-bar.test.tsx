// SPDX-License-Identifier: MIT
/**
 * SV-8: the "Show terminal server in the menu bar" row — off by default, an
 * On | Off radio writing `updateSettings({ terminalServerMenuBar })`, shown on
 * macOS only.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

const mac = vi.hoisted(() => ({ value: true }));

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));
vi.mock("@/shared/lib/shortcuts/keystroke", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/shortcuts/keystroke")>()),
  isMac: () => mac.value,
}));

import { GeneralSection } from "./general-section";

describe("GeneralSection — terminal server in the menu bar (SV-8)", () => {
  beforeEach(() => {
    mac.value = true;
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, terminalServerMenuBar: undefined },
    });
  });

  it("is off by default and writes the setting on toggle", () => {
    render(<GeneralSection query="menu bar" />);
    expect(screen.getByTestId("settings-terminal-server-menu-bar-off")).toHaveAttribute(
      "aria-checked",
      "true",
    );

    fireEvent.click(screen.getByTestId("settings-terminal-server-menu-bar-on"));
    expect(useViewerStore.getState().settings.terminalServerMenuBar).toBe(true);

    fireEvent.click(screen.getByTestId("settings-terminal-server-menu-bar-off"));
    expect(useViewerStore.getState().settings.terminalServerMenuBar).toBe(false);
  });

  it("is not offered off macOS", () => {
    mac.value = false;
    render(<GeneralSection query="menu bar" />);
    expect(screen.queryByTestId("settings-terminal-server-menu-bar")).not.toBeInTheDocument();
  });
});

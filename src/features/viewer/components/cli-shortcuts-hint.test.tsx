// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

import { CliShortcutsHint } from "./cli-shortcuts-hint";

vi.mock("@/tauri/commands", () => ({
  loadAppState: () => Promise.resolve("{}"),
  saveAppState: () => Promise.resolve(),
  rebuildMenu: () => Promise.resolve(),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
}));

const openAppSettings = vi.fn();
vi.mock("@/features/settings/lib/open-settings", () => ({
  openAppSettings: (...args: unknown[]) => openAppSettings(...args),
}));

beforeEach(() => {
  useViewerStore.setState({ cliShortcutsHintSeen: false });
  openAppSettings.mockReset();
});

describe("CliShortcutsHint — OC-17", () => {
  it("renders the hint when it has not been seen", () => {
    render(<CliShortcutsHint />);
    expect(screen.getByTestId("cli-shortcuts-hint")).toBeInTheDocument();
    expect(screen.getByTestId("cli-shortcuts-hint-open")).toHaveTextContent("view shortcuts");
  });

  it("renders nothing once the hint has been seen", () => {
    useViewerStore.setState({ cliShortcutsHintSeen: true });
    render(<CliShortcutsHint />);
    expect(screen.queryByTestId("cli-shortcuts-hint")).toBeNull();
  });

  it("dismissing persists the seen flag and hides the hint", () => {
    render(<CliShortcutsHint />);
    fireEvent.click(screen.getByTestId("cli-shortcuts-hint-dismiss"));
    expect(useViewerStore.getState().cliShortcutsHintSeen).toBe(true);
    expect(screen.queryByTestId("cli-shortcuts-hint")).toBeNull();
  });

  it("the CTA opens Settings → Shortcuts and marks the hint seen", () => {
    render(<CliShortcutsHint />);
    fireEvent.click(screen.getByTestId("cli-shortcuts-hint-open"));
    expect(openAppSettings).toHaveBeenCalledWith("shortcuts");
    expect(useViewerStore.getState().cliShortcutsHintSeen).toBe(true);
  });
});

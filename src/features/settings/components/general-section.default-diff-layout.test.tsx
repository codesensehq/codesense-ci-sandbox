// SPDX-License-Identifier: MIT
/**
 * SLW-WD-7: the "Default diff layout" settings row — a two-option segmented
 * radio (Inline | Side-by-side) that seeds the per-tile source-diff fallback
 * via `updateSettings({ defaultSourceDiffLayout })` (AC-2/RD-4).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

// GeneralSection pulls in several rows with side-effecting imports; stub the
// Tauri/theme surfaces so the section renders in happy-dom. Only the
// DefaultDiffLayoutRow is rendered (query-filtered), so these are never invoked.
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

import { GeneralSection } from "./general-section";

describe("GeneralSection — default diff layout row (SLW-WD-7)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: "inline" },
    });
  });

  it("reflects the current setting and writes the chosen layout", () => {
    render(<GeneralSection query="default diff layout" />);
    const group = screen.getByTestId("settings-default-diff-layout");
    expect(group).toBeInTheDocument();

    // Inline is active by default.
    const inlineBtn = screen.getByTestId("settings-default-diff-layout-inline");
    const splitBtn = screen.getByTestId("settings-default-diff-layout-split");
    expect(inlineBtn).toHaveAttribute("aria-checked", "true");
    expect(splitBtn).toHaveAttribute("aria-checked", "false");

    // Choosing side-by-side writes the setting.
    fireEvent.click(splitBtn);
    expect(useViewerStore.getState().settings.defaultSourceDiffLayout).toBe("split");

    // And back to inline.
    fireEvent.click(screen.getByTestId("settings-default-diff-layout-inline"));
    expect(useViewerStore.getState().settings.defaultSourceDiffLayout).toBe("inline");
  });

  it("defaults the displayed value to inline when the setting is unset", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, defaultSourceDiffLayout: undefined },
    });
    render(<GeneralSection query="default diff layout" />);
    expect(screen.getByTestId("settings-default-diff-layout-inline")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("shows inline selected (not a blank row) for a corrupted persisted value", () => {
    // A corrupted blob must not leave the row with neither radio selected — the
    // row resolves through the same fail-safe the diff uses (→ inline).
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        defaultSourceDiffLayout: "sideBySide" as never,
      },
    });
    render(<GeneralSection query="default diff layout" />);
    expect(screen.getByTestId("settings-default-diff-layout-inline")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByTestId("settings-default-diff-layout-split")).toHaveAttribute(
      "aria-checked",
      "false",
    );
  });
});

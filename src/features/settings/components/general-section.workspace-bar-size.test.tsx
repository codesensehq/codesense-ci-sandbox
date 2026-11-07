// SPDX-License-Identifier: MIT
/**
 * The "Workspace bar size" row: Small / Normal / Large for the bottom workspace
 * bar (pills, avatars, badges, the Agents button and the bar's height scale
 * together). Normal is the original, larger bar.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

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

const setSize = (workspaceBarSize: unknown) =>
  useViewerStore.setState({
    settings: {
      ...useViewerStore.getState().settings,
      workspaceBarSize: workspaceBarSize as never,
    },
  });

describe("GeneralSection — workspace bar size", () => {
  beforeEach(() => setSize("normal"));

  it("offers Small, Normal and Large, with Normal selected by default", () => {
    render(<GeneralSection query="workspace bar size" />);
    const group = screen.getByRole("radiogroup", { name: "Workspace bar size" });
    expect(group).toBeInTheDocument();
    const labels = Array.from(group.querySelectorAll('[role="radio"]')).map((b) => b.textContent);
    expect(labels).toEqual(["Small", "Normal", "Large"]);
    expect(screen.getByTestId("settings-workspace-bar-size-normal")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("writes the chosen size to settings", () => {
    render(<GeneralSection query="workspace bar size" />);
    fireEvent.click(screen.getByTestId("settings-workspace-bar-size-small"));
    expect(useViewerStore.getState().settings.workspaceBarSize).toBe("small");
    fireEvent.click(screen.getByTestId("settings-workspace-bar-size-large"));
    expect(useViewerStore.getState().settings.workspaceBarSize).toBe("large");
  });

  it("is found by the words people search with", () => {
    for (const q of ["pills", "bottom bar", "tray", "scale"]) {
      const { unmount } = render(<GeneralSection query={q} />);
      expect(screen.getByTestId("settings-workspace-bar-size")).toBeInTheDocument();
      unmount();
    }
  });

  it.each([undefined, "gigantic"])(
    "shows Normal selected (not a blank row) for a missing or corrupted value (%s)",
    (saved) => {
      setSize(saved);
      render(<GeneralSection query="workspace bar size" />);
      expect(screen.getByTestId("settings-workspace-bar-size-normal")).toHaveAttribute(
        "aria-checked",
        "true",
      );
    },
  );

  it("stays out of unrelated searches", () => {
    for (const q of ["agents", "font"]) {
      const { unmount } = render(<GeneralSection query={q} />);
      expect(screen.queryByTestId("settings-workspace-bar-size")).toBeNull();
      unmount();
    }
  });
});

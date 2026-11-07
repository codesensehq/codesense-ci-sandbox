// SPDX-License-Identifier: MIT
/**
 * SW-VE-8: the "Editor line wrapping" settings row — a three-option segmented
 * radio (Auto | On | Off) writing `updateSettings({ editorLineWrap })` (AC-10).
 * Mirrors the pipeline-minimap row test.
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

import { GeneralSection, GENERAL_ROW_SPEC_LIST } from "./general-section";

describe("GeneralSection — editor line-wrap row (SW-VE-8)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, editorLineWrap: "auto" },
    });
  });

  it("reflects the setting (default Auto) and writes each chosen mode", () => {
    render(<GeneralSection query="line wrap" />);
    expect(screen.getByTestId("settings-editor-line-wrap")).toBeInTheDocument();

    expect(screen.getByTestId("settings-editor-line-wrap-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );

    fireEvent.click(screen.getByTestId("settings-editor-line-wrap-on"));
    expect(useViewerStore.getState().settings.editorLineWrap).toBe("on");

    fireEvent.click(screen.getByTestId("settings-editor-line-wrap-off"));
    expect(useViewerStore.getState().settings.editorLineWrap).toBe("off");

    fireEvent.click(screen.getByTestId("settings-editor-line-wrap-auto"));
    expect(useViewerStore.getState().settings.editorLineWrap).toBe("auto");
  });

  it("shows Auto (not a blank row) for a corrupted in-memory value", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, editorLineWrap: "garbage" as never },
    });
    render(<GeneralSection query="line wrap" />);
    expect(screen.getByTestId("settings-editor-line-wrap-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("is discoverable via its keywords (search wiring)", () => {
    const spec = GENERAL_ROW_SPEC_LIST.find((s) => s.key === "editorLineWrap");
    expect(spec).toBeDefined();
    expect(spec?.keywords).toContain("prose");
    render(<GeneralSection query="prose" />);
    expect(screen.getByTestId("settings-editor-line-wrap")).toBeInTheDocument();
  });
});

// SPDX-License-Identifier: MIT
/**
 * SW-VE-11: the four editor power-config settings rows — folding (tri-state),
 * whitespace (on/off), tab size (2/4/8), and prose font (on/off). Mirrors the
 * SW-VE-8 line-wrap row test.
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

beforeEach(() => {
  useViewerStore.setState({
    settings: {
      ...useViewerStore.getState().settings,
      editorFolding: "auto",
      editorShowWhitespace: false,
      editorTabSize: 2,
      editorProseFont: false,
    },
  });
});

describe("GeneralSection — editor folding row (SW-VE-11)", () => {
  it("reflects default Auto and writes each mode", () => {
    render(<GeneralSection query="fold" />);
    expect(screen.getByTestId("settings-editor-folding-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-editor-folding-on"));
    expect(useViewerStore.getState().settings.editorFolding).toBe("on");
    fireEvent.click(screen.getByTestId("settings-editor-folding-off"));
    expect(useViewerStore.getState().settings.editorFolding).toBe("off");
  });

  it("shows Auto (not a blank row) for a corrupted in-memory value", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, editorFolding: "garbage" as never },
    });
    render(<GeneralSection query="fold" />);
    expect(screen.getByTestId("settings-editor-folding-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });
});

describe("GeneralSection — editor whitespace row (SW-VE-11)", () => {
  it("defaults Off and toggles on", () => {
    render(<GeneralSection query="whitespace" />);
    expect(screen.getByTestId("settings-editor-whitespace-off")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-editor-whitespace-on"));
    expect(useViewerStore.getState().settings.editorShowWhitespace).toBe(true);
  });
});

describe("GeneralSection — editor tab size row (SW-VE-11)", () => {
  it("defaults 2 and writes 4 / 8", () => {
    render(<GeneralSection query="tab size" />);
    expect(screen.getByTestId("settings-editor-tab-size-2")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-editor-tab-size-4"));
    expect(useViewerStore.getState().settings.editorTabSize).toBe(4);
    fireEvent.click(screen.getByTestId("settings-editor-tab-size-8"));
    expect(useViewerStore.getState().settings.editorTabSize).toBe(8);
  });
});

describe("GeneralSection — editor prose font row (SW-VE-11)", () => {
  it("defaults Off and toggles on", () => {
    render(<GeneralSection query="prose font" />);
    expect(screen.getByTestId("settings-editor-prose-font-off")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-editor-prose-font-on"));
    expect(useViewerStore.getState().settings.editorProseFont).toBe(true);
  });
});

describe("GeneralSection — power-config rows are discoverable", () => {
  it.each([
    ["editorFolding", "fold"],
    ["editorWhitespace", "whitespace"],
    ["editorTabSize", "indent"],
    ["editorProseFont", "reading"],
  ])("%s row matches keyword %s", (key, keyword) => {
    const spec = GENERAL_ROW_SPEC_LIST.find((s) => s.key === key);
    expect(spec).toBeDefined();
    expect(spec?.keywords).toContain(keyword);
  });
});

// SPDX-License-Identifier: MIT
/**
 * SW-EW-32: the "Summary section headings" settings row — parses the
 * comma-separated input into an ordered list on blur (trim, drop blanks) and
 * falls back to the defaults when cleared.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

// GeneralSection pulls in several rows with side-effecting imports; stub the
// Tauri/theme surfaces so the section renders in happy-dom. Only the
// SummaryHeadingsRow is rendered (query-filtered), so these are never invoked.
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

describe("GeneralSection — summary headings row (SW-EW-32)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, summaryHeadings: ["Purpose", "Story"] },
    });
  });

  it("shows the current headings comma-joined and commits the parsed list on blur", async () => {
    render(<GeneralSection query="summary section headings" />);
    const input = screen.getByTestId("settings-summary-headings") as HTMLInputElement;
    expect(input.value).toBe("Purpose, Story");

    // Mixed spacing + a blank entry — all trimmed/dropped on commit.
    fireEvent.change(input, { target: { value: "Overview, Purpose , ,Story" } });
    fireEvent.blur(input);

    expect(useViewerStore.getState().settings.summaryHeadings).toEqual([
      "Overview",
      "Purpose",
      "Story",
    ]);
    // Re-normalized for display.
    await waitFor(() => expect(input.value).toBe("Overview, Purpose, Story"));
  });

  it("falls back to the defaults when the field is cleared", () => {
    render(<GeneralSection query="summary section headings" />);
    const input = screen.getByTestId("settings-summary-headings") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.blur(input);
    expect(useViewerStore.getState().settings.summaryHeadings).toEqual(["Purpose", "Story"]);
  });

  it.each(["", "Purpose,, Story"])("normalizes a no-op headings commit from %j", async (draft) => {
    render(<GeneralSection query="summary section headings" />);
    const input = screen.getByTestId("settings-summary-headings");
    fireEvent.change(input, { target: { value: draft } });
    fireEvent.blur(input);
    await waitFor(() => expect(input).toHaveValue("Purpose, Story"));
  });
});

it("keeps a dirty draft when a committed setting arrives from outside", () => {
  useViewerStore.setState({
    settings: { ...useViewerStore.getState().settings, summaryHeadings: ["Purpose", "Story"] },
  });
  render(<GeneralSection query="summary section headings" />);
  const input = screen.getByTestId("settings-summary-headings");
  fireEvent.change(input, { target: { value: "Pending local input" } });
  act(() => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, summaryHeadings: ["Outside"] },
    });
  });
  expect(input).toHaveValue("Pending local input");
});

// SPDX-License-Identifier: MIT
/**
 * SV-PBF-327: Settings → About → Check for Updates on a build with no update source reports
 * that, as information, with no "Try again". Other failures still read as a failed check.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

const { mockCheckForUpdate, mockToast, mockToastError } = vi.hoisted(() => ({
  mockCheckForUpdate: vi.fn(),
  mockToast: vi.fn(),
  mockToastError: vi.fn(),
}));

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: mockCheckForUpdate,
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));
vi.mock("sonner", () => ({
  toast: Object.assign(mockToast, { error: mockToastError, dismiss: vi.fn() }),
}));

import { GeneralSection } from "./general-section";

const NO_SOURCE = "This build has no update source. Install the latest release manually.";

describe("About row — update check failures", () => {
  beforeEach(() => {
    mockCheckForUpdate.mockReset();
    mockToast.mockReset();
    mockToastError.mockReset();
  });

  it("a build with no update source says so, without Try again", async () => {
    mockCheckForUpdate.mockRejectedValue({ kind: "notFound", message: NO_SOURCE });
    render(<GeneralSection query="" />);
    fireEvent.click(screen.getByTestId("settings-about-check-updates"));
    await waitFor(() => expect(mockToast).toHaveBeenCalled());

    const [title, opts] = mockToast.mock.calls[0];
    expect(title).toBe(NO_SOURCE);
    expect(opts.description).toBeUndefined();
    expect(JSON.stringify(mockToast.mock.calls)).not.toContain("Try again");
    expect(opts.action.label).toBe("Download manually");
    expect(mockToastError).not.toHaveBeenCalled();
  });

  it.each([
    [{ kind: "network", message: "update check failed: offline" }],
    [{ kind: "unknown", message: "boom" }],
    ["plain string error"],
  ])("other failures still read as a failed check: %j", async (err) => {
    mockCheckForUpdate.mockRejectedValue(err);
    render(<GeneralSection query="" />);
    fireEvent.click(screen.getByTestId("settings-about-check-updates"));
    await waitFor(() => expect(mockToastError).toHaveBeenCalled());

    const [title, opts] = mockToastError.mock.calls[0];
    expect(title).toBe("Update check failed");
    expect(opts.description).toBe("Try again or download manually.");
    expect(mockToast).not.toHaveBeenCalled();
  });
});

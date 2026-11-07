// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { PUBLIC_DOWNLOAD_URL } from "./channel";
import { COPY } from "./copy";

const { mockInvoke, mockListen, mockOpenUrl } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  mockListen: vi.fn(),
  mockOpenUrl: vi.fn(() => Promise.resolve()),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: mockOpenUrl }));

/** The view `license_state` returns since SV-PBF-278: the channel and the update fields. */
function view(overrides: Record<string, unknown> = {}) {
  return {
    channel: "beta",
    update: null,
    update_attention: null,
    updates_off: false,
    restart_ready: false,
    ...overrides,
  };
}

async function renderSection(channel: string, state: ReturnType<typeof view>) {
  window.__CODESENSE_CHANNEL__ = channel;
  mockInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === "license_state") return state;
    if (cmd === "license_headless_runs") return 0;
    if (cmd === "license_check_now") return state;
    throw new Error(`unknown command ${cmd}`);
  });
  const { UpdatesSection } = await import("./UpdatesSection");
  return render(<UpdatesSection />);
}

describe("UpdatesSection", () => {
  beforeEach(() => {
    cleanup();
    vi.resetModules();
    delete window.__CODESENSE_CHANNEL__;
    mockInvoke.mockReset();
    mockListen.mockReset();
    mockListen.mockResolvedValue(() => {});
    mockOpenUrl.mockClear();
  });

  it("renders nothing in the internal build, which has no license commands", async () => {
    await renderSection("internal", view());
    expect(screen.queryByTestId("settings-updates-section")).toBeNull();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("says how updates work when nothing is staged, and shows no license detail", async () => {
    await renderSection("beta", view());
    expect(await screen.findByTestId("settings-updates-idle")).toBeTruthy();
    expect(screen.getByText(COPY.updatesTitle)).toBeTruthy();
    for (const gone of ["Plan", "Status", "Expires", "Deactivate", "Private beta"]) {
      expect(screen.queryByText(gone)).toBeNull();
    }
  });

  it("shows the staged build, its notes and Restart to update", async () => {
    await renderSection(
      "beta",
      view({
        update: { staged_version: "0.1.1", notes: "Fixes." },
        restart_ready: true,
      }),
    );
    expect(await screen.findByText(COPY.updateReady("0.1.1"), { exact: false })).toBeTruthy();
    fireEvent.click(screen.getByTestId("settings-updates-release-notes"));
    expect(screen.getByTestId("settings-updates-release-notes-text").textContent).toBe("Fixes.");
    expect(screen.getByTestId("settings-updates-restart")).toBeTruthy();
    expect(screen.queryByTestId("settings-updates-idle")).toBeNull();
  });

  // SV-PBF-278 case row 14: every by-hand line points to the public download, never /beta/*.
  it.each([
    ["the app can't replace itself", { update_attention: "not_writable" }, "update-by-hand"],
    ["downloads keep failing", { update_attention: "failing" }, "update-by-hand"],
    [
      "a staged build can't be installed by a restart",
      { update: { staged_version: "0.1.1", notes: "" }, restart_ready: false },
      "update-by-hand",
    ],
    ["updates are off", { updates_off: true }, "updates-off"],
  ])("when %s, the by-hand line links to the public download", async (_, overrides, id) => {
    await renderSection("beta", view(overrides));
    const line = await screen.findByTestId(`settings-updates-${id}`);
    expect(line.textContent).toContain(COPY.downloadLatest);
    expect(line.textContent).not.toMatch(/download link|fresh links/);
    fireEvent.click(screen.getByTestId("settings-updates-update-download"));
    expect(mockOpenUrl).toHaveBeenCalledWith(PUBLIC_DOWNLOAD_URL);
    expect(PUBLIC_DOWNLOAD_URL).not.toContain("/beta/");
  });

  it("shows a staged build that can't restart without the Restart to update button", async () => {
    await renderSection(
      "beta",
      view({ update: { staged_version: "0.1.1", notes: "" }, restart_ready: false }),
    );
    expect(await screen.findByTestId("settings-updates-ready")).toBeTruthy();
    expect(screen.queryByTestId("settings-updates-restart")).toBeNull();
  });

  it("says when the update records can't be saved", async () => {
    await renderSection(
      "beta",
      view({
        update: { staged_version: "0.1.1", notes: "" },
        restart_ready: true,
        update_attention: "records_unwritable",
      }),
    );
    expect((await screen.findByTestId("settings-updates-records-unwritable")).textContent).toBe(
      COPY.recordsUnwritable(COPY.restartToUpdate),
    );
  });

  it("drops the daily-check line when updates are off", async () => {
    await renderSection("beta", view({ updates_off: true }));
    expect(await screen.findByTestId("settings-updates-updates-off")).toBeTruthy();
    expect(screen.queryByTestId("settings-updates-idle")).toBeNull();
    expect(screen.queryByTestId("settings-updates-check")).toBeNull();
  });

  it("Check now starts a background check and says so", async () => {
    await renderSection("beta", view());
    fireEvent.click(await screen.findByTestId("settings-updates-check"));
    expect((await screen.findByTestId("settings-updates-check-note")).textContent).toBe(
      COPY.checkStarted,
    );
    expect(mockInvoke).toHaveBeenCalledWith("license_check_now");
  });

  it("offers no Check now while a build is staged", async () => {
    await renderSection(
      "beta",
      view({ update: { staged_version: "0.0.32", notes: "" }, restart_ready: true }),
    );
    expect(await screen.findByTestId("settings-updates-ready")).toBeTruthy();
    expect(screen.queryByTestId("settings-updates-check")).toBeNull();
  });
});

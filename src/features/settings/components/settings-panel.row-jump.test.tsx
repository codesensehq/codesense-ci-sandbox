// SPDX-License-Identifier: MIT
/**
 * Opening Settings at a row (Check version → Agent CLIs) is a fresh request each time: it lands
 * on its section and row from whatever section the open tab shows, and a later manual section
 * change never replays it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@/test-utils";

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", resolvedTheme: "dark", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
  terminalShellInfo: vi.fn().mockResolvedValue(null),
  setTerminalShellOverride: vi.fn(),
  getDependencyStatus: vi.fn().mockResolvedValue({
    git: { available: true, path: "/usr/bin/git", version: "2.50.0" },
    agents: {},
  }),
  recheckDependencies: vi.fn(),
  revealContextRepo: vi.fn().mockResolvedValue(undefined),
  listDirectory: vi.fn().mockResolvedValue([]),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn().mockResolvedValue("0.0.0-test") }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));
vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: vi.fn() }));
vi.mock("@/shared/lib/codesense-home", () => ({
  getCodesenseHome: () => "/mock/home",
  deriveOrgRoot: (p: string) => p,
}));

const { useUiStore } = await import("@/shared/stores/ui-store");
const { openSettingsTab } = await import("@/features/settings/lib/open-settings-tab");
const { SettingsPanel } = await import("./settings-panel");

const navTo = (name: string) => fireEvent.click(screen.getByRole("tab", { name }));
const clisRow = () => document.querySelector<HTMLElement>('[data-setting-row="agentClis"]');

let scrolled: ReturnType<typeof vi.fn>;
beforeEach(() => {
  scrolled = vi.fn();
  Element.prototype.scrollIntoView = scrolled as never;
  useUiStore.setState({ settingsInitialSection: null, settingsInitialRow: null });
});
afterEach(() => useUiStore.setState({ settingsInitialSection: null, settingsInitialRow: null }));

describe("Settings opened at a row", () => {
  it("lands on Agent CLIs from another section of an open Settings tab", async () => {
    render(<SettingsPanel />);
    navTo("Appearance");
    expect(clisRow()).toBeNull();
    act(() => openSettingsTab("general", "agentClis"));
    await waitFor(() => expect(clisRow()).toHaveFocus());
  });

  it("lands again on a second request, and a later manual General click does not replay it", async () => {
    render(<SettingsPanel />);
    act(() => openSettingsTab("general", "agentClis"));
    await waitFor(() => expect(clisRow()).toHaveFocus());
    navTo("Appearance");
    act(() => openSettingsTab("general", "agentClis"));
    await waitFor(() => expect(clisRow()).toHaveFocus());
    const landings = scrolled.mock.calls.length;
    navTo("Appearance");
    navTo("General");
    await act(async () => {});
    expect(clisRow()).not.toHaveFocus();
    expect(scrolled.mock.calls.length).toBe(landings);
  });

  it("a row request the user leaves by hand before it lands is dropped", async () => {
    render(<SettingsPanel />);
    navTo("Appearance");
    // A request for a row on a section the user then leaves by hand.
    act(() => useUiStore.setState({ settingsInitialRow: "agentClis" }));
    navTo("Dashboards");
    expect(useUiStore.getState().settingsInitialRow).toBeNull();
    navTo("General");
    await act(async () => {});
    expect(clisRow()).not.toHaveFocus();
  });
});

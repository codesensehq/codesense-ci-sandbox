// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { resolveSlot } from "@/features/shell/slots/reveal-slot";
/**
 * The "Notify when an agent needs you" row — On/Off, default On, writing
 * `updateSettings({ osNotifications })`.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  readDashboardProfile: vi.fn(async () => null),
  writeDashboardProfile: vi.fn(async (_org, profile) => profile),
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));

const osAttention = vi.hoisted(() => ({ notify: vi.fn() }));
vi.mock("@/features/viewer/lib/os-attention", () => ({
  tauriOsAttention: osAttention,
}));

const sonner = vi.hoisted(() => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));
vi.mock("sonner", () => ({ toast: sonner.toast }));

import { GeneralSection } from "./general-section";

describe("GeneralSection — notify when an agent needs you", () => {
  beforeEach(() => {
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        osNotifications: undefined,
        notifyOnFinish: undefined,
        notificationSound: undefined,
      },
    });
    osAttention.notify.mockReset();
    sonner.toast.error.mockReset();
    sonner.toast.success.mockReset();
  });

  it("is found by 'notification', defaults On, and writes the chosen value", () => {
    render(<GeneralSection query="notification" />);
    expect(screen.getByTestId("settings-os-notifications-on")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-os-notifications-off"));
    expect(useViewerStore.getState().settings.osNotifications).toBe(false);
    fireEvent.click(screen.getByTestId("settings-os-notifications-on"));
    expect(useViewerStore.getState().settings.osNotifications).toBe(true);
  });

  it("has no classic Inbox choice", () => {
    render(<GeneralSection query="classic inbox" />);
    expect(screen.queryByText("Use the classic Inbox")).toBeNull();
    expect(screen.queryByTestId("settings-inbox-pane-preview-on")).toBeNull();
  });

  it("offers classic Notes only when listed, defaults On, and preserves Inbox in both directions", async () => {
    const org = "acme/repo";
    const ref = (id: string) => ({ scope: "team" as const, org, team: "dev", id });
    useOrgStore.setState({ activeOrgSlug: org, activeTeamName: "dev" });
    useDashboardProfileStore.setState({ org, profile: null });
    usePaneStore.setState({
      teamKey: `${org}:dev`,
      listing: {
        contentRoot: "/ctx",
        chain: [],
        panes: ["inbox", "notes"].map((id) => ({
          id,
          scope: "team",
          team: "dev",
          title: id,
          path: `/ctx/${id}.html`,
          panesDir: "/ctx",
          panelId: id,
          surface: id,
          dashboardRef: ref(id),
        })),
      },
    });
    render(<GeneralSection query="classic" />);
    const checked = (surface: string, value: string) =>
      expect(screen.getByTestId(`settings-${surface}-pane-preview-${value}`)).toHaveAttribute(
        "aria-checked",
        "true",
      );
    checked("notes", "on");
    expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.dashboardRef).toEqual(
      ref("inbox"),
    );
    fireEvent.click(screen.getByTestId("settings-notes-pane-preview-off"));
    await waitFor(() => checked("notes", "off"));
    expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.dashboardRef).toEqual(
      ref("inbox"),
    );
    fireEvent.click(screen.getByTestId("settings-notes-pane-preview-on"));
    await waitFor(() => checked("notes", "on"));
    expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.dashboardRef).toEqual(
      ref("inbox"),
    );
    expect(screen.getByText("Turn off to use the new Notes.")).toBeInTheDocument();
  });

  it("shows the selected inherited Notes pane even when a new child pane is listed first", async () => {
    const org = "acme/repo";
    const parent = { scope: "team" as const, org, team: "parent", id: "journal" };
    const child = { ...parent, team: "dev", id: "notes" };
    useOrgStore.setState({ activeOrgSlug: org, activeTeamName: "dev" });
    useDashboardProfileStore.setState({
      org,
      profile: {
        id: "default",
        name: "Default",
        posture: "",
        slots: {},
        cliLayout: "default",
        chrome: {},
        rightPaneSurfaces: { notes: parent },
      },
    });
    usePaneStore.setState({
      teamKey: `${org}:dev`,
      listing: {
        contentRoot: "/ctx",
        chain: ["dev", "parent"],
        panes: [child, parent].map((ref) => ({
          id: ref.id,
          title: ref.id,
          scope: ref.scope,
          team: ref.team,
          path: `/ctx/${ref.team}/${ref.id}.html`,
          panesDir: `/ctx/${ref.team}`,
          panelId: `pane:${org}:${ref.team}/${ref.id}`,
          surface: "notes",
          dashboardRef: ref,
        })),
      },
    });
    render(<GeneralSection query="classic notes" />);
    expect(screen.getByTestId("settings-notes-pane-preview-off")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-notes-pane-preview-on"));
    await waitFor(() =>
      expect(useDashboardProfileStore.getState().profile?.rightPaneSurfaces?.notes).toBe("native"),
    );
  });

  it("hides both rows while the listing belongs to the previous team", () => {
    useOrgStore.setState({ activeOrgSlug: "acme/repo", activeTeamName: "other" });
    usePaneStore.setState({
      teamKey: "acme/repo:dev",
      listing: {
        contentRoot: "/ctx",
        chain: ["dev"],
        panes: ["inbox", "notes"].map((id) => ({
          id,
          title: id,
          scope: "team",
          team: "dev",
          panelId: `pane:acme/repo:dev/${id}`,
          path: `/ctx/${id}.html`,
          panesDir: "/ctx",
          surface: id,
          dashboardRef: { scope: "team", org: "acme/repo", team: "dev", id },
        })),
      },
    });
    render(<GeneralSection query="classic" />);
    expect(screen.queryByText("Use the classic Notes")).not.toBeInTheDocument();
    expect(screen.queryByText("Use the classic Inbox")).not.toBeInTheDocument();
  });

  it("hides classic Notes when no dashboard is listed", () => {
    usePaneStore.setState({ listing: { contentRoot: "/ctx", chain: [], panes: [] } });
    render(<GeneralSection query="classic notes" />);
    expect(screen.queryByText("Use the classic Notes")).not.toBeInTheDocument();
  });

  it("is found by 'dock badge'", () => {
    render(<GeneralSection query="badge" />);
    expect(screen.getByTestId("settings-os-notifications")).toBeInTheDocument();
  });

  it("Send test notification: fires through the OS boundary and succeeds quietly", async () => {
    osAttention.notify.mockResolvedValue(true);
    render(<GeneralSection query="notification" />);
    fireEvent.click(screen.getByTestId("settings-test-notification"));
    await waitFor(() => expect(osAttention.notify).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(sonner.toast.success).toHaveBeenCalled());
    expect(sonner.toast.error).not.toHaveBeenCalled();
  });

  it("Send test notification: surfaces an actionable toast when the OS denies the plugin", async () => {
    osAttention.notify.mockResolvedValue(false);
    render(<GeneralSection query="notification" />);
    fireEvent.click(screen.getByTestId("settings-test-notification"));
    await waitFor(() => expect(sonner.toast.error).toHaveBeenCalled());
    const [, opts] = sonner.toast.error.mock.calls[0];
    expect(opts.description).toMatch(/System Settings/i);
  });

  it("finished-while-away: found by 'away', defaults On, and writes the chosen value", () => {
    render(<GeneralSection query="away" />);
    expect(screen.getByTestId("settings-notify-on-finish-on")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-notify-on-finish-off"));
    expect(useViewerStore.getState().settings.notifyOnFinish).toBe(false);
  });

  it("sound: found by 'sound', defaults Off, and writes the chosen value", () => {
    render(<GeneralSection query="sound" />);
    expect(screen.getByTestId("settings-notification-sound-off")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByTestId("settings-notification-sound-needs-you"));
    expect(useViewerStore.getState().settings.notificationSound).toBe("needs-you");
    fireEvent.click(screen.getByTestId("settings-notification-sound-all"));
    expect(useViewerStore.getState().settings.notificationSound).toBe("all");
  });
});

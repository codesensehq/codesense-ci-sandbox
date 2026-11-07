// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
vi.mock("next-themes", () => ({ useTheme: () => ({ theme: "system", setTheme: vi.fn() }) }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "test") }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));
const write = vi.hoisted(() => vi.fn(async (_org, profile) => profile));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  writeDashboardProfile: write,
}));
import { GeneralSection } from "./general-section";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
const ref = { scope: "built-in" as const, org: "acme", id: "files-tree" };
beforeEach(() => {
  write.mockClear();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" });
  useDashboardProfileStore.setState({
    org: "acme",
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      chrome: {},
      slots: {},
      cliLayout: "default",
    },
  });
  usePaneStore.setState({
    teamKey: "acme:a",
    listing: { contentRoot: "/ctx", chain: ["a"], panes: [] },
    refresh: async () => null,
  });
});
it("hides the row until a file-nav pane is available", () => {
  render(<GeneralSection query="classic file list" />);
  expect(screen.queryByText("Use the classic file list")).toBeNull();
});
it("writes file-nav and applies Off/On to the loaded profile immediately", async () => {
  usePaneStore.setState({
    listing: {
      contentRoot: "/ctx",
      chain: ["a"],
      panes: [
        {
          id: ref.id,
          title: "Files",
          scope: "private",
          path: "/builtin/files.html",
          panesDir: "/builtin",
          panelId: "files",
          dashboardRef: ref,
          declaration: { slots: ["file-nav"], capabilities: [] },
        },
      ],
    },
  });
  render(<GeneralSection query="classic file list" />);
  fireEvent.click(screen.getByRole("radio", { name: "Off" }));
  await waitFor(() =>
    expect(useDashboardProfileStore.getState().profile?.slots["file-nav"]).toEqual(ref),
  );
  expect(write.mock.calls.at(-1)?.[1].slots["file-nav"]).toEqual(ref);
  fireEvent.click(screen.getByRole("radio", { name: "On" }));
  await waitFor(() =>
    expect(useDashboardProfileStore.getState().profile?.slots["file-nav"]).toBe("native"),
  );
});

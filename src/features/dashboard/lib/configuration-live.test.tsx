// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({ read: vi.fn(), write: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  readDashboardProfile: api.read,
  writeDashboardProfile: api.write,
}));
import { setSlotDashboard, useDashboardProfileStore } from "./dashboard-profile";

beforeEach(() => {
  vi.clearAllMocks();
  useDashboardProfileStore.setState({
    org: "acme/repo",
    profile: {
      id: "default",
      name: "Cached name",
      posture: "",
      cliLayout: "default",
      slots: { dock: "native" },
      chrome: { newer: "cached" },
    },
  });
  api.write.mockImplementation(async (_org, profile) => profile);
});
it("sends only the changed slot, leaving newer on-disk fields with their writer", async () => {
  await setSlotDashboard("acme/repo", "file-nav", "native");
  expect(api.write).toHaveBeenCalledWith(
    "acme/repo",
    { slots: { "file-nav": "native" } },
    undefined,
  );
  expect(api.read).not.toHaveBeenCalled();
});

it("retires profile full reads from both registration and the shared profile store", () => {
  for (const file of [
    "dashboard-profile.ts",
    "../../shell/right-pane/panes/use-register-pane-panels.ts",
  ]) {
    expect(readFileSync(resolve(__dirname, file), "utf8")).not.toMatch(
      /loadDashboardProfile|readDashboardProfile/,
    );
  }
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const flag = vi.hoisted(() => ({ on: false }));
vi.mock("@/features/dashboard/lib/dock-default", () => ({
  get DOCK_DEFAULT_HTML_AGENT_LIST() {
    return flag.on;
  },
}));

import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { useOrgStore } from "@/shared/stores/org-store";
import type { DashboardProfile, DashboardRef } from "@/tauri/commands";
import { dashboardSelected, selectedRefs } from "./reveal-slot";
import { singleSlotChoice } from "./single-slot-dashboard";

const ref: DashboardRef = { scope: "team", org: "acme", team: "a", id: "people" };
const builtin: DashboardRef = { scope: "built-in", org: "acme", id: "agent-list" };
const profile = (slots: DashboardProfile["slots"]): DashboardProfile =>
  ({
    id: "default",
    name: "Default",
    posture: "",
    slots,
    cliLayout: "default",
    chrome: {},
  }) as never;

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" });
  useDashboardProfileStore.setState({ org: "acme", profile: profile({}), warm: {} });
});

describe.each([
  [false, "off", [] as DashboardRef[]],
  [true, "on", [builtin]],
])("the HTML agent list default is %s", (on, _label, unset) => {
  beforeEach(() => {
    flag.on = on;
  });

  it("resolves an unset dock choice to the default, and a saved choice always wins", () => {
    expect(selectedRefs("dock", "acme")).toEqual(unset);
    expect(singleSlotChoice("dock")).toEqual(unset[0] ?? null);
    expect(dashboardSelected(builtin, false, "dock")).toBe(on);
    useDashboardProfileStore.setState({ profile: profile({ dock: "native" }) });
    expect(selectedRefs("dock", "acme")).toEqual([]);
    expect(singleSlotChoice("dock")).toBeNull();
    useDashboardProfileStore.setState({ profile: profile({ dock: ref }) });
    expect(selectedRefs("dock", "acme")).toEqual([ref]);
    expect(dashboardSelected(builtin, false, "dock")).toBe(false);
  });

  it("never uses another organization's saved choice", () => {
    useDashboardProfileStore.setState({ profile: profile({ dock: ref }) });
    expect(selectedRefs("dock", "other")).toEqual(on ? [{ ...builtin, org: "other" }] : []);
  });
});

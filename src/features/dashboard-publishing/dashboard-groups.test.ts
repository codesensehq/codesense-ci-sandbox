// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { groupDashboardRows } from "./dashboard-groups";
import type { DashboardInventory, InventoryRow } from "./types";

const teamRow = (
  slug: string,
  org: string,
  team: string,
  sharedFrom: string | null = null,
): InventoryRow => ({
  slug,
  title: slug,
  folder: `/teams/${team}/content/dashboards/${slug}`,
  location: "local",
  localState: null,
  hosted: null,
  pushedAt: null,
  checkedAt: null,
  error: null,
  differentKey: false,
  team: { org, team, sharedFrom, relativeFolder: `dashboards/${slug}` },
});

const inventory: DashboardInventory = {
  host: null,
  source: "local",
  checkedAt: "",
  error: null,
  teams: [
    { org: "org-b/repo", team: "zeta" },
    { org: "org-a/repo", team: "parent" },
    { org: "org-a/repo", team: "alpha" },
  ],
  rows: [
    teamRow("shared", "org-a/repo", "alpha", "parent"),
    teamRow("normal", "org-a/repo", "alpha"),
    teamRow("z-dashboard", "org-b/repo", "zeta"),
    {
      ...teamRow("manual", "", ""),
      team: null,
      folder: "/tmp/manual",
    },
  ],
};

it("puts the active team first, sorts other teams, and keeps shared rows at the end", () => {
  const groups = groupDashboardRows(inventory, { org: "org-b/repo", team: "zeta" });

  expect(
    groups.map((group) => (group.kind === "team" ? `${group.org}:${group.team}` : group.key)),
  ).toEqual(["org-b/repo:zeta", "org-a/repo:alpha", "org-a/repo:parent", "attached"]);
  expect(groups[0]).toMatchObject({ kind: "team", active: true, rows: [{ slug: "z-dashboard" }] });
  expect(groups[1]).toMatchObject({
    kind: "team",
    rows: [{ slug: "normal" }],
    sharedRows: [{ slug: "shared" }],
  });
  expect(groups[2]).toMatchObject({ kind: "team", rows: [], sharedRows: [] });
  expect(groups[3]).toMatchObject({ kind: "attached", rows: [{ slug: "manual" }] });
});

// SPDX-License-Identifier: MIT
import type { DashboardInventory, InventoryRow } from "./types";

export type DashboardListGroup =
  | {
      kind: "team";
      key: string;
      org: string;
      team: string;
      active: boolean;
      rows: InventoryRow[];
      sharedRows: InventoryRow[];
    }
  | { kind: "attached"; key: "attached"; rows: InventoryRow[] };

function teamKey(org: string, team: string) {
  return `${org}\u0000${team}`;
}

export function groupDashboardRows(
  inventory: DashboardInventory,
  activeTeam: { org: string | null; team: string | null },
): DashboardListGroup[] {
  const teams = new Map<string, DashboardListGroup & { kind: "team" }>();
  for (const header of inventory.teams) {
    const key = teamKey(header.org, header.team);
    teams.set(key, {
      kind: "team",
      key,
      org: header.org,
      team: header.team,
      active: header.org === activeTeam.org && header.team === activeTeam.team,
      rows: [],
      sharedRows: [],
    });
  }

  const attached: InventoryRow[] = [];
  for (const row of inventory.rows) {
    if (!row.team) {
      attached.push(row);
      continue;
    }
    const key = teamKey(row.team.org, row.team.team);
    let group = teams.get(key);
    if (!group) {
      group = {
        kind: "team",
        key,
        org: row.team.org,
        team: row.team.team,
        active: row.team.org === activeTeam.org && row.team.team === activeTeam.team,
        rows: [],
        sharedRows: [],
      };
      teams.set(key, group);
    }
    (row.team.sharedFrom ? group.sharedRows : group.rows).push(row);
  }

  const byTitle = (a: InventoryRow, b: InventoryRow) =>
    a.title.localeCompare(b.title) || a.slug.localeCompare(b.slug);
  const grouped: DashboardListGroup[] = [...teams.values()]
    .map((group) => ({
      ...group,
      rows: group.rows.sort(byTitle),
      sharedRows: group.sharedRows.sort(byTitle),
    }))
    .sort(
      (a, b) =>
        Number(b.active) - Number(a.active) ||
        a.org.localeCompare(b.org) ||
        a.team.localeCompare(b.team),
    );
  attached.sort(byTitle);
  if (attached.length) grouped.push({ kind: "attached", key: "attached", rows: attached });
  return grouped;
}

export function dashboardRowKey(row: InventoryRow) {
  const team = row.team
    ? `${row.team.org}\u0000${row.team.team}\u0000${row.team.sharedFrom ?? ""}`
    : "";
  return `${row.slug}\u0000${row.folder ?? ""}\u0000${team}`;
}

export function sharedRowsByParent(group: DashboardListGroup & { kind: "team" }) {
  return [...new Set(group.sharedRows.map((row) => row.team?.sharedFrom))]
    .filter((parent): parent is string => !!parent)
    .sort((a, b) => a.localeCompare(b))
    .map(
      (parent) =>
        [parent, group.sharedRows.filter((row) => row.team?.sharedFrom === parent)] as const,
    );
}

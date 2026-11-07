// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import type { RoleConfig } from "@/tauri/commands";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "../fixtures";

/**
 * The Roles mock backend: the fixture team's named queues, readable (`roles_list`) and savable
 * (`role_save`) so the Roles view works in the mock app. A save replaces the role of that id (or
 * adds it) and emits `roles:changed`, as the real backend does.
 */
const FIXTURE_TEAM = `${MOCK_ORG_SLUG}:${MOCK_TEAM_NAME}`;

const SEED: RoleConfig[] = [
  { id: "triage", name: "Triage", icon: "builtin-chess:knight", defaultHandler: null },
  { id: "reviewer", name: "Reviewer", icon: "builtin-chess:rook", defaultHandler: null },
];

export function roleHandlers(events: MockEventBus): HandlerRegistry {
  const byTeam = new Map<string, RoleConfig[]>();
  const rolesOf = (teamKey: string): RoleConfig[] => {
    if (!byTeam.has(teamKey)) byTeam.set(teamKey, teamKey === FIXTURE_TEAM ? [...SEED] : []);
    return byTeam.get(teamKey)!;
  };
  return {
    roles_list: (args) => rolesOf(String(args.teamKey)).map((r) => ({ ...r })),
    role_save: (args) => {
      const teamKey = String(args.teamKey);
      const config = args.config as RoleConfig;
      const rows = rolesOf(teamKey);
      const at = rows.findIndex((r) => r.id === config.id);
      if (at >= 0) rows[at] = { ...config };
      else rows.push({ ...config });
      events.emit("roles:changed", { teamKey });
      return { ...config };
    },
  };
}

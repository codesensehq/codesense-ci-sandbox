// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { logger } from "@/shared/lib/logger";
import { NO_TEAM_SENTINEL, parseTeamKey } from "@/shared/lib/team-key";
import { agentReadRoles, releaseRole, type InboxTeam, type RoleQueue } from "@/tauri/commands";
import { AGENT_STATE_CHANGED_EVENT } from "@/tauri/events";

/** A role a session holds: its live registration, by the instance id the session registered
 *  with (its session id), in the team whose role state lists it. */
export interface RoleHolding {
  role: string;
  /** The team-configured display name, falling back to the stable queue id. */
  label: string;
  instanceId: string;
  generation: number;
  /** The team whose role state holds the registration (Release acts there). */
  teamKey: string;
}

/** Registrations lapse silently, so the list re-reads on this beat as well as on changes. */
const POLL_MS = 20_000;
/** A burst of role-state changes is read once, this long after the last of them. */
export const ROLE_READ_DEBOUNCE_MS = 250;

/** The live registrations of a role read, by the instance id that holds them. */
export function holdingsByInstance(
  roles: readonly RoleQueue[],
  teamKey: string,
): ReadonlyMap<string, RoleHolding[]> {
  const by = new Map<string, RoleHolding[]>();
  for (const queue of roles) {
    for (const reg of queue.registrations) {
      if (!reg.live) continue;
      const list = by.get(reg.instanceId) ?? [];
      list.push({
        role: queue.role,
        label: queue.roleName?.trim() || queue.role,
        instanceId: reg.instanceId,
        generation: reg.generation,
        teamKey,
      });
      by.set(reg.instanceId, list);
    }
  }
  return by;
}

const NO_HOLDINGS: ReadonlyMap<string, RoleHolding[]> = new Map();

export interface RoleHoldings {
  /** The roles a session of `teamKey` holds, by its session id or handle; empty when none. */
  holdsOf: (
    teamKey: string,
    session: { sessionId: string | null; handle: string },
  ) => readonly RoleHolding[];
  /** Release a held role (the same `release_role` an agent calls), in its own team: its
   *  un-accepted claims go back to the queue. */
  release: (holding: RoleHolding) => Promise<void>;
}

/** The inbox team a team key names, or null for one with no team. */
function inboxTeamOf(teamKey: string): InboxTeam | null {
  if (teamKey === NO_TEAM_SENTINEL) return null;
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  return teamName ? { org: orgSlug, team: teamName } : null;
}

/**
 * The roles each session holds in every team of `teamKeys`, read from each team's role state
 * (what `role_state` returns to agents), re-read when role state changes and on a slow beat while
 * `enabled`. Read only, except Release.
 */
export function useRoleHoldings(teamKeys: readonly string[], enabled: boolean): RoleHoldings {
  // Compared by value: the caller may build a new list each render.
  const teamsId = [...new Set(teamKeys)].sort().join("\n");
  const teams = useMemo(() => {
    const out = new Map<string, InboxTeam>();
    for (const tk of teamsId ? teamsId.split("\n") : []) {
      const team = inboxTeamOf(tk);
      if (team) out.set(tk, team);
    }
    return out;
  }, [teamsId]);
  const [held, setHeld] = useState<Readonly<Record<string, ReadonlyMap<string, RoleHolding[]>>>>(
    {},
  );
  // Each team's latest read, so an older answer never overwrites a newer one.
  const seq = useRef(new Map<string, number>());
  // The teams listed now, so an answer for a team that has left the list is dropped.
  const teamsRef = useRef(teams);
  useEffect(() => {
    teamsRef.current = teams;
    // A team no longer listed keeps no read count; its holdings go with the next update (and
    // `holdsOf` reads nothing of it meanwhile).
    for (const tk of [...seq.current.keys()]) if (!teams.has(tk)) seq.current.delete(tk);
  }, [teams]);

  /** One team's holdings as read now; undefined when the read failed or a newer one started. */
  const readTeam = useCallback(async (teamKey: string, team: InboxTeam) => {
    const mine = (seq.current.get(teamKey) ?? 0) + 1;
    seq.current.set(teamKey, mine);
    try {
      const { roles } = await agentReadRoles(team, undefined, 1);
      return mine === seq.current.get(teamKey) ? holdingsByInstance(roles, teamKey) : undefined;
    } catch (err) {
      logger.info("agents", `Role holdings read failed: ${String(err)}`);
      return undefined;
    }
  }, []);
  /** Keep the answers of the teams still listed, in one update (a failed read keeps the last). */
  const apply = useCallback(
    (answers: readonly (readonly [string, ReadonlyMap<string, RoleHolding[]> | undefined])[]) => {
      const fresh = answers.filter(
        (a): a is readonly [string, ReadonlyMap<string, RoleHolding[]>] =>
          a[1] !== undefined && teamsRef.current.has(a[0]),
      );
      if (fresh.length === 0) return;
      setHeld((prev) => ({
        ...Object.fromEntries(Object.entries(prev).filter(([tk]) => teamsRef.current.has(tk))),
        ...Object.fromEntries(fresh),
      }));
    },
    [],
  );
  const reloadTeam = useCallback(
    async (teamKey: string, team: InboxTeam) => apply([[teamKey, await readTeam(teamKey, team)]]),
    [readTeam, apply],
  );
  // Every team read together, their answers merged into one update.
  const reload = useCallback(async () => {
    apply(
      await Promise.all(
        [...teams].map(async ([tk, team]) => [tk, await readTeam(tk, team)] as const),
      ),
    );
  }, [teams, readTeam, apply]);

  useEffect(() => {
    if (!enabled || teams.size === 0) return;
    void reload();
    let pending: ReturnType<typeof setTimeout> | undefined;
    const later = () => {
      clearTimeout(pending);
      pending = setTimeout(() => void reload(), ROLE_READ_DEBOUNCE_MS);
    };
    const timer = setInterval(later, POLL_MS);
    const unlisten = listen(AGENT_STATE_CHANGED_EVENT, later);
    return () => {
      clearInterval(timer);
      clearTimeout(pending);
      void unlisten.then((u) => u()).catch(() => {});
    };
  }, [enabled, teams, reload]);

  const holdsOf = useCallback(
    (teamKey: string, session: { sessionId: string | null; handle: string }) => {
      // A team no longer listed holds nothing here.
      const by = teams.has(teamKey) ? (held[teamKey] ?? NO_HOLDINGS) : NO_HOLDINGS;
      return (
        (session.sessionId ? by.get(session.sessionId) : undefined) ?? by.get(session.handle) ?? []
      );
    },
    [held, teams],
  );

  const release = useCallback(
    async (holding: RoleHolding) => {
      const team = inboxTeamOf(holding.teamKey);
      if (!team) return;
      try {
        await releaseRole(team, holding.role, holding.instanceId, holding.generation);
        toast.success(`Released ${holding.label}`);
      } catch (err) {
        toast.error(`Couldn't release ${holding.label}: ${String(err)}`);
      }
      await reloadTeam(holding.teamKey, team);
    },
    [reloadTeam],
  );

  return useMemo(() => ({ holdsOf, release }), [holdsOf, release]);
}

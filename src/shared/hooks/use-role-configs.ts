// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { rolesList, type RoleConfig } from "@/tauri/commands";
import { useActiveTeamKey } from "@/shared/lib/team-key";

const EMPTY: readonly RoleConfig[] = [];
const pending = new Set<string>();
const refreshAfterRead = new Set<string>();
export const useRoleConfigsStore = create<{
  byTeam: Record<string, readonly RoleConfig[]>;
  errors: Record<string, string | undefined>;
}>(() => ({ byTeam: {}, errors: {} }));
export async function reloadRoleConfigs(teamKey: string): Promise<void> {
  if (!teamKey || !teamKey.includes(":") || teamKey.endsWith(":_none")) return;
  if (pending.has(teamKey)) {
    refreshAfterRead.add(teamKey);
    return;
  }
  pending.add(teamKey);
  try {
    const rows = await rolesList(teamKey);
    useRoleConfigsStore.setState((s) => ({
      byTeam: { ...s.byTeam, [teamKey]: rows },
      errors: { ...s.errors, [teamKey]: undefined },
    }));
  } catch (e) {
    useRoleConfigsStore.setState((s) => ({ errors: { ...s.errors, [teamKey]: String(e) } }));
  } finally {
    pending.delete(teamKey);
    if (refreshAfterRead.delete(teamKey)) void reloadRoleConfigs(teamKey);
  }
}
let watchers = 0;
let unlisten: Promise<() => void>[] = [];
export function watchRoleConfigs() {
  if (watchers++ === 0) {
    const reload = () => {
      for (const key of Object.keys(useRoleConfigsStore.getState().byTeam))
        void reloadRoleConfigs(key);
    };
    unlisten = [listen("roles:changed", reload), listen("sync-wave-completed", reload)];
    reload();
  }
  return () => {
    if (--watchers === 0) {
      for (const stop of unlisten) void stop.then((f) => f()).catch(() => {});
      unlisten = [];
    }
  };
}
export function useRoleConfigs(teamKey?: string) {
  const active = useActiveTeamKey();
  const key = teamKey ?? active;
  const roles = useRoleConfigsStore((s) => s.byTeam[key] ?? EMPTY);
  const error = useRoleConfigsStore((s) => s.errors[key]);
  useEffect(watchRoleConfigs, []);
  useEffect(() => {
    if (!useRoleConfigsStore.getState().byTeam[key] && !pending.has(key))
      void reloadRoleConfigs(key);
  }, [key]);
  return { roles, error, teamKey: key };
}

// SPDX-License-Identifier: MIT
import { useCallback, useRef, useState } from "react";

import {
  useDashboardPolicy,
  OPEN_POLICY,
  updateDashboardPolicy,
  type DashboardPolicy,
} from "@/features/dashboard/lib/verb-policy";
import { usePrefsStore } from "@/features/dashboard/lib/prefs-client";
import { logger } from "@/shared/lib/logger";
import type { DashboardCommandInfo, ShortcutAction } from "@/shared/lib/shortcuts/registry";

type HumanOnlyPolicy = Pick<DashboardPolicy, "preset" | "humanOnly">;

/** Whether a dashboard's commands are kept from agents as a whole, and why it's locked. */
export function dashboardHumanOnly(policy: HumanOnlyPolicy, info: DashboardCommandInfo) {
  const teamSafe = policy.preset === "team-safe" && info.dashboardScope === "private";
  return {
    on: teamSafe || policy.humanOnly?.dashboards?.[info.dashboard] === true,
    lockedBy: teamSafe ? ("team-safe" as const) : null,
  };
}

/** One command's humans-only state: the same rule the host applies to agents
 *  (`Policy::human_only`), with why the switch is locked, if it is. */
export function commandHumanOnly(policy: HumanOnlyPolicy, action: ShortcutAction) {
  const info = action.command!;
  const dashboard = dashboardHumanOnly(policy, info);
  const own = policy.humanOnly?.commands?.[action.id];
  return {
    on: dashboard.on || (own ?? info.humanOnly),
    lockedBy: dashboard.lockedBy ?? (dashboard.on ? ("dashboard" as const) : null),
    overridden: own !== undefined,
    /** Shown while the manifest's suggestion stands. */
    suggested: info.humanOnly && own === undefined && !dashboard.on,
  };
}

/** Live committed policy shared with Settings and dashboard hosts. */
export function useHumanOnlyPolicy() {
  const committed = useDashboardPolicy() ?? OPEN_POLICY;
  const principal = usePrefsStore((state) => state.principal);
  const sequence = useRef(0);
  const [pending, setPending] = useState<
    Array<{ id: number; principal: string | null; apply: (p: DashboardPolicy) => DashboardPolicy }>
  >([]);
  const policy = pending
    .filter((edit) => edit.principal === principal)
    .reduce((value, edit) => edit.apply(value), committed);
  const write = useCallback(
    (kind: "dashboards" | "commands", key: string, value: boolean | undefined) => {
      const apply = <P extends HumanOnlyPolicy>(p: P): P => {
        const entries = { ...p.humanOnly?.[kind] };
        if (value === undefined) delete entries[key];
        else entries[key] = value;
        return { ...p, humanOnly: { ...p.humanOnly, [kind]: entries } };
      };
      const id = ++sequence.current;
      setPending((edits) => [...edits, { id, principal, apply }]);
      void updateDashboardPolicy(apply)
        .catch((e: unknown) => logger.warn("human-only", `write policy failed — ${String(e)}`))
        .finally(() => setPending((edits) => edits.filter((edit) => edit.id !== id)));
    },
    [principal],
  );
  return {
    policy,
    setDashboard: (dashboard: string, on: boolean) =>
      write("dashboards", dashboard, on ? true : undefined),
    setCommand: (actionId: string, on: boolean | undefined) => write("commands", actionId, on),
  };
}

// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import { fieldPatch } from "@/shared/lib/field-patch";
import { usePrefsStore } from "./prefs-client";
import { loadAppState, saveAppState } from "@/tauri/commands";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
export type ConfirmPolicy = "never" | "prompt" | "prompt-once";
export type DashboardPolicy = {
  preset: "open" | "team-safe";
  switches: {
    confirmMessageDestructive: boolean;
    confirmTeamSendAsYou: boolean;
    privateInstall?: boolean;
  };
  perDashboard: Record<
    string,
    { grant?: string[]; [verb: string]: ConfirmPolicy | string[] | undefined }
  >;
  /** "Humans only" switches for dashboard commands, by dashboard key and by command id. */
  humanOnly?: { dashboards?: Record<string, boolean>; commands?: Record<string, boolean> };
};
export const OPEN_POLICY: DashboardPolicy = {
  preset: "open",
  switches: {
    confirmMessageDestructive: false,
    confirmTeamSendAsYou: false,
    privateInstall: false,
  },
  perDashboard: {},
};
function normalizePolicy(policy: Partial<DashboardPolicy> | undefined): DashboardPolicy {
  return { ...OPEN_POLICY, ...policy, switches: { ...OPEN_POLICY.switches, ...policy?.switches } };
}
export function updateDashboardPolicy(
  update: (policy: DashboardPolicy) => DashboardPolicy,
): Promise<void> {
  return serializeAppStateWrite(async () => {
    const raw = await loadAppState();
    const state = raw ? JSON.parse(raw) : {};
    const policy = normalizePolicy(state.dashboardPolicy);
    const patch = fieldPatch(policy, update(policy));
    if (patch !== undefined) await saveAppState(JSON.stringify({ dashboardPolicy: patch }), true);
  });
}

export function useDashboardPolicy(): DashboardPolicy | null {
  const policy = usePrefsStore((state) => state.owned.dashboardPolicy);
  const ready = usePrefsStore((state) => state.ownedReady);
  return useMemo(
    () => (ready ? normalizePolicy(policy as Partial<DashboardPolicy> | undefined) : null),
    [policy, ready],
  );
}

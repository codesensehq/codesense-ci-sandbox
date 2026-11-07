// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import type { MachineSummary } from "@/tauri/commands";

/**
 * SW-AGR-19 PR-2 — the terminal servers older than this app, read straight from
 * `list_machines` (`terminalServer.relation`), so the UI and MCP count the same
 * set. Only `older` counts: a server from another CodeSense build on This Mac
 * (`otherBuild`) or a newer one is never nagged about.
 */
export function outOfDateServers(machines: readonly MachineSummary[]): MachineSummary[] {
  return machines.filter(
    (m) => m.status.state === "connected" && m.terminalServer?.relation === "older",
  );
}

/** One stale server's identity: the machine and the build it runs, so a
 *  dismissal covers exactly that build. */
export function outOfDateEntry(machine: MachineSummary): string {
  const s = machine.terminalServer;
  const build = s?.buildDigest ?? s?.buildCommit ?? s?.serverVersion ?? s?.announcedVersion ?? "";
  const when = s?.builtAtMs ?? s?.startedAtMs;
  return `${machine.machineId}@${build}${when ? `:${when}` : ""}`;
}

/** The stale servers to tell the user about: hidden while every one of them
 *  is one the user already dismissed. A server that is updated, or goes
 *  offline for a moment, never brings the notice back; a newly stale server,
 *  or another stale build, does. */
export function undismissed(
  servers: readonly MachineSummary[],
  dismissed: readonly string[],
): boolean {
  return servers.some((m) => !dismissed.includes(outOfDateEntry(m)));
}

export function useOutOfDateNotice(machines: readonly MachineSummary[] | null): {
  servers: MachineSummary[];
  dismiss: () => void;
} {
  const dismissed = useViewerStore((s) => s.dismissedTerminalServers);
  const servers = useMemo(() => (machines ? outOfDateServers(machines) : []), [machines]);
  return useMemo(
    () => ({
      servers: undismissed(servers, dismissed) ? servers : [],
      dismiss: () => useViewerStore.getState().dismissTerminalServers(servers.map(outOfDateEntry)),
    }),
    [servers, dismissed],
  );
}

export function outOfDateLabel(count: number): string {
  return count === 1 ? "1 terminal server out of date" : `${count} terminal servers out of date`;
}

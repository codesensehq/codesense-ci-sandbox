// SPDX-License-Identifier: MIT
// SW-XS-10: what a failed Step 2's Retry runs. A launch path records how to
// launch an agent into a workspace again, keyed by (team, workspace, agent); the
// failure line's Retry re-runs exactly that agent's launch — its Step 2 again
// (never Step 1), then the spawn continues. Two agents in one workspace keep
// separate launches. A workspace whose agent was launched elsewhere (an MCP
// call, another window) has none recorded, and its failure line offers
// "Launch <agent> again" instead.

const launches = new Map<string, () => Promise<unknown>>();

const key = (teamKey: string, workspaceId: string, agent: string) =>
  `${teamKey}\u0000${workspaceId}\u0000${agent}`;

/** Record how to launch `agent` into this workspace again. `names` are every
 *  name its Step 2 events may carry (the display name, the key, its id). */
export function rememberAgentLaunch(
  teamKey: string,
  workspaceId: string,
  names: readonly string[],
  launch: () => Promise<unknown>,
): void {
  for (const name of new Set(names.filter(Boolean)))
    launches.set(key(teamKey, workspaceId, name), launch);
}

/** The recorded launch of `agent` (as its Step 2 named it) here, if any. */
export function agentLaunchRetry(
  teamKey: string,
  workspaceId: string,
  agent: string | undefined,
): (() => Promise<unknown>) | undefined {
  return agent ? launches.get(key(teamKey, workspaceId, agent)) : undefined;
}

/** Tests only. */
export function clearAgentLaunchRetries(): void {
  launches.clear();
}

/** Every name an agent's Step 2 events may carry: its display name (what the
 *  backend prepares it under), its key, and the id its key ends in. */
export function agentLaunchNames(agentKey: string, displayName?: string): string[] {
  const id = agentKey.slice(agentKey.lastIndexOf("/") + 1);
  return [displayName ?? "", agentKey, id].filter(Boolean);
}

/** Run an agent launch into a workspace, recording it for Retry. A launch that
 *  refuses before its Step 2 reported (an outdated terminal server, a machine
 *  that won't take it) while the workspace's line waits on it puts the reason
 *  on that line instead of leaving it waiting. */
export function trackAgentLaunch<T>(
  input: {
    teamKey: string;
    workspaceId: string;
    names: readonly string[];
    /** Whether the line waits on this launch. (An outdated terminal server is
     *  always reported: the line offers its one-click update.) */
    lineWaits: () => boolean;
    report: (names: readonly string[], reason: string, code?: string) => void;
  },
  launch: () => Promise<T>,
): Promise<T> {
  const run = (): Promise<T> =>
    launch().catch((error: unknown) => {
      const code = (error as { code?: string } | null)?.code;
      if (code === "terminalServerOutdated" || input.lineWaits())
        input.report(input.names, error instanceof Error ? error.message : String(error), code);
      throw error;
    });
  rememberAgentLaunch(input.teamKey, input.workspaceId, input.names, run);
  return run();
}

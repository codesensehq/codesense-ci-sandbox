// SPDX-License-Identifier: MIT
import { resolveAgentCli } from "@/tauri/commands";

/** The agent a launch runs when its caller names none: the built-in Claude starter. */
export const DEFAULT_LAUNCH_AGENT = "builtin/claude";

/** The agent a background launch runs when its caller names none: the built-in Claude
 *  starter with permission checks skipped, so a session nobody watches never sits waiting
 *  on a permission prompt. */
export const DEFAULT_UNATTENDED_AGENT = "builtin/claude-unattended";

/** The CLI the built-in Claude agent runs. */
export const DEFAULT_AGENT_CLI = "claude";

/**
 * The installed path of the agent CLI `binary`, or `null` when it isn't installed (or the
 * dependency probe hasn't resolved it yet). Asked of the backend at each call, which checks
 * the cached binary is still there. Keyed by the CLI's own basename, so an unresolved CLI is
 * never silently replaced by another one.
 */
export async function resolveAgentCliPath(
  binary: string = DEFAULT_AGENT_CLI,
): Promise<string | null> {
  return (await resolveAgentCli(binary)) ?? null;
}

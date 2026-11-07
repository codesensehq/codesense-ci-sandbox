// SPDX-License-Identifier: MIT
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { registeredCliOfBinary } from "@/shared/lib/agent-runtimes";

/** The agent CLIs the built-in adapters run, by binary, as their makers name them. */
export const AGENT_CLI_NAMES: Readonly<Record<string, string>> = {
  claude: "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  hermes: "Hermes Agent",
};

/** The maker's name for an agent CLI binary, or `undefined` when it isn't a known one. */
export function agentCliName(binary: string): string | undefined {
  return Object.prototype.hasOwnProperty.call(AGENT_CLI_NAMES, binary)
    ? AGENT_CLI_NAMES[binary]
    : undefined;
}

/** The install row that stands in for every agent of a missing CLI — the same line the
 *  Launch menu and Connections show, so the two never disagree. */
export function missingCliRow(binary: string): string {
  const preview = registeredCliOfBinary(binary)?.preview
    ? `. See Setting up agents → ${agentCliLabel(binary)} (preview) for the pinned source installation`
    : "";
  return `Install ${agentCliName(binary) ?? binary} — \`${binary}\` isn't on your PATH${preview}`;
}

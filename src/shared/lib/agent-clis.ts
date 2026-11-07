// SPDX-License-Identifier: MIT
/**
 * The agent CLIs CodeSense recognises running in a terminal tab, by executable name, with
 * the name people know each by. A tab whose shell runs one of these is an agent, however it
 * was started (typed by hand, before this app launched, through a node or python shim).
 * The one list: the tab-process probe watches for these on top of the built-in agent adapters,
 * and agent rows label their CLI from it.
 */
export const KNOWN_AGENT_CLIS = [
  { cli: "claude", label: "Claude" },
  { cli: "codex", label: "Codex" },
  { cli: "gemini", label: "Gemini" },
  { cli: "opencode", label: "OpenCode" },
  { cli: "aider", label: "Aider" },
  { cli: "cursor-agent", label: "Cursor Agent" },
] as const;

/** Every known agent CLI's executable name. */
export const KNOWN_AGENT_CLI_NAMES: readonly string[] = KNOWN_AGENT_CLIS.map((a) => a.cli);

/** An agent CLI's name for people: a known one's own label, else the executable capitalised
 *  (`claude` → `Claude`); `Agent` when nothing names it. */
export function agentCliLabel(cli: string | null | undefined): string {
  const name = cli?.trim();
  if (!name) return "Agent";
  const known = KNOWN_AGENT_CLIS.find((a) => a.cli === name.toLowerCase());
  return known ? known.label : name.charAt(0).toUpperCase() + name.slice(1);
}

// SPDX-License-Identifier: MIT
/**
 * What a person chose an agent to look like in the agents list: its role piece, its face and
 * the manager it shows under.
 * Display only — it never changes what the agent does. Absent fields mean Auto: the launch's
 * own choice, then its definition, then what the launch tree says.
 */

/** The roles a person can give an agent. */
export const AGENT_ROLE_CHOICES = [
  { id: "manager", label: "Manager" },
  { id: "worker", label: "Worker" },
  { id: "reviewer", label: "Reviewer" },
  { id: "qa", label: "QA" },
] as const;

export type AgentRoleChoice = (typeof AGENT_ROLE_CHOICES)[number]["id"];

export interface AgentIdentityChoice {
  /** Explicit tab rename, keyed by the PTY session id rather than the launch handle. */
  name?: string;
  role?: AgentRoleChoice;
  /** A face as `pack:id`, from any avatar pack. */
  face?: string;
  /** The agent this one shows under: a running agent's handle, or {@link NO_MANAGER} for the
   *  top level. Absent: whoever launched it. */
  manager?: string;
}

/** The manager choice that puts an agent at the top level, whoever launched it. */
export const NO_MANAGER = "@none";

/** A change to a choice: a value sets it, `null` returns that field to Auto. */
export interface AgentIdentityPatch {
  name?: string | null;
  role?: AgentRoleChoice | null;
  face?: string | null;
  manager?: string | null;
}

/** `current` with `patch` applied, or null when nothing is chosen any more. */
export function applyIdentityPatch(
  current: AgentIdentityChoice | undefined,
  patch: AgentIdentityPatch,
): AgentIdentityChoice | null {
  const next: AgentIdentityChoice = { ...current };
  for (const key of ["role", "face", "manager", "name"] as const) {
    if (!(key in patch)) continue;
    const value = patch[key];
    if (value === null || value === undefined) delete next[key];
    else (next as Record<string, string>)[key] = value;
  }
  return next.role || next.face || next.manager || next.name ? next : null;
}

// SPDX-License-Identifier: MIT
import type {
  AgentAccountChoice,
  AgentAccountResolutionReason,
  AgentDetailView,
  AgentRow,
  AgentRuntime,
} from "@/tauri/commands";
import { agentCliLabel } from "@/shared/lib/agent-clis";

/** One synced team's agents, in the order they render. */
export interface AgentTeamGroup {
  /** `org/team`, the group's identity. */
  id: string;
  label: string;
  rows: AgentRow[];
}

/** The list's groups: the caller's personal agents, one group per synced team, then the
 *  starters CodeSense ships. */
export interface AgentGroups {
  mine: AgentRow[];
  teams: AgentTeamGroup[];
  builtin: AgentRow[];
}

/** A starter CodeSense ships: read-only, copied to customize. */
export function isBuiltin(row: Pick<AgentRow, "scope">): boolean {
  return row.scope === "builtin";
}

/** Why a starter's change controls are off. */
export const BUILTIN_READ_ONLY = "Built-in template — use template to customize";

/** The starter the user's own CLI login points at: Codex when only Codex has an account
 *  set up, else Claude. `rows` are the starters' rows, each with its account preview. */
export function preferredStarterRuntime(rows: readonly AgentRow[]): AgentRuntime {
  const hasAccount = (runtime: AgentRuntime) =>
    rows.some((r) => isBuiltin(r) && r.runtime === runtime && r.account.account_id !== null);
  return hasAccount("codex") && !hasAccount("claude") ? "codex" : "claude";
}

/** Which starter a launch picks when no other agent comes first, and why, in words. */
export function starterDefaultNote(rows: readonly AgentRow[]): string {
  const why =
    preferredStarterRuntime(rows) === "codex"
      ? "Codex, since only Codex has an account set up"
      : "Claude, or Codex when only Codex has an account set up";
  return `When there are no other agents, a story's Launch agent menu and its shortcut start with ${why}. Any built-in agent can be launched from here or from that menu.`;
}

/** Rows ordered by name (then key, so equal names never swap): the order depends only on
 *  the agents themselves, never on selection or on what is running. */
function byName(a: AgentRow, b: AgentRow): number {
  return (
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.key.localeCompare(b.key)
  );
}

/** Split rows into Mine (the personal space), one group per team and the starters, each by
 *  name. Every row but a starter is fully usable and editable whichever group it is in. */
export function groupAgents(rows: readonly AgentRow[]): AgentGroups {
  const sorted = [...rows].sort(byName);
  const teams = new Map<string, AgentTeamGroup>();
  for (const row of sorted) {
    if (row.scope !== "team") continue;
    const id = `${row.org}/${row.team}`;
    const group = teams.get(id) ?? { id, label: teamLabel(row), rows: [] };
    group.rows.push(row);
    teams.set(id, group);
  }
  return {
    mine: sorted.filter((r) => r.scope === "personal"),
    teams: [...teams.values()].sort((a, b) => a.id.localeCompare(b.id)),
    builtin: sorted.filter(isBuiltin),
  };
}

/** A team as a group heading: its name, with the org it belongs to. */
function teamLabel(row: Pick<AgentRow, "org" | "team">): string {
  return `${row.team} · ${row.org}`;
}

/** Where an agent lives, in words: Personal, its team, or Built-in. */
export function placeLabel(row: Pick<AgentRow, "scope" | "org" | "team">): string {
  if (row.scope === "personal") return "Only me";
  if (row.scope === "builtin") return "Built-in";
  return teamLabel(row);
}

/** The runtime's product name. */
export function runtimeLabel(runtime: AgentRuntime): string {
  return agentCliLabel(runtime);
}

/** Why the account preview cannot launch, in words. */
export function accountReasonCopy(reason: AgentAccountResolutionReason): string {
  switch (reason) {
    case "detached":
      return "the bound account was detached";
    case "auth-required":
      return "the account needs you to sign in again";
    case "provider-mismatch":
      return "the bound account is for another CLI";
    case "generation-mismatch":
      return "the account changed after this launch was prepared";
    case "account-missing":
      return "the account no longer exists";
    case "required-missing":
      return "no account is set up for this CLI";
    case "focus-account-mismatch":
      return "the account doesn't match the focused one";
    default:
      return "the account can't be used right now";
  }
}

/** The account line under an agent: the account a launch would use (a pin by its name, the
 *  Active account marked "(Active)"), your own CLI login only when nothing is selected, or a
 *  warning when the preview says the launch could not use it. */
export type AccountLine = { kind: "ok"; text: string } | { kind: "warning"; text: string };

export function accountLine(account: AgentAccountChoice, runtime: AgentRuntime): AccountLine {
  if (account.provider === null) return { kind: "ok", text: "No CodeSense login" };
  if (account.store_error) {
    return { kind: "warning", text: `Accounts couldn't be read: ${account.store_error}` };
  }
  if (!account.spawn_eligible) {
    return { kind: "warning", text: `Can't launch: ${accountReasonCopy(account.reason)}` };
  }
  if (account.source === "binding") {
    return { kind: "ok", text: account.label ?? account.bound_account_id ?? "your account" };
  }
  if (account.resolved_source === "active" && account.label) {
    return { kind: "ok", text: `${account.label} (Active)` };
  }
  return { kind: "ok", text: `your own ${runtimeLabel(runtime)} login` };
}

function routines(n: number): string {
  return n === 1 ? "1 routine" : `${n} routines`;
}

/** "used by N routines", or that none use it. */
export function usedByLabel(row: Pick<AgentRow, "used_by" | "archived">): string {
  const n = row.used_by.length;
  if (row.archived) return n > 0 ? `archived, still used by ${routines(n)}` : "archived";
  return n > 0 ? `used by ${routines(n)}` : "not used by any routine";
}

/** Who created an agent: "you" for the caller, else their name. Attribution only. */
export function createdByLabel(row: Pick<AgentRow, "created_by" | "created_by_you">): string {
  return row.created_by_you ? "you" : row.created_by;
}

/** What the fact sheet reads of an agent beyond its definition: what it starts with, its
 *  permissions in words and how many manifest errors it has. */
export interface AgentSentence {
  startup: string;
  permissions: string;
  errorCount: number;
}

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

/** The permissions clause from the generated launch entry: Claude's own rules counted
 *  (CodeSense's required allows left out), Codex's approval and sandbox modes, each marked
 *  "default" when the agent did not set it. */
function permissionsClause(detail: AgentDetailView): string {
  const { definition, resolved } = detail;
  if (resolved.launch_admission?.permission_display)
    return resolved.launch_admission.permission_display;
  const generated = resolved.manifest.generated[0];
  if (definition.runtime === "opencode") {
    const { permissions: rules } = (generated?.content ?? {}) as {
      permissions?: { action: string; resource: string; effect: string }[];
    };
    const own = definition.permissions?.opencode?.permission ?? {};
    const customRules = Object.keys(own).length;
    let fallback = "ask";
    for (const rule of rules ?? []) {
      if (rule.action === "*" && rule.resource === "*") fallback = rule.effect;
    }
    return `${customRules} custom permission rules · unspecified tools ${fallback}`;
  }
  if (definition.runtime === "codex") {
    const content = (generated?.content ?? {}) as Record<string, unknown>;
    const own = definition.permissions?.codex ?? {};
    const mode = (key: "approval_policy" | "sandbox_mode", fallback: string) => {
      const value = content[key];
      if (typeof value !== "string") return `${fallback} (CLI default)`;
      return own[key] ? value : `${value} (default)`;
    };
    return `${mode("approval_policy", "approvals")} · ${mode("sandbox_mode", "sandbox")}`;
  }
  const rules = ((generated?.content ?? {}) as { permissions?: Record<string, unknown> })
    .permissions;
  const required = new Set(generated?.required ?? []);
  const allow = strings(rules?.allow).filter((r) => !required.has(r)).length;
  const deny = strings(rules?.deny).length;
  const ask = strings(rules?.ask).length;
  // The mode decides what runs without asking (a headless launch gets one by default), so
  // it is never left out of the summary.
  const mode = typeof rules?.defaultMode === "string" ? rules.defaultMode : null;
  const own = definition.permissions?.claude?.default_mode !== undefined;
  const modeText = mode ? ` · ${mode}${own ? "" : " (default)"}` : "";
  if (allow + deny + ask === 0) return `no permission rules of its own${modeText}`;
  const parts = [`${allow} allow`, `${deny} deny`];
  if (ask > 0) parts.push(`${ask} ask`);
  return `${parts.join(", ")} rules${modeText}`;
}

export function agentSentence(detail: AgentDetailView): AgentSentence {
  const { resolved } = detail;
  const manifest = resolved.manifest;
  return {
    startup: `${plural(manifest.entries.length, "path")} · ${plural(manifest.totals.files, "file")}`,
    permissions: permissionsClause(detail),
    errorCount: manifest.errors.length,
  };
}

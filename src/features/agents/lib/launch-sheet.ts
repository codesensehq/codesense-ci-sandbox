// SPDX-License-Identifier: MIT
import { create } from "zustand";

import {
  LOCAL_MACHINE_ID,
  type AgentLaunchOpenArgs,
  type AgentRow,
  type AgentSessionRow,
} from "@/tauri/commands";

/**
 * The launch sheet: one place a person launches an agent from, wherever they asked (the
 * agents list header, a right-click on a manager, an agent's page). Pure form rules here; the
 * sheet draws them, and the launch itself is the shipped targeted open agents use over MCP.
 */

/** Where the sheet was asked for, and what it starts with. */
export interface LaunchSheetRequest {
  /** The team the agent launches in. */
  teamKey: string;
  /** The agent definition to launch. */
  agentKey?: string;
  contextWorktreeId?: string;
  where?: LaunchWhere;
  name?: string;
  /** The existing hierarchy label retained when relaunching a session. */
  role?: string;
  /** Missing inherits the saved default; null explicitly joins no queue. */
  queueRole?: string | null;
  face?: string;
  /** The running agent it goes under (its handle); absent: none. */
  managerHandle?: string;
  /** The story it works on, when the opener knows one. */
  storyId?: string;
  /** What it should do, prefilled. */
  task?: string;
}

interface LaunchSheetState {
  request: LaunchSheetRequest | null;
  open: (request: LaunchSheetRequest) => void;
  close: () => void;
}

/** The one open launch sheet, app-wide. */
export const useLaunchSheetStore = create<LaunchSheetState>((set) => ({
  request: null,
  open: (request) => set({ request }),
  close: () => set({ request: null }),
}));

/** Open the launch sheet. */
export function openLaunchSheet(request: LaunchSheetRequest): void {
  useLaunchSheetStore.getState().open(request);
}

export function launchUnderAgent(row: AgentSessionRow, teamKey: string, task?: string): void {
  openLaunchSheet({
    teamKey,
    managerHandle: row.handle,
    storyId: row.story ?? undefined,
    ...(task === undefined ? {} : { task }),
  });
}

/** Where a launched agent works: a named tab in a story's workspace, or its own team folder. */
export type LaunchWhere = "story" | "team-workspace";

export interface LaunchSheetForm {
  agentKey: string;
  worktree?: { kind: "existing"; id: string } | { kind: "new"; name: string };
  /** The manager's handle; null for none. */
  manager: string | null;
  where: LaunchWhere;
  storyId: string;
  name: string;
  role?: string;
  /** Missing inherits the saved default; null explicitly joins no queue. */
  queueRole?: string | null;
  /** A face as `pack:id`; null for Auto. */
  face: string | null;
  /** What it should do: its first prompt. */
  task: string;
}

export type LaunchField = "agentKey" | "storyId" | "name" | "worktree";

/** The longest name and role label a launch records. */
export const LAUNCH_LABEL_MAX = 64;

/** The id an agent's key ends in. */
export function agentIdOfKey(key: string): string {
  return key.slice(key.lastIndexOf("/") + 1);
}

/** Whether a name can make a tab: it needs a letter or a digit. */
function hasWordChar(value: string): boolean {
  return /[\p{L}\p{N}]/u.test(value);
}

/** What is wrong with the form, field by field; empty when it can launch. */
export function validateLaunch(form: LaunchSheetForm): Partial<Record<LaunchField, string>> {
  const errors: Partial<Record<LaunchField, string>> = {};
  if (!form.agentKey) errors.agentKey = "Pick the agent to launch.";
  if (form.where === "story" && !form.storyId.trim()) {
    errors.storyId = "Pick the story it works on.";
  }
  if (form.where === "team-workspace" && form.worktree?.kind === "new") {
    const name = form.worktree.name.trim();
    if (!name) errors.worktree = "Name the worktree: it becomes its branch.";
    else if (!hasWordChar(name)) errors.worktree = "Use at least one letter or digit.";
    else if (name.length > LAUNCH_LABEL_MAX)
      errors.worktree = `Keep it to ${LAUNCH_LABEL_MAX} characters.`;
  }
  const name = form.name.trim();
  if (!name) errors.name = "Give it a name: it becomes the tab's name.";
  else if (!hasWordChar(name)) errors.name = "Use at least one letter or digit.";
  else if (name.length > LAUNCH_LABEL_MAX) {
    errors.name = `Keep it to ${LAUNCH_LABEL_MAX} characters.`;
  }
  return errors;
}

/** Why the Machine can't be chosen, and the machine it is held to; `null` when it can. */
export interface MachineLock {
  machineId: string;
  reason: string;
}

/**
 * The machine a launch is held to. A story's workspace stays where it was created, so a
 * story whose workspace exists launches on its machine (`place`: `"local"` or the machine
 * id where it lives, `null` when it has none, and the Machine then picks where it is
 * created). A team folder runs on This Mac.
 */
export function launchMachineLock(
  where: LaunchWhere,
  storyId: string,
  place: string | null,
  labelOf: (machineId: string) => string,
): MachineLock | null {
  if (where === "team-workspace")
    return { machineId: LOCAL_MACHINE_ID, reason: "A team folder runs on This Mac." };
  if (place === null) return null;
  return {
    machineId: place,
    reason: `${storyId.trim()}'s workspace lives on ${labelOf(place)}, so its agents run there.`,
  };
}

/** The form as the launch command takes it. Blank fields are left out. `machineId`: where
 *  a story launch runs. */
export function toLaunchArgs(form: LaunchSheetForm, machineId?: string): AgentLaunchOpenArgs {
  const task = form.task.trim();
  return {
    target: form.where,
    ...(form.where === "story" ? { storyId: form.storyId.trim() } : {}),
    ...(form.where === "story" && machineId ? { machineId } : {}),
    ...(form.where === "team-workspace" && form.worktree
      ? {
          worktree:
            form.worktree.kind === "existing" ? form.worktree.id : form.worktree.name.trim(),
          ...(form.worktree.kind === "new" ? { createWorktree: true } : {}),
        }
      : {}),
    name: form.name.trim(),
    agent: form.agentKey,
    ...(task ? { initialPrompt: form.task } : {}),
    ...(form.manager ? { launchedBy: form.manager } : {}),
    ...(form.role ? { role: form.role } : {}),
    ...(form.queueRole !== undefined ? { queueRole: form.queueRole } : {}),
    ...(form.face ? { avatar: form.face } : {}),
  };
}

/** The handle a launch would get, so a name already running there can be flagged before it
 *  launches. Mirrors the backend's slug rule. */
export function launchHandle(form: Pick<LaunchSheetForm, "where" | "storyId" | "name">): string {
  const slug = form.name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, LAUNCH_LABEL_MAX)
    .replace(/-+$/, "");
  return form.where === "story" ? `${form.storyId.trim()}#${slug}` : slug;
}

/**
 * A name for a new session of `agentName` that no running agent wears yet: the agent's own
 * name, then "Name 2", "Name 3"….
 */
export function suggestName(agentName: string, taken: Iterable<string>, separator = " "): string {
  const used = new Set([...taken].map((n) => n.trim().toLowerCase()));
  const base = agentName.trim() || "Agent";
  if (!used.has(base.toLowerCase())) return base;
  for (let n = 2; ; n++) {
    const next = `${base}${separator}${n}`;
    if (!used.has(next.toLowerCase())) return next;
  }
}

/** The agent a sheet opened without one starts on: the built-in Claude, else the first listed. */
export function defaultAgentKey(rows: readonly AgentRow[]): string {
  const live = rows.filter((r) => !r.archived);
  return (live.find((r) => r.key === "builtin/claude") ?? live[0])?.key ?? "";
}

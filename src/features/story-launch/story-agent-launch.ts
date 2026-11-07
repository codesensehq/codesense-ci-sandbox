// SPDX-License-Identifier: MIT
/**
 * One-action "Launch agent" for a story: ensure the story's worktree, wait for it
 * to settle, and start the chosen agent in its terminal with a prompt naming the
 * story file.
 *
 * This is NOT a new launch path. It drives the dashboard `openWorkspace` story
 * spine (`openStoryWorkspace` over `makeRealDashboardLaunchDeps`) — the same
 * worktree ensure, session bind, focus and seed the dashboards use, with the
 * named agent prepared by the backend for the session it spawns (its runtime,
 * revision, permissions and this user's account). The only difference is where
 * the story comes from: the story surfaces (nav row)
 * already hold the story's managed path, so the frontmatter lookup is replaced
 * by that known path.
 *
 * The menu lists the agents you can see (the Agents tab's list): the story's own
 * team first, then your personal agents, then other teams', then the starters
 * CodeSense ships (the one for your own CLI login first). The starters are always
 * there, so the menu is never empty. Nothing here names a vendor; an agent's
 * runtime picks its CLI.
 */

import { agentCliName, missingCliRow } from "@/shared/lib/agent-cli-names";
import {
  openStoryWorkspace,
  type DashboardLaunchDeps,
} from "@/features/dashboard/components/dashboard-launch";
import { makeRealDashboardLaunchDeps } from "@/features/dashboard/components/dashboard-launch-controller";
// New terminal's local agent launch reaches the dashboard spine through here, so the
// terminal seam never imports dashboard code itself.
export { launchAgentInWorkspace } from "@/features/dashboard/components/dashboard-launch-controller";
import { toast } from "sonner";

import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { userFacingErrorMessage } from "@/shared/lib/error-message";
import { isPipelineFileName } from "@/shared/lib/pipeline-graph/pipeline-file-filter";
import { getActiveTeamKey, NO_TEAM_SENTINEL, parseTeamKey } from "@/shared/lib/team-key";
import { preferredStarterRuntime } from "@/features/agents/lib/agent-display";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { parseStoryIdFromFrontmatter, storyIdFromPath } from "@/shared/lib/worktree/resolve";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  agentsList,
  getDependencyStatus,
  readFile,
  resolveAgentCli,
  type AgentRow,
  type AgentRuntime,
} from "@/tauri/commands";

/** Launcher provenance recorded on sessions this surface opens (the
 *  Background-agents panel shows which surface started a session). */
export const STORY_LAUNCH_LAUNCHER = "virtual://story-launch";

/** Whether the agent's CLI is installed, as the dependency cache knows it. */
export type LaunchAvailability = "available" | "missing" | "checking";

/** One agent on a story surface's "Launch agent" menu. */
export interface StoryLaunchAgent {
  /** The agent's key, which the launch names. */
  agentKey: string;
  name: string;
  runtime: AgentRuntime;
  /** The CLI basename the runtime runs. */
  binary: string;
  /** Where the agent lives, for the menu: `null` for the story's own team. */
  place: string | null;
  /** The first ordered agent. The shortcut runs the first AVAILABLE agent — see
   *  `groupStoryLaunchMenu().shortcutAgent`. */
  isDefault: boolean;
  availability: LaunchAvailability;
}

/** Which story to launch into. `storyId` is the frontmatter id when known. */
export interface StoryLaunchTarget {
  storyFilePath: string;
  storyId?: string | null;
}

/** What a story surface needs to render + act on its "Launch agent" menu. */
export interface StoryAgentLaunchActions {
  agents: readonly StoryLaunchAgent[];
  launch: (target: StoryLaunchTarget, agent: StoryLaunchAgent) => void;
  /** The default agent's keystroke, formatted for display (e.g. "⌘⇧A"). */
  shortcutLabel?: string;
}

/**
 * The initial prompt a launched agent receives. Identical wording to the task a
 * headless story run derives when no prompt is given, so an interactive and a
 * background launch of the same story ask the agent for the same thing.
 */
export function storyLaunchPrompt(storyFilePath: string): string {
  return `Read the story at ${storyFilePath} and complete it, satisfying every acceptance criterion.`;
}

/** The CLI an agent's runtime runs: the built-in runtime descriptors are named after it. */
function runtimeBinary(runtime: AgentRuntime): string {
  return runtime;
}

function byName(a: AgentRow, b: AgentRow): number {
  return (
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.key.localeCompare(b.key)
  );
}

/**
 * Order the agents for the menu: the story team's agents, then your personal agents, then
 * every other team's, each group by name, then the starters, the one for your own CLI login
 * first. So with no agents of your own, the default is that starter. Archived agents are left
 * out. Empty while the list is still loading (`rows` null).
 */
export function orderStoryLaunchAgents(
  rows: readonly AgentRow[] | null,
  teamKey: string,
): Omit<StoryLaunchAgent, "availability">[] {
  if (!rows) return [];
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const preferred = preferredStarterRuntime(rows);
  const rank = (row: AgentRow) => {
    // The default is the starter for your CLI login, by key, never whichever built-in
    // happens to sort first by name.
    if (row.scope === "builtin") return row.key === `builtin/${preferred}` ? 3 : 4;
    if (row.scope === "personal") return 1;
    return row.org === orgSlug && row.team === teamName ? 0 : 2;
  };
  const place = (row: AgentRow) => {
    if (rank(row) === 0) return null;
    if (row.scope === "builtin") return "built-in";
    return row.scope === "personal" ? "personal" : row.team;
  };
  return rows
    .filter((row) => !row.archived)
    .sort((a, b) => rank(a) - rank(b) || byName(a, b))
    .map((row, i) => ({
      agentKey: row.key,
      name: row.name,
      runtime: row.runtime,
      binary: runtimeBinary(row.runtime),
      place: place(row),
      isDefault: i === 0,
    }));
}

/** The "Launch agent" menu, grouped: one row per launchable agent, and one install row
 *  per missing CLI. */
export interface StoryLaunchMenu {
  /** The agents whose CLI is (or may be) on the PATH, in menu order. */
  agents: StoryLaunchAgent[];
  /** One row per agent CLI that isn't on the PATH, however many agents run it. */
  missing: { binary: string; label: string }[];
  /** The row the default-launch shortcut runs: the first available agent. */
  shortcutAgent: StoryLaunchAgent | null;
}

/** Group the ordered, availability-marked agents for display. Order is preserved. */
export function groupStoryLaunchMenu(agents: readonly StoryLaunchAgent[]): StoryLaunchMenu {
  const launchable: StoryLaunchAgent[] = [];
  const missing: { binary: string; label: string }[] = [];
  for (const agent of agents) {
    if (agent.availability === "missing") {
      if (!missing.some((row) => row.binary === agent.binary)) {
        missing.push({ binary: agent.binary, label: missingCliRow(agent.binary) });
      }
    } else {
      launchable.push(agent);
    }
  }
  return { agents: launchable, missing, shortcutAgent: launchable[0] ?? null };
}

/**
 * Toast a failed story launch. The spine's typed `unavailable:` prefix is for code
 * to branch on; the person reads only what follows it (e.g. the install hint).
 */
export function toastLaunchError(err: unknown, prefix = "Couldn't launch agent: "): void {
  toast.error(`${prefix}${userFacingErrorMessage(err)}`);
}

/**
 * The one-line install hint shown in place of a launch for a missing CLI. It names
 * the CLI, not the agent: several agents can run one CLI.
 */
export function missingCliHint(agent: Pick<StoryLaunchAgent, "binary">): string {
  const name = agentCliName(agent.binary);
  return name
    ? `Install the ${name} CLI (\`${agent.binary}\` isn't on your login-shell PATH)`
    : `Install \`${agent.binary}\` (it isn't on your login-shell PATH)`;
}

/**
 * The session handle for a story launch: its frontmatter `story_id` (the id the
 * dashboards and agents address a story by), read from the file when the surface
 * doesn't carry it, else the file stem. An id containing `#` is skipped: `#`
 * separates a story from a named session in a handle.
 */
export async function resolveStoryHandle(
  target: StoryLaunchTarget,
  read: (path: string) => Promise<string> = readFile,
): Promise<string> {
  const usable = (id: string | null | undefined) => {
    const trimmed = id?.trim();
    return trimmed && !trimmed.includes("#") ? trimmed : null;
  };
  const known = usable(target.storyId);
  if (known) return known;
  const fromFile = await read(target.storyFilePath)
    .then((content) => usable(parseStoryIdFromFrontmatter(content)))
    .catch(() => null);
  return fromFile ?? storyIdFromPath(target.storyFilePath);
}

/** Injectable seams (tests); production uses the real dashboard launch deps. */
export interface StoryLaunchSeams {
  deps?: DashboardLaunchDeps;
  readFile?: (path: string) => Promise<string>;
}

/**
 * Launch `agent` in the story's worktree, in one action. Creates the worktree in
 * the story's `source_repo` if missing, waits for its establish to settle, starts
 * the agent in the worktree terminal (or focuses the story's live session) and
 * seeds {@link storyLaunchPrompt}. Rejects with the spine's typed error
 * (`unavailable:`/`busy:`/`needs-attention:`) on failure.
 */
export async function launchAgentInStoryWorktree(
  teamKey: string,
  target: StoryLaunchTarget,
  agent: StoryLaunchAgent,
  seams: StoryLaunchSeams = {},
): Promise<{ sessionId: string; promptDelivered?: boolean }> {
  if (agent.availability === "missing") {
    throw new Error(`unavailable: ${missingCliHint(agent)}`);
  }
  const { storyFilePath } = target;
  const deps = seams.deps ?? makeRealDashboardLaunchDeps(STORY_LAUNCH_LAUNCHER);
  const handle = await resolveStoryHandle(target, seams.readFile);
  const launched = await openStoryWorkspace(
    handle,
    { ...deps, resolveStory: async () => ({ storyFilePath, teamKey }) },
    {
      headed: true,
      agent: agent.agentKey,
      initialPrompt: storyLaunchPrompt(storyFilePath),
    },
  );
  announceLaunchBehindMaximizedPanel(teamKey, handle, agent);
  return launched;
}

/**
 * The story's "Launch agent" menu: open the launch sheet on the story, with the chosen
 * agent and the story task prefilled, where the person picks its name, manager, role,
 * face and machine (the story's machine when its workspace exists). A missing CLI is
 * refused with its install hint instead.
 */
export async function openStoryLaunchSheet(
  teamKey: string,
  target: StoryLaunchTarget,
  agent: StoryLaunchAgent,
  read: (path: string) => Promise<string> = readFile,
): Promise<void> {
  if (agent.availability === "missing") {
    throw new Error(`unavailable: ${missingCliHint(agent)}`);
  }
  const storyId = await resolveStoryHandle(target, read);
  openLaunchSheet({
    teamKey,
    agentKey: agent.agentKey,
    storyId,
    task: storyLaunchPrompt(target.storyFilePath),
  });
}

/**
 * A launch from a maximized panel (the full-screen board) starts the agent in a terminal
 * the panel covers. Say so, with a Show that restores the layout — never a silent launch.
 */
export function announceLaunchBehindMaximizedPanel(
  teamKey: string,
  storyHandle: string,
  agent: Pick<StoryLaunchAgent, "name">,
): void {
  const store = useViewerStore.getState();
  if (!store.maximizedPanel?.[teamKey]) return;
  // Neutral wording: the launch may have focused the story's existing session instead.
  toast.success(`${agent.name} is running on ${storyHandle}`, {
    action: {
      label: "Show",
      onClick: () => useViewerStore.getState().setMaximized(teamKey, null),
    },
  });
}

/** Whether an agent's CLI is launchable, as the launch itself will judge it. */
export type LaunchProbe = (agent: Pick<StoryLaunchAgent, "binary">) => Promise<LaunchAvailability>;

/**
 * An agent's CLI as the launch itself will judge it (installed and at or above its
 * version floor), so the menu never offers a launch the backend would refuse. A cached
 * available CLI is taken as is; one whose PATH probe is still running is `checking`. A
 * cached unavailable CLI is asked of the backend's launch-time lookup once, which retries
 * a CLI that was not found, at startup or after it went missing, once its cooldown has
 * passed. A cache that cannot be read is left for the
 * launch itself to surface.
 */
export const probeAgentCli: LaunchProbe = (agent) =>
  getDependencyStatus()
    .then((deps) => {
      const entry = deps.agents[agent.binary];
      if (entry?.available && entry.path) return "available" as LaunchAvailability;
      if (entry?.resolving) return "checking" as LaunchAvailability;
      return resolveAgentCli(agent.binary).then(
        (path): LaunchAvailability => (path ? "available" : "missing"),
        (): LaunchAvailability => "missing",
      );
    })
    .catch((): LaunchAvailability => "available");

/**
 * The agent the one-action launch uses: the first item of the "Launch agent" menu whose
 * CLI is installed. A missing CLI never wins over an available one.
 * When none is available the first agent comes back marked `missing`, so the launch
 * refuses with its install hint. The starters are always listed, so there is always one.
 */
export async function defaultLaunchAgent(
  teamKey: string,
  probe: LaunchProbe = probeAgentCli,
): Promise<StoryLaunchAgent> {
  const rows = await agentsList(true);
  const candidates: StoryLaunchAgent[] = orderStoryLaunchAgents(rows, teamKey).map((agent) => ({
    ...agent,
    availability: "checking",
  }));
  if (candidates.length === 0) throw new Error("unavailable: the agents list came back empty");
  // One probe per distinct CLI, in menu order, stopping at the first available one.
  const byBinary = new Map<string, LaunchAvailability>();
  for (const candidate of candidates) {
    let availability = byBinary.get(candidate.binary);
    if (availability === undefined) {
      availability = await probe(candidate);
      byBinary.set(candidate.binary, availability);
    }
    if (availability !== "missing") return { ...candidate, availability: "available" };
  }
  return { ...candidates[0], availability: "missing" };
}

/**
 * The story the default-launch shortcut acts on: the focused story workspace,
 * else the active tab when it is a story file (one with a frontmatter
 * `story_id`). `null` when neither is a story.
 */
export async function activeStoryTarget(
  teamKey: string,
  read: (path: string) => Promise<string> = readFile,
): Promise<StoryLaunchTarget | null> {
  const { workspace } = getActiveWorkspaceContext();
  if (workspace?.kind === "story" && workspace.storyFilePath) {
    return { storyFilePath: workspace.storyFilePath, storyId: workspace.storyId ?? null };
  }
  const tab = useViewerStore.getState().activeTab[teamKey];
  if (!tab || !isPipelineFileName(tab.split(/[\\/]/).pop() ?? "")) return null;
  const storyId = await read(tab)
    .then((content) => parseStoryIdFromFrontmatter(content))
    .catch(() => null);
  return storyId ? { storyFilePath: tab, storyId } : null;
}

/**
 * The default-launch shortcut: launch the first available agent on the story in
 * focus. Toasts when nothing launchable is in focus or the launch fails.
 */
export async function launchDefaultAgentForActiveStory(): Promise<void> {
  const teamKey = getActiveTeamKey();
  if (teamKey === NO_TEAM_SENTINEL) return;
  try {
    const target = await activeStoryTarget(teamKey);
    if (!target) {
      toast.info("Open a story to launch an agent in its worktree");
      return;
    }
    await launchAgentInStoryWorktree(teamKey, target, await defaultLaunchAgent(teamKey));
  } catch (err) {
    toastLaunchError(err);
  }
}

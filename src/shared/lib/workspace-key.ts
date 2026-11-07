// SPDX-License-Identifier: MIT
/**
 * The terminal host's key for a CodeSense workspace — the ONE place a
 * `WorkspaceKey` is minted (grep-tested). A workspace id alone is not unique:
 * every team has its own `git-sync`/`notes` workspace and `root:owner/repo`,
 * and a `generic:<dir>` collides when two teams open one directory. So the key
 * carries the team: `${teamKey}|${form}`.
 *
 * A story workspace's form is its frontmatter `story_id`, not the file-stem
 * workspace id, so a story opened over MCP (which knows only the `story_id`)
 * lands in the same key as the story opened in the viewer. The Rust side mints
 * the same shape in `commands::pty_layout::workspace_key`; a contract test pins
 * the separator on both sides.
 */

/** Separates the team from the workspace form in a key. */
export const WORKSPACE_KEY_SEPARATOR = "|";

/** What a key is minted from: a workspace record, or just an id when the
 *  record is gone (an agent hidden without one). */
export interface WorkspaceKeySource {
  id: string;
  kind?: string;
  storyId?: string;
}

/** The workspace's form inside a key: its story id for a story, else its id. */
function workspaceForm(ws: WorkspaceKeySource): string {
  return ws.kind === "story" && ws.storyId ? ws.storyId : ws.id;
}

/** Mint the terminal host's key for `ws` in `teamKey`. */
export function workspaceKey(teamKey: string, ws: WorkspaceKeySource): string {
  return `${teamKey}${WORKSPACE_KEY_SEPARATOR}${workspaceForm(ws)}`;
}

/** Split a key into its team and workspace form (`null` for a key this app
 *  did not mint). A team key never holds the separator, so the first one
 *  splits. */
export function parseWorkspaceKey(key: string): { teamKey: string; form: string } | null {
  const at = key.indexOf(WORKSPACE_KEY_SEPARATOR);
  if (at <= 0 || at === key.length - 1) return null;
  return { teamKey: key.slice(0, at), form: key.slice(at + 1) };
}

/** The workspace id of a team's bucket of agents with no workspace: an agent whose tab and
 *  story name no workspace. No workspace record has it. */
export const NO_WORKSPACE_ID = "" as const;

/** The group key of a team's bucket of agents with no workspace. It has no separator, so it
 *  never equals a workspace's group key (a team key never holds the separator). */
const NO_WORKSPACE_GROUP = "#no-workspace";

/** The pane's identity for one workspace group: a pill, a flash and an arrow filter target it.
 *  {@link NO_WORKSPACE_ID} names the bucket of agents with no workspace, keyed apart from every
 *  workspace. */
export function workspaceGroupKey(teamKey: string, workspaceId: string): string {
  if (workspaceId === NO_WORKSPACE_ID) return `${teamKey}${NO_WORKSPACE_GROUP}`;
  return workspaceKey(teamKey, { id: workspaceId });
}

const WORKSPACE_FOLD = "#ws-fold:";
const MANAGER_FOLD = "#manager-fold:";
const TEAM_FOLD = "#team-fold:";

/** The key a workspace card folds under, per team. Folded cards are stored as `true`. */
export function workspaceFoldKey(teamKey: string, workspaceId: string): string {
  return `${WORKSPACE_FOLD}${workspaceGroupKey(teamKey, workspaceId)}`;
}

/** The key a team header folds under. Folded teams are stored as `true`. */
export function teamFoldKey(teamKey: string): string {
  return `${TEAM_FOLD}${teamKey}`;
}

/** A manager's descendants fold independently of the workspace cards. */
export function managerFoldKey(teamKey: string, handle: string): string {
  return `${MANAGER_FOLD}${teamKey}${WORKSPACE_KEY_SEPARATOR}${handle}`;
}

/** The team a fold key names (a team, workspace card or manager), or null for a
 *  key this app did not mint. A card's group with a separator is a workspace's, whatever its id
 *  ends with (a team key never holds the separator); only a group without one is a team's
 *  no-workspace bucket. */
export function teamOfFoldKey(key: string): string | null {
  if (key.startsWith(TEAM_FOLD)) return key.slice(TEAM_FOLD.length) || null;
  if (key.startsWith(MANAGER_FOLD))
    return parseWorkspaceKey(key.slice(MANAGER_FOLD.length))?.teamKey ?? null;
  if (!key.startsWith(WORKSPACE_FOLD)) return null;
  const group = key.slice(WORKSPACE_FOLD.length);
  if (group.includes(WORKSPACE_KEY_SEPARATOR)) return parseWorkspaceKey(group)?.teamKey ?? null;
  if (!group.endsWith(NO_WORKSPACE_GROUP)) return null;
  return group.slice(0, -NO_WORKSPACE_GROUP.length) || null;
}

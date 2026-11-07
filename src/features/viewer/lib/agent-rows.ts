// SPDX-License-Identifier: MIT
import type { AgentSessionRow } from "@/tauri/commands";

/**
 * SW-AM-5 — the agents list's scale layer: a stable order over the session rows,
 * so the list stays legible at 12+ sessions. Pure and view-agnostic (no store, no
 * clock) so the ordering is unit-tested without a render and can never reorder on
 * the list's 1s "elapsed" tick.
 *
 * `foreground` is a VIEW derivation the caller computes per row (an `alive`
 * session with a live visible tab). It is passed in — not re-derived here — so
 * this module needs no knowledge of the tab map.
 */
interface OrderedAgentRow {
  row: AgentSessionRow;
  /** `true` when this session is shown as a live tab right now (alive + hasLiveTab). */
  foreground: boolean;
}

// Order group: foreground first (watched), then background-alive, then stopped.
function orderGroup(r: OrderedAgentRow): 0 | 1 | 2 {
  if (!r.row.alive) return 2;
  return r.foreground ? 0 : 1;
}

/**
 * SW-AM-5 (AC-1/AC-4): a stable total order for the panel rows. Keys ONLY on
 * stable fields — never `elapsed`/`now` — so a per-second tick or a same-content
 * refetch (new array, identical rows) produces an identical order (no reflow
 * thrash as SW-AM-4 progress ticks arrive). Foreground → background-alive →
 * stopped; within a group most-recent first (`startedAtMs` desc, null anchors
 * last); `handle` asc as the final deterministic tiebreak. Returns a new array
 * (does not mutate the input).
 */
export function orderAgentRows<T extends OrderedAgentRow>(rows: T[]): T[] {
  return [...rows].sort((a, b) => {
    const group = orderGroup(a) - orderGroup(b);
    if (group !== 0) return group;
    const at = a.row.startedAtMs;
    const bt = b.row.startedAtMs;
    if (at !== bt) {
      // most-recent first; a null (unknown) birth anchor sorts last in its group.
      if (at === null) return 1;
      if (bt === null) return -1;
      return bt - at;
    }
    // Total, deterministic tiebreak so two rows sharing a birth anchor never swap.
    return a.row.handle < b.row.handle ? -1 : a.row.handle > b.row.handle ? 1 : 0;
  });
}

/** The named agent a session runs: the key of its `<key>@<revision>` stamp, or null for a
 *  session that names none. */
export function agentKeyOfRevision(stamp: string | null | undefined): string | null {
  if (!stamp) return null;
  const at = stamp.lastIndexOf("@");
  return at > 0 ? stamp.slice(0, at) : null;
}

// --- SW-AV-2: fleet agents ---------------------------------------------------

/**
 * SW-AV-2: the kinds that are NOT fleet agents — bare/utility sessions the agents
 * list leaves out so "empty → cluttered" never happens. The set is the ONE definition of "not a real fleet
 * agent":
 *  - `live-pty` — SW-AV-1's `KIND_LIVE_PTY` synthetic row: a registry-less live PTY
 *    (a shell/worktree opened outside the launch registry) with no launch metadata.
 *  - `root` / `notes` / `#` — the workspace-utility shells (a repo-root shell,
 *    the notes scratch pad, a bare `#` scratch).
 * A `registered` row with no named agent and no detected one is a shell too
 * ({@link isPlainRegistration}). Everything else — a `dashboard-launched` agent, a
 * `registered` one that names or runs an agent, a `headless` run, a Weft
 * `controller`/worker — is a real fleet agent.
 */
export const UTILITY_SESSION_KINDS: ReadonlySet<string> = new Set([
  "live-pty",
  "root",
  "notes",
  "#",
]);

/** SW-AV-2: is this a bare/utility session (not a fleet agent)? Keys on
 *  `kind` ALONE via {@link UTILITY_SESSION_KINDS} — never the handle — so a fork
 *  handle like `SW-DX-3#attempt-2` (a real agent whose handle contains `#`) is never
 *  misclassified as a `#` scratch. Pure. */
export function isUtilitySession(row: Pick<AgentSessionRow, "kind">): boolean {
  return UTILITY_SESSION_KINDS.has(row.kind);
}

/** A shell the launch registry holds only so it can be addressed (a tab opened over
 *  MCP, a workspace's main shell): it names no agent and none is detected running in
 *  it. It is a fleet agent only while one runs there. Mirrors Rust `plain_shell`. Pure. */
export function isPlainRegistration(
  row: Pick<AgentSessionRow, "kind" | "agentRevision" | "detected">,
): boolean {
  return row.kind === "registered" && !row.agentRevision && !row.detected;
}

/** SW-AV-2: what qualifies as a FLEET AGENT — a real managed agent that ranks in
 *  the primary list, as opposed to a bare/utility session ({@link isUtilitySession})
 *  or a plain registered shell ({@link isPlainRegistration}). The demotion predicate,
 *  defined once. Pure. */
export function isFleetAgent(
  row: Pick<AgentSessionRow, "kind" | "agentRevision" | "detected">,
): boolean {
  return !isUtilitySession(row) && !isPlainRegistration(row);
}

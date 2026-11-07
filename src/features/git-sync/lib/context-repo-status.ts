// SPDX-License-Identifier: MIT
import type { ContextConflictSide, ContextRepoStatus, ContextSyncState } from "@/tauri/commands";

/** Branch names that read as the repo's main line (drawn plain, not as a filled chip). */
const MAIN_BRANCHES = new Set(["main", "master"]);

export function isMainBranch(branch: string | null): boolean {
  return branch !== null && MAIN_BRANCHES.has(branch);
}

/** How the branch half of the button is drawn. */
export type BranchLook = "main" | "branch" | "detached";

/** Everything the status button shows, derived once. */
export interface StatusButtonModel {
  look: BranchLook;
  /** The branch name, or the short sha when detached. */
  branchLabel: string;
  /** ↑: changes GitHub doesn't have (uncommitted edits + unpushed commits). */
  up: number;
  /** A local branch GitHub has never seen: ↑ reads "new". */
  upIsNew: boolean;
  /** ↓: new on GitHub. */
  down: number;
  conflicts: number;
  state: ContextSyncState;
  /** The short word an abnormal state takes when there is room. */
  stateWord: string | null;
  /** Nothing to say: no counts, no conflict, a normal state. */
  clean: boolean;
  /** Does a click open the card (a conflict or a failure) instead of Changes? */
  opensCard: boolean;
  /** Changes GitHub lacks with no remote to send them: the card offers Set up. */
  unsent: boolean;
  /** The full sentence for the tooltip and the accessible name. */
  sentence: string;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** "2m ago" / "3h ago" / "Sep 28". */
export function agoText(ms: number, nowMs: number): string {
  const s = Math.max(0, Math.round((nowMs - ms) / 1000));
  if (s < 60) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  if (d < 7) return `${d}d ago`;
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

const STATE_WORD: Record<ContextSyncState, string | null> = {
  ok: null,
  syncing: null,
  failed: "failed",
  offline: "offline",
  signin: "sign in",
  retrying: null,
};

/**
 * The status button's model. `uncommitted` is the count of changed files in scope; the
 * graph's `ahead` adds the commits not pushed. `status.conflicts` decides the conflict count.
 */
export function statusButtonModel(
  status: ContextRepoStatus,
  uncommitted: number,
  nowMs: number,
): StatusButtonModel {
  if (status.worktree) {
    const owner = status.owner
      ? ` · Owner: ${status.owner.kind === "person" ? "you" : status.owner.name}`
      : "";
    const agents = status.agents?.length
      ? ` · ${status.agents.map((a) => a.name).join(" and ")} working here`
      : "";
    const conflicts = status.conflicts.length;
    const { state } = status.sync;
    const operation = status.sync.reason ?? status.sync.progress ?? STATE_WORD[state];
    const attention = [
      conflicts > 0 ? `${plural(conflicts, "conflict")} to settle` : null,
      operation ?? (state === "syncing" ? "Git operation in progress" : null),
    ]
      .filter(Boolean)
      .map((text) => ` · ${text}`)
      .join("");
    return {
      look: "branch",
      branchLabel: status.branch ?? status.headSha.slice(0, 7),
      up: status.ahead,
      down: status.behind,
      upIsNew: false,
      conflicts,
      state,
      stateWord: STATE_WORD[state],
      clean: status.ahead === 0 && status.behind === 0 && conflicts === 0 && state === "ok",
      opensCard: true,
      unsent: false,
      sentence: `${status.worktree.name} · branch ${status.branch ?? "detached"} · ${status.ahead} commits ahead of main, ${status.behind} behind · ${status.uncommitted ?? 0} uncommitted files${attention}${owner}${agents}`,
    };
  }
  const detached = status.branch === null;
  const look: BranchLook = detached ? "detached" : isMainBranch(status.branch) ? "main" : "branch";
  const branchLabel = status.branch ?? status.headSha;
  const upIsNew = !detached && !status.hasUpstream;
  const up = uncommitted + status.ahead;
  const down = status.behind;
  const conflicts = status.conflicts.length;
  const { state } = status.sync;
  const clean = up === 0 && down === 0 && conflicts === 0 && !upIsNew && state === "ok";
  // No remote is only a decision when there is something it would have sent: an org kept on
  // this Mac with nothing changed opens Changes like any other.
  const unsent = !!status.sync.noSender && up > 0;
  const opensCard = conflicts > 0 || state === "failed" || state === "signin" || unsent;

  const parts: string[] = [];
  if (detached) parts.push(`On no branch (detached at ${status.headSha})`);
  else if (upIsNew) parts.push(`On ${status.branch}, which GitHub has never seen`);
  else parts.push(`On ${status.branch}${look === "branch" ? ", not main" : ""}`);
  if (up > 0) {
    const bits: string[] = [];
    if (uncommitted > 0) bits.push(plural(uncommitted, "uncommitted edit"));
    if (status.ahead > 0) bits.push(plural(status.ahead, "commit") + " not pushed");
    parts.push(`${plural(up, "change")} not on GitHub (${bits.join(" + ")})`);
  }
  if (down > 0) parts.push(`${down} new on GitHub`);
  if (conflicts > 0) parts.push(`${plural(conflicts, "conflict")} to settle`);
  if (unsent) parts.push("nothing on this Mac will send these changes");
  const ago = status.sync.lastSyncedAt ? agoText(status.sync.lastSyncedAt, nowMs) : null;
  const since = ago ? `, last synced ${ago}` : "";
  switch (state) {
    case "ok":
      if (detached) parts.push("nothing syncs until an agent puts it back on a branch");
      else if (ago) parts.push(`last synced ${ago}`);
      break;
    case "syncing":
      parts.push(status.sync.progress ? `syncing: ${status.sync.progress}` : "syncing");
      break;
    case "failed":
      parts.push(`sync failed${status.sync.reason ? ` (${status.sync.reason})` : ""}${since}`);
      break;
    case "offline":
      parts.push(`offline${since}`);
      break;
    case "signin":
      parts.push(`signed out of GitHub${since}`);
      break;
    case "retrying":
      parts.push(
        `retrying${
          status.sync.attempt
            ? `, attempt ${status.sync.attempt}${status.sync.maxAttempts ? ` of ${status.sync.maxAttempts}` : ""}`
            : ""
        }${since}`,
      );
      break;
  }
  const sentence = parts.join("; ");

  return {
    look,
    branchLabel,
    up,
    upIsNew,
    down,
    conflicts,
    state,
    stateWord: STATE_WORD[state],
    clean,
    opensCard,
    unsent,
    sentence: sentence.charAt(0).toUpperCase() + sentence.slice(1) + ".",
  };
}

/** One side of a conflict in a line: "You: Q4 table, 40m ago." / "maya deleted it, 20m ago." */
export function conflictSideLine(
  side: ContextConflictSide,
  fallbackWho: string,
  nowMs: number,
): string {
  const who = side.who ?? fallbackWho;
  const ago = side.at ? agoText(side.at, nowMs) : null;
  if (side.deleted) return `${who} deleted it${ago ? `, ${ago}` : ""}.`;
  return `${who}: ${[side.subject, ago].filter(Boolean).join(", ") || "changed it"}.`;
}

/** A long branch keeps its start and its end: `gs/_bmad-o…d735ccb`. */
export function elideMiddle(name: string, max: number): string {
  if (name.length <= max) return name;
  const tail = Math.ceil((max - 1) / 2);
  const head = max - 1 - tail;
  return `${name.slice(0, head)}…${name.slice(name.length - tail)}`;
}

// SPDX-License-Identifier: MIT
/**
 * SW-XS-27: "unsaved changes" for team files edited inside workspaces — the
 * words and small decisions the counter and its list share. Pure: no store, no
 * commands, so every sentence is unit-testable.
 */
import { logger } from "@/shared/lib/logger";
import type {
  ForgetMissingWorkspaceResult,
  TeamFileEdit,
  TeamFileEditDestination,
  TeamFileEditKind,
  TeamFileEditsStatus,
  TeamFileEditsWorkspace,
} from "@/tauri/commands";

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The counter's full sentence (tooltip + accessible name). */
export function counterSentence(status: TeamFileEditsStatus): string {
  const gone = status.workspaces.filter((w) => w.state === "missing").length;
  const missing = gone ? plural(gone, "workspace folder") + " gone" : "";
  const listed = status.workspaces.filter((w) => editCount(w) > 0).length;
  const conflicts = status.workspaces.reduce(
    (n, w) =>
      n +
      (w.state === "missing"
        ? 0
        : (w.files?.filter((file) => file.kind === "conflict").length ?? 0)),
    0,
  );
  if (status.total === 0) {
    const unsure = status.workspaces.filter(
      (w) => w.state === "unfinishedCopy" || w.state === "unknown",
    ).length;
    return (
      [
        unsure > 0 ? `Couldn't check ${plural(unsure, "workspace")} for unsaved team edits` : "",
        missing,
      ]
        .filter(Boolean)
        .join("; ") || "No unsaved team edits"
    );
  }
  const both = conflicts > 0 ? `; ${conflicts} changed here and in the team too` : "";
  return `${plural(status.total, "unsaved team edit")} in ${plural(listed, "workspace")}: team files changed in a workspace and not in the team yet${both}${missing ? `; ${missing}` : ""}`;
}

/** How many edits a workspace holds. */
export function editCount(ws: TeamFileEditsWorkspace): number {
  return ws.state === "missing" ? 0 : (ws.files?.length ?? 0);
}

/** Whether the counter shows at all: something to say. */
export function counterVisible(status: TeamFileEditsStatus | null | undefined): boolean {
  if (!status) return false;
  return (
    status.total > 0 ||
    status.workspaces.some(
      (w) => w.state === "unfinishedCopy" || w.state === "unknown" || w.state === "missing",
    )
  );
}

/** "2m ago" / "3h ago" / "Sep 28" — the same clock words the GitHub button uses. */
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

/** The one-letter status chip for a kind, in the changed-file vocabulary the app uses
 *  elsewhere (letter + color + word, never color alone). */
export const KIND_CHIP: Record<TeamFileEditKind, { letter: string; cssVar: string; word: string }> =
  {
    changed: { letter: "M", cssVar: "--cs-status-modified", word: "changed" },
    new: { letter: "A", cssVar: "--cs-status-added", word: "new" },
    removed: { letter: "D", cssVar: "--cs-status-deleted", word: "removed here" },
    conflict: { letter: "!", cssVar: "--cs-git-conflict", word: "changed here and in the team" },
    cannotPublish: { letter: "—", cssVar: "--muted-foreground", word: "can't be published" },
  };

/** Where a file publishes to, when that is not simply this team (`null` then). */
export function destinationNote(dest: TeamFileEditDestination): string | null {
  switch (dest.kind) {
    case "team":
      return null;
    case "parentTeam":
      return `goes to ${dest.name}`;
    case "private":
      return "goes to your private files";
    case "repo":
      return "goes to this repo's files";
    case "other":
      return `goes to ${dest.label}`;
  }
}

/** The second line of a file row, when it needs one. Engine reasons are never shown raw
 *  for a conflict (they carry hashes); a can't-publish reason is already plain. */
export function fileNote(file: TeamFileEdit): string | null {
  if (file.kind === "conflict") return "Changed here and in the team since it was copied";
  if (file.kind === "cannotPublish")
    return file.discardable
      ? "Too large to publish. Copy it into the team by hand, or discard it."
      : "Too large to publish or put back. Copy it by hand.";
  return destinationNote(file.destination);
}

/** What Discard does to this file, said before it is done. */
export function discardSentence(file: TeamFileEdit): string {
  const name = baseName(file.path);
  switch (file.kind) {
    case "new":
      return `Delete ${name} from this workspace?`;
    case "removed":
      return `Put the team's ${name} back in this workspace?`;
    default:
      return `Throw away the changes to ${name} here and use the team's version?`;
  }
}

/** Which actions a file row offers. */
export function fileActions(file: TeamFileEdit): {
  view: boolean;
  publish: boolean;
  discard: boolean;
} {
  switch (file.kind) {
    case "conflict":
      // A publish refuses it: the person decides (View, then Discard to take the team's).
      return { view: true, publish: false, discard: true };
    case "cannotPublish":
      // Too large to move or show here: listed so it is never invisible; Discard
      // when it can still be settled (a new file deleted, a copy restored).
      return { view: false, publish: false, discard: !!file.discardable };
    case "removed":
      return { view: false, publish: true, discard: true };
    default:
      return { view: true, publish: true, discard: true };
  }
}

/** The files "Publish all" sends: changed and new ones (a removal or a conflict is
 *  always its own decision). */
export function publishAllPaths(ws: TeamFileEditsWorkspace): string[] {
  return (ws.files ?? [])
    .filter((f) => f.kind === "changed" || f.kind === "new")
    .map((f) => f.path);
}

/** The workspace's state line, or `null` when it was just counted on this Mac. */
export function workspaceLine(ws: TeamFileEditsWorkspace, nowMs: number): string | null {
  const remote = ws.target.kind === "remote";
  const age = ws.checkedAtMs ? agoText(ws.checkedAtMs, nowMs) : null;
  switch (ws.state) {
    case "checked":
      return remote && age ? `on a machine · checked ${age}` : null;
    case "missing": {
      const machine =
        ws.machineName ?? (ws.target.kind === "remote" ? ws.target.machineId : "the machine");
      const count = Object.values(ws.counts ?? {}).reduce((n, value) => n + value, 0);
      return `folder no longer exists on ${machine}${count ? ` · its ${plural(count, "unsaved team edit")} went with the folder` : ""}`;
    }
    case "notChecked":
      return "on a machine · not checked yet";
    case "offline":
      return editCount(ws) > 0 && age
        ? `machine offline · last count ${age}`
        : "machine offline · not checked yet";
    case "unfinishedCopy":
      return "an earlier copy to this workspace didn't finish; check again after it does";
    case "unknown": {
      const pathReason = PATH_REASONS.find(([pattern]) => pattern.test(ws.reason ?? ""));
      return pathReason
        ? `couldn't check: ${pathReason[1]}`
        : "couldn't check this workspace; it may still hold unsaved edits";
    }
  }
}

/** Split a path into its name and the folder it sits in. */
export function baseName(path: string): string {
  return path.split("/").pop() ?? path;
}
export function dirName(path: string): string {
  const i = path.lastIndexOf("/");
  return i > 0 ? path.slice(0, i) : "";
}

/** A stable key for a workspace. */
export function workspaceKey(ws: TeamFileEditsWorkspace): string {
  return ws.target.kind === "local"
    ? `local:${ws.target.codeRepoPath}`
    : `remote:${ws.target.machineId}:${ws.target.workspaceHandle}`;
}

/** Engine reasons the list can say in its own words. */
const PATH_REASONS: [RegExp, string][] = [
  [
    /^the workspace path on the machine isn't a normal folder$/,
    "the workspace path on the machine isn't a normal folder",
  ],
  [/^something else is at the workspace path$/, "something else is at the workspace path"],
];
const PLAIN: [RegExp, string][] = [
  ...PATH_REASONS,
  [/changed since (it was looked at|the preview|you looked)/i, "it changed since you looked"],
  [/team file changed since/i, "the team changed it too"],
  [/no longer in team content|team no longer has/i, "the team no longer has this file"],
  [/tracked by the repo/i, "the repository tracks this file; commit it instead"],
  [/is (\d+) MiB|files up to \d+ MiB|too large/i, "it is too large"],
  [
    /machine.*(not connected|unreachable|offline)|admitted remote connection unavailable/i,
    "the machine is offline",
  ],
  [/not one of this team's workspaces/i, "this workspace is no longer one of this team's"],
  [/cancel/i, "it was stopped"],
];

/** Plain words for a failure: a known engine reason in the list's own words, else a
 *  generic line. Engine text (tool names, hashes, paths) never reaches the screen;
 *  it goes to the log. */
export function plainError(message: string): string {
  for (const [pattern, words] of PLAIN) if (pattern.test(message)) return words;
  logger.warn("team-edits", `action failed: ${message}`);
  return "it didn't go through (details are in the log)";
}

/** A stable signature of the workspaces that couldn't be checked. */
export function unsureSignature(status: TeamFileEditsStatus): string {
  return status.workspaces
    .filter((w) => w.state === "unfinishedCopy" || w.state === "unknown" || w.state === "missing")
    .map((w) => `${workspaceKey(w)}:${w.state}`)
    .sort()
    .join("|");
}

/** Plain refusal copy shared by every removal response. */
export function removalRefusal(
  result: Extract<ForgetMissingWorkspaceResult, { status: "refused" }>,
  machine: string,
): string {
  if (result.reason === "updateTerminalServer")
    return `Not removed: update the terminal server on ${machine} first.`;
  if (result.state === "checked") return "Not removed: the folder is there again.";
  if (result.state === "offline")
    return `Not removed: ${machine} went offline. Try again when it's back.`;
  return "Not removed: couldn't confirm the folder is gone.";
}

// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { PublishSet, FileRedline } from "@/features/git-sync/lib/types";
import type { ContextConflict, ContextRepoStatus, HistoryBranches } from "@/tauri/commands";
import { MOCK_TEAM_NAME, mockOrg } from "../fixtures";
import { isLeftNavDemo } from "./left-nav-demo";
import { navGitCommitFiles, navGitHistory } from "./nav-git-history";

/**
 * The context repo's git world for the left nav's status button and Changes view, in
 * `?nav=demo`: a changed-file set mirroring the design (a modified story, an added dashboard,
 * a deleted image, a rename, a repo-tier file, a conflict) and the status the button reads.
 *
 * `?git=` picks the status, so every state can be walked:
 *   clean · changes (default) · syncing · behind · diverged · failed · offline · signin ·
 *   retrying · nosender · conflict · notmain · detached · noupstream
 * The conflict states also mark `roadmap.html` as conflicted.
 */
export type NavGitState =
  | "clean"
  | "changes"
  | "syncing"
  | "behind"
  | "diverged"
  | "failed"
  | "offline"
  | "signin"
  | "retrying"
  | "nosender"
  | "conflict"
  | "notmain"
  | "detached"
  | "noupstream";

const PREFIX = `${MOCK_TEAM_NAME}/content`;
const rel = (p: string) => `${PREFIX}/${p}`;

const MINUTE = 60_000;

function changedSet(): PublishSet {
  return {
    buckets: [],
    general: {
      count: 6,
      files: [
        { path: rel("26-agent-inbox/AID-4.md"), status: "modified", added: 41, removed: 6 },
        {
          path: rel("26-agent-inbox/dashboards/inbox-board.html"),
          status: "added",
          added: 88,
          removed: 0,
        },
        { path: rel("26-agent-inbox/rail-mock.png"), status: "deleted" },
        { path: rel("27-left-nav/LN-1.md"), status: "modified", added: 9, removed: 2 },
        {
          path: rel("27-left-nav/icons.md"),
          status: "modified",
          renamedFrom: rel("27-left-nav/icon-spec.md"),
          badges: ["rename"],
          added: 0,
          removed: 0,
        },
        { path: rel("repos/widgets/CLAUDE.md"), status: "modified", added: 3, removed: 1 },
      ],
    },
  };
}

function roadmapConflict(now: number): ContextConflict {
  return {
    path: rel("roadmap.html"),
    mine: { who: "You", subject: "Q4 table", at: now - 40 * MINUTE, deleted: false },
    theirs: { who: "maya", subject: "moved the table", at: now - 20 * MINUTE, deleted: false },
    lastWriter: "claude",
    heldBack: [rel("team.yaml"), rel("metrics.csv")],
  };
}

export function navGitStatus(state: NavGitState, now: number): ContextRepoStatus {
  const base: ContextRepoStatus = {
    branch: "main",
    headSha: "394fa6d",
    hasUpstream: true,
    upstream: "origin/main",
    remoteProvider: "github",
    ahead: 1,
    behind: 0,
    sync: { state: "ok", lastSyncedAt: now - 2 * MINUTE },
    conflicts: [],
  };
  switch (state) {
    case "clean":
      return { ...base, ahead: 0 };
    case "changes":
      return base;
    case "syncing":
      return {
        ...base,
        sync: { state: "syncing", lastSyncedAt: now - 2 * MINUTE, progress: "Pulling 3 of 7" },
      };
    case "behind":
      return { ...base, behind: 2 };
    case "diverged":
      return { ...base, ahead: 3, behind: 2 };
    case "failed":
      return {
        ...base,
        sync: {
          state: "failed",
          lastSyncedAt: now - 47 * MINUTE,
          reason: "The team's copy refused the send: branch protection requires a review",
        },
      };
    case "offline":
      return { ...base, sync: { state: "offline", lastSyncedAt: now - 3 * 60 * MINUTE } };
    case "signin":
      return { ...base, sync: { state: "signin", lastSyncedAt: now - 3 * 60 * MINUTE } };
    case "retrying":
      return {
        ...base,
        sync: {
          state: "retrying",
          lastSyncedAt: now - 9 * MINUTE,
          reason: "The network dropped mid-send",
          attempt: 2,
          maxAttempts: 5,
        },
      };
    case "nosender":
      return { ...base, ahead: 0, sync: { state: "ok", lastSyncedAt: null, noSender: true } };
    case "conflict":
      return { ...base, conflicts: [roadmapConflict(now)] };
    case "notmain":
      return { ...base, branch: "gs/_bmad-output-d735ccb", ahead: 2 };
    case "detached":
      return { ...base, branch: null, ahead: 1 };
    case "noupstream":
      return {
        ...base,
        branch: "feat-left-nav-search",
        hasUpstream: false,
        upstream: null,
        ahead: 0,
      };
  }
}

const REDLINES: Record<string, FileRedline> = {
  [rel("26-agent-inbox/AID-4.md")]: {
    status: "modified",
    binary: false,
    before:
      "# Dashboards from the inbox\n\nEach dashboard reads the inbox rows it was given. Unread rows sit on top and pulse.\n\nOpening a row opens its thread in the right pane.\n",
    after:
      "# Dashboards from the inbox\n\nEach dashboard reads the inbox rows it was given. Rows from an agent show its face; rows from a person show their name. Unread rows sit on top.\n\nOpening a row opens its thread in the right pane.\n",
  },
  [rel("roadmap.html")]: {
    status: "modified",
    binary: false,
    before: "<h1>Roadmap</h1><p>Q4: inbox, left nav, routines.</p>",
    after: "<h1>Roadmap</h1><p>Q4: inbox, left nav, remote machines, routines.</p>",
  },
};

function resolveState(search: string): NavGitState {
  const raw = new URLSearchParams(search).get("git") ?? "changes";
  const known: NavGitState[] = [
    "clean",
    "changes",
    "syncing",
    "behind",
    "diverged",
    "failed",
    "offline",
    "signin",
    "retrying",
    "nosender",
    "conflict",
    "notmain",
    "detached",
    "noupstream",
  ];
  return (known as string[]).includes(raw) ? (raw as NavGitState) : "changes";
}

export function navGitHandlers(search: string): HandlerRegistry {
  if (!isLeftNavDemo(search)) return {};
  const state = resolveState(search);
  const root = mockOrg.org_root;
  let conflicts = navGitStatus(state, Date.now()).conflicts;
  const clean = state === "clean" || state === "behind" || state === "detached";
  return {
    get_publish_set: (args) =>
      args.commitSha
        ? navGitCommitFiles(args.commitSha as string, state === "behind")
        : args.repoPath === root && !clean
          ? changedSet()
          : { buckets: [], general: { count: 0, files: [] } },
    context_repo_history: (args) =>
      navGitHistory(
        (args.branches as HistoryBranches) ?? { kind: "all" },
        (args.paths as string[] | null) ?? null,
        state === "behind" || state === "diverged",
        Date.now(),
      ),
    context_repo_status: () => ({ ...navGitStatus(state, Date.now()), conflicts }),
    resolve_context_conflict: (args) => {
      conflicts = conflicts.filter((c) => c.path !== args.path);
    },
    hand_context_conflict_to_agent: () => ({ status: "sent", messageId: 1 }),
    run_context_sync_action: () => ({ status: "sent", messageId: 2 }),
    get_file_redline: (args) =>
      REDLINES[args.filePath as string] ?? {
        status: "modified",
        binary: false,
        before: "Before.\n",
        after: "After, with a change.\n",
      },
  };
}

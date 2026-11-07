// SPDX-License-Identifier: MIT
import type { PublishSet } from "@/features/git-sync/lib/types";
import type { HistoryBranches, HistoryCommit, HistoryPage } from "@/tauri/commands";

import { MOCK_TEAM_NAME } from "../fixtures";

/**
 * The context repo's history in `?nav=demo`, shaped on a real one: a mostly linear main;
 * sync branches merged by PR; an open PR; a local branch GitHub never saw; a backup; one
 * commit not pushed. `?git=behind` adds two commits on GitHub this Mac lacks.
 */

const PREFIX = `${MOCK_TEAM_NAME}/content`;
const rel = (p: string) => `${PREFIX}/${p}`;
const HOUR = 3600;

const sha = (id: string) => `${id}${"0".repeat(40 - id.length)}`;

interface Seed {
  id: string;
  short: string;
  parents: string[];
  subject: string;
  author: string;
  byYou?: boolean;
  agent?: string;
  ago: number;
  lane: HistoryCommit["lane"];
  pr?: number;
  localOnly?: boolean;
  refs?: HistoryCommit["refs"];
  files: string[];
}

function seeds(behind: boolean): Seed[] {
  const head: Seed = {
    id: "c0",
    short: "5a7a83a",
    parents: ["c1"],
    subject: "Explicit sync + agent routines epics",
    author: "you",
    byYou: true,
    ago: 4 * 60,
    lane: "main",
    localOnly: true,
    refs: [
      { name: "main", kind: "local" },
      { name: "HEAD", kind: "head" },
    ],
    files: [rel("29-routines/epic.md"), rel("30-sync/epic.md")],
  };
  const rest: Seed[] = [
    head,
    {
      id: "p1",
      short: "e91c0d2",
      parents: ["p0"],
      subject: "Sync routine stories, first draft",
      author: "you",
      byYou: true,
      agent: "claude",
      ago: 20 * 60,
      lane: "open",
      refs: [{ name: "origin/gs/sync-routines", kind: "remote" }],
      files: [rel("29-routines/RT-1.md"), rel("29-routines/RT-2.md")],
    },
    {
      id: "c1",
      short: "e6bdd43",
      parents: ["c2"],
      subject: "Epic 27 fixes: per-tab inbox design",
      author: "you",
      byYou: true,
      agent: "claude",
      ago: HOUR,
      lane: "main",
      refs: behind ? [] : [{ name: "origin/main", kind: "remote" }],
      files: [rel("27-left-nav/LN-1.md")],
    },
    {
      id: "p0",
      short: "77ab310",
      parents: ["c3"],
      subject: "Sync routine epic outline",
      author: "claude[bot]",
      agent: "claude",
      ago: 2 * HOUR,
      lane: "open",
      files: [rel("29-routines/epic.md")],
    },
    {
      id: "c2",
      short: "e7bbe4b",
      parents: ["c3"],
      subject: "Release runbook: v0.0.20 state",
      author: "you",
      byYou: true,
      ago: 3 * HOUR,
      lane: "main",
      files: [rel("runbook.md")],
    },
    {
      id: "c3",
      short: "5486bae",
      parents: ["c4", "g1"],
      subject: "Migrate dashboards to runPrompt",
      author: "maya",
      ago: 5 * HOUR,
      lane: "main",
      pr: 23,
      files: [
        rel("26-agent-inbox/dashboards/agent-dag.html"),
        rel("26-agent-inbox/dashboards/inbox-board.html"),
        rel("26-agent-inbox/SW-GW-11.md"),
        rel("weft.sqlite3"),
      ],
    },
    {
      id: "g1",
      short: "60f852d",
      parents: ["c5"],
      subject: "Migrate dashboards to runPrompt",
      author: "maya",
      agent: "codex",
      ago: 6 * HOUR,
      lane: "merged",
      refs: [{ name: "origin/gs/general-dd2b", kind: "remote" }],
      files: [rel("26-agent-inbox/dashboards/inbox-board.html")],
    },
    {
      id: "c4",
      short: "4c2590c",
      parents: ["c5"],
      subject: "Epic 26: AID-4 and AID-5 merged",
      author: "you",
      byYou: true,
      agent: "claude",
      ago: 26 * HOUR,
      lane: "main",
      files: [rel("26-agent-inbox/AID-4.md")],
    },
    {
      id: "f1",
      short: "a6a847b",
      parents: ["c5"],
      subject: "Sync team content: Board and Pipeline panes",
      author: "you",
      byYou: true,
      ago: 27 * HOUR,
      lane: "local",
      localOnly: true,
      refs: [{ name: "feat-left-nav-search", kind: "local" }],
      files: [rel("27-left-nav/LN-2.md")],
    },
    {
      id: "c5",
      short: "701e05a",
      parents: ["c6", "g3"],
      subject: "Fixes from the remote-machine test",
      author: "maya",
      ago: 50 * HOUR,
      lane: "main",
      pr: 22,
      files: [rel("28-remote-machines/FX-7.md")],
    },
    {
      id: "g3",
      short: "c7c7ec6",
      parents: ["g2"],
      subject: "FX-7 done",
      author: "you",
      byYou: true,
      agent: "claude",
      ago: 52 * HOUR,
      lane: "merged",
      refs: [{ name: "origin/gs/fixes-c7c7", kind: "remote" }],
      files: [rel("28-remote-machines/FX-7.md")],
    },
    {
      id: "g2",
      short: "7449ae8",
      parents: ["c7"],
      subject: "Live droplet fixes FX-1 to FX-6",
      author: "you",
      byYou: true,
      agent: "claude",
      ago: 74 * HOUR,
      lane: "merged",
      files: [rel("28-remote-machines/FX-1.md")],
    },
    {
      id: "b1",
      short: "394fa6d",
      parents: ["c7"],
      subject: "Back up agent workspace mockup",
      author: "you",
      byYou: true,
      ago: 7 * 24 * HOUR,
      lane: "backup",
      refs: [{ name: "origin/backup/agent-workspace-cli", kind: "remote" }],
      files: [rel("mockups/agent-workspace.html")],
    },
    {
      id: "c6",
      short: "7fdc098",
      parents: ["c7"],
      subject: "Store the notify hook as executable",
      author: "you",
      byYou: true,
      ago: 7 * 24 * HOUR + 600,
      lane: "main",
      files: [rel("hooks/notify.sh")],
    },
    {
      id: "c7",
      short: "f29bbad",
      parents: [],
      subject: "Consolidate mockup UX through v135",
      author: "maya",
      ago: 8 * 24 * HOUR,
      lane: "main",
      files: [rel("mockups/index.html")],
    },
  ];
  if (!behind) return rest;
  return [
    {
      id: "o2",
      short: "b3e10f4",
      parents: ["o1"],
      subject: "Roadmap: Q4 reshuffle",
      author: "maya",
      ago: 8 * 60,
      lane: "main",
      refs: [{ name: "origin/main", kind: "remote" }],
      files: [rel("roadmap.html")],
    },
    {
      id: "o1",
      short: "0d7a2c9",
      parents: ["c1"],
      subject: "AID-6 story draft",
      author: "maya",
      agent: "codex",
      ago: 15 * 60,
      lane: "main",
      files: [rel("26-agent-inbox/AID-6.md")],
    },
    ...rest,
  ];
}

function toCommit(s: Seed, now: number): HistoryCommit {
  return {
    sha: sha(s.id),
    shortSha: s.short,
    parents: s.parents.map(sha),
    refs: s.refs ?? [],
    subject: s.subject,
    author: s.author,
    byYou: !!s.byYou,
    committedAt: Math.round(now / 1000) - s.ago,
    pr: s.pr,
    lane: s.lane,
    localOnly: !!s.localOnly,
    agent: s.agent ? { name: s.agent } : undefined,
    forkPoint: false,
    files: s.files,
  };
}

const BRANCH_TIPS: Record<string, string[]> = {
  "gs/sync-routines": ["p1", "p0"],
  "feat-left-nav-search": ["f1"],
  "backup/agent-workspace-cli": ["b1"],
  "gs/general-dd2b": ["g1"],
  "gs/fixes-c7c7": ["g3", "g2"],
};
const FORK: Record<string, string> = {
  "gs/sync-routines": "c3",
  "feat-left-nav-search": "c5",
  "backup/agent-workspace-cli": "c7",
  "gs/general-dd2b": "c5",
  "gs/fixes-c7c7": "c7",
};

export function navGitHistory(
  branches: HistoryBranches,
  paths: string[] | null,
  behind: boolean,
  now: number,
): HistoryPage {
  const all = seeds(behind).map((s) => toCommit(s, now));
  let commits: HistoryCommit[];
  if (branches.kind === "main") {
    commits = all
      .filter((c) => c.lane === "main")
      .map((c) => ({ ...c, parents: c.parents.slice(0, 1) }));
  } else if (branches.kind === "branch") {
    const ids = BRANCH_TIPS[branches.name] ?? [];
    const own = all.filter((c) => ids.some((id) => sha(id) === c.sha));
    const fork = all.find((c) => c.sha === sha(FORK[branches.name] ?? ""));
    commits = fork ? [...own, { ...fork, parents: [], forkPoint: true }] : own;
  } else commits = all;
  if (paths) {
    commits = commits.filter(
      (c) => c.forkPoint || (c.files ?? []).some((f) => paths.some((p) => f.startsWith(`${p}/`))),
    );
  }
  return {
    commits,
    nextCursor: null,
    openPrs: [{ number: 24, sha: sha("p1"), branch: "gs/sync-routines" }],
    branches: [
      "main",
      ...Object.keys(BRANCH_TIPS).sort(),
      "gs/sync-agents",
      "gs/inbox-aid",
      "gs/notes-mcp",
      "gs/fx-89",
    ],
  };
}

/** What a commit changed, for its unfolded file list. */
export function navGitCommitFiles(commitSha: string, behind: boolean): PublishSet {
  const seed = seeds(behind).find((s) => sha(s.id) === commitSha);
  const files = (seed?.files ?? []).map((path, i) => {
    if (path.endsWith(".sqlite3")) return { path, status: "deleted" as const };
    if (path.includes("agent-dag") || path.includes("SW-GW-11"))
      return {
        path,
        status: "added" as const,
        added: path.includes("agent-dag") ? 214 : 58,
        removed: 0,
      };
    return { path, status: "modified" as const, added: 12 + i, removed: 3 };
  });
  return { buckets: [], general: { count: files.length, files } };
}

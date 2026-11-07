// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { ScratchSpaceListing } from "@/tauri/commands";
import { PUBLISH_SETS, EMPTY_SYNC_SET } from "../git-sync-fixtures";

/**
 * Sync-viewer mock commands. `get_publish_set` mirrors the real org-agnostic
 * command: one handler, keyed by `repoPath`. The scratch-space commands serve a
 * small scratch space for the mock org (its notes live in the notes mock).
 *
 * The read-only reimagine sources the working-tree set by `repoPath` only; the
 * real command's PR/commit source axes (`workspaceRef`/`prNumber`/`commitSha`)
 * are out of scope here and intentionally ignored.
 */
export function gitSyncHandlers(search = ""): HandlerRegistry {
  // `?changes=N` pins the Changes badge count so its states can be walked; otherwise the
  // count is the fixture publish set's size for the repo (0 when it has none).
  const pinnedChanges = Number.parseInt(new URLSearchParams(search).get("changes") ?? "", 10);
  // SLW-PT-20: "Use my scratch items in ‹Team›" — on unless switched off (keyed `org:team`).
  const scratchOff = new Set<string>();
  return {
    get_scratch_team_use: (args) => !scratchOff.has(`${args.orgSlug}:${args.team}`),
    set_scratch_team_use: (args) => {
      const key = `${args.orgSlug}:${args.team}`;
      if (args.on) scratchOff.delete(key);
      else scratchOff.add(key);
    },
    list_scratch_overlay_files: () => [],
    set_scratch_file_use_team: () => undefined,
    get_publish_set: (args) => PUBLISH_SETS[args.repoPath as string] ?? EMPTY_SYNC_SET,
    changed_path_count: (args) => {
      if (Number.isFinite(pinnedChanges) && pinnedChanges >= 0) return pinnedChanges;
      const set = PUBLISH_SETS[args.repoPath as string];
      return set ? set.buckets.reduce((n, b) => n + b.count, 0) + set.general.count : 0;
    },
    list_scratch_space: (args) => mockScratchSpace((args.orgSlug as string | null) ?? null),
    create_scratch_file: (args) =>
      `${SCRATCH_ROOT}/orgs/${args.orgSlug as string}/content/${args.name as string}`,
  };
}

const SCRATCH_ROOT = "/mock/users/1/scratch";

/** The mock scratch space: one file + one dashboard for the open org, and an all-orgs
 *  skill plus a file no org reads. */
export function mockScratchSpace(orgSlug: string | null): ScratchSpaceListing {
  const org = orgSlug ? `${SCRATCH_ROOT}/orgs/${orgSlug}` : null;
  return {
    orgSlug,
    exists: org !== null,
    files: org
      ? [
          { path: `${org}/content/docs/ideas.md`, logical: "docs/ideas.md", inertKind: null },
          {
            path: `${org}/content/prompts/standup.md`,
            logical: "prompts/standup.md",
            inertKind: "prompt",
          },
        ]
      : [],
    allOrgs: [
      {
        path: `${SCRATCH_ROOT}/all-orgs/skills/review/SKILL.md`,
        logical: "skills/review/SKILL.md",
        kind: "skill",
      },
      { path: `${SCRATCH_ROOT}/all-orgs/run.sh`, logical: "run.sh", kind: null },
    ],
    syncedOrgs: orgSlug ? [orgSlug] : [],
  };
}

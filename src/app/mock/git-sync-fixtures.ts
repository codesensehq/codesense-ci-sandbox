// SPDX-License-Identifier: MIT
/**
 * Seed fixtures for the reimagined sync viewer (the personal ⇄ team context
 * plane), served through the single `invoke` door in mock mode.
 *
 * This module is the FE-first **DTO contract**: the shape here is what the real
 * backend must later satisfy. It returns the real `PublishSet` wire shape
 * (`{ buckets, general }` of `PublishFile`) and layers on the facets the
 * reimagined surface renders against — `scope`, `contentRole`, and a top-level
 * `counts` roll-up. See `README.md` (DTO contract).
 */

import type {
  PublishSet,
  PublishBucket,
  GeneralBucket,
  PublishFile,
  PublishBucketKind,
} from "@/features/git-sync/lib/types";

// ── Net-new facets the fixture defines as the backend obligation ──────────────

/** Which sourced repo a change came from — repo identity, not a path guess. */
export type FileScope = "team" | "personal" | "loose";

/** What a change *is* (grouping/summary axis only). Retires the "Documentation"
 *  catch-all. NOT the render signal — how a file is drawn (incl. binary) is the
 *  FE-derived render-kind axis (SV-7, `FileRedline.binary`), independent of role. */
export type ContentRole = "agent-context" | "human-doc" | "config" | "binary";

/** Sub-grouping of the personal plane, mirroring the personal-team scope-lock. */
export type PersonalSurface = "notes" | "dashboards" | "content-overlay";

/** A `PublishFile` extended with the scope + role facets the viewer groups by.
 *  `personalSurface` is present only on personal-set files. */
export interface SyncViewerFile extends PublishFile {
  scope: FileScope;
  contentRole: ContentRole;
  personalSurface?: PersonalSurface;
}

/** Top-level roll-up so the summary header reads O(1), never walking rows. */
export interface PublishSetCounts {
  team: number;
  personal: number;
  loose: number;
  byRole: Record<ContentRole, number>;
}

export interface SyncViewerBucket extends Omit<PublishBucket, "files"> {
  files: SyncViewerFile[];
}

/** The reimagined publish set: the real bucketed shape + the viewer facets. */
export interface SyncViewerPublishSet extends Omit<PublishSet, "buckets" | "general"> {
  buckets: SyncViewerBucket[];
  general: Omit<GeneralBucket, "files"> & { files: SyncViewerFile[] };
  counts: PublishSetCounts;
}

// ── Roots the handler keys on ─────────────────────────────────────────────────

export const TEAM_ROOT = "/mock/orgs/acme/widgets";
export const PERSONAL_ROOT = "/mock/orgs/_personal/_personal";

/** Well-known scenario roots so `dev:mock` and tests can select any pathological
 *  case by passing its path to `get_publish_set`. Cross-plane scenarios are
 *  sourcing PAIRS: e.g. empty-team/full-personal = source `emptyTeam` (team side)
 *  together with `personal` (the full-personal side). */
export const FIXTURE_ROOTS = {
  team: TEAM_ROOT,
  personal: PERSONAL_ROOT,
  baseline3: "/mock/scenario/baseline-3",
  large3000: "/mock/scenario/large-3000",
  zeroChange: "/mock/scenario/zero-change",
  emptyTeam: "/mock/scenario/empty-team",
  allLoose: "/mock/scenario/all-loose",
} as const;

// ── Builders (one authoritative construction path) ────────────────────────────

const EMPTY_ROLE_COUNTS: Record<ContentRole, number> = {
  "agent-context": 0,
  "human-doc": 0,
  config: 0,
  binary: 0,
};

function file(
  path: string,
  scope: FileScope,
  contentRole: ContentRole,
  extra: Partial<SyncViewerFile> = {},
): SyncViewerFile {
  return { path, status: "modified", scope, contentRole, ...extra };
}

function bucket(
  id: string,
  label: string,
  kind: PublishBucketKind,
  files: SyncViewerFile[],
): SyncViewerBucket {
  return { workspaceRef: { id, label, kind }, count: files.length, files };
}

/** Flatten a set's files (buckets + general) — the traversal the counts roll-up,
 *  the tests, and the viewer's grouping all walk. */
export function allSyncViewerFiles(set: SyncViewerPublishSet): SyncViewerFile[] {
  return [...set.buckets.flatMap((b) => b.files), ...set.general.files];
}

/** Derive the counts roll-up from the files, so the header's O(1) numbers can
 *  never contradict the rows. Runs once at fixture-build time, not on render. */
function countsOf(files: SyncViewerFile[]): PublishSetCounts {
  const byRole: Record<ContentRole, number> = { ...EMPTY_ROLE_COUNTS };
  let team = 0;
  let personal = 0;
  let loose = 0;
  for (const f of files) {
    if (f.scope === "team") team++;
    else if (f.scope === "personal") personal++;
    else loose++;
    byRole[f.contentRole]++;
  }
  return { team, personal, loose, byRole };
}

function publishSet(
  buckets: SyncViewerBucket[],
  generalFiles: SyncViewerFile[],
): SyncViewerPublishSet {
  return {
    buckets,
    general: { count: generalFiles.length, files: generalFiles },
    counts: countsOf([...buckets.flatMap((b) => b.files), ...generalFiles]),
  };
}

// ── The seeded sets ───────────────────────────────────────────────────────────

/** A rich team set: story + epic buckets and a loose remainder, carrying a
 *  rename, a delete, and a spread of content roles. */
const teamSet: SyncViewerPublishSet = publishSet(
  [
    bucket("story-sv-16", "SW-SV-16 · sync viewer", "story", [
      file("docs/sync-viewer/overview.md", "team", "human-doc"),
      file("skills/sync-viewer/guide.md", "team", "agent-context"),
      file(".mcp.json", "team", "config", { status: "added" }),
      file("skills/sync-viewer/legacy.md", "team", "agent-context", {
        renamedFrom: "skills/sync-viewer/old-guide.md",
        badges: ["rename"],
      }),
      // Noise badges the "changed-content-only" lens (SV-4) hides.
      file("docs/sync-viewer/whitespace.md", "team", "human-doc", { badges: ["eol-only"] }),
      file("scripts/run.sh", "team", "config", { badges: ["mode"] }),
    ]),
    bucket("epic-sw-sv", "SW-SV · sync viewer reimagined", "epic", [
      file("dashboards/context-plane.json", "team", "config"),
      file("docs/retired-notes.md", "team", "human-doc", { status: "deleted" }),
    ]),
  ],
  [
    // Loose remainder: genuinely-homeless, in neither repo.
    file("scratch/notes.txt", "loose", "human-doc", { status: "added" }),
    file(".claude/skills/review/SKILL.md", "team", "agent-context"),
    file(".claude/hooks/lint.sh", "team", "config"),
  ],
);

/** The personal team's pending files across all three personal surfaces. Named so the intent to
 *  share these across scenarios is explicit (not reached out of another set). */
const personalFiles: SyncViewerFile[] = [
  file("notes/standup.md", "personal", "human-doc", { personalSurface: "notes" }),
  file("dashboards/my-week.json", "personal", "config", { personalSurface: "dashboards" }),
  file("skills/personal-review.md", "personal", "agent-context", {
    personalSurface: "content-overlay",
  }),
];

/** The personal team set: general bucket only (a personal repo has no story/epic
 *  workspace buckets). */
const personalSet: SyncViewerPublishSet = publishSet([], personalFiles);

/** A minimal 3-file team set for baseline sanity. */
const baseline3Set: SyncViewerPublishSet = publishSet(
  [],
  [
    file("docs/readme.md", "team", "human-doc", { status: "added" }),
    file("config/app.json", "team", "config"),
    file("skills/helper.md", "team", "agent-context", { status: "deleted" }),
  ],
);

/** A 3,000-file team set built by a cheap generator (never 3,000 literals) —
 *  the row-count repro harness for the large-changeset blank bug. */
const GEN_KINDS: Array<{ role: ContentRole; ext: string }> = [
  { role: "human-doc", ext: "md" },
  { role: "config", ext: "json" },
  { role: "config", ext: "yaml" },
  { role: "agent-context", ext: "sh" },
  { role: "agent-context", ext: "ts" },
  { role: "binary", ext: "png" },
];

function generateLargeSet(n: number): SyncViewerPublishSet {
  const files: SyncViewerFile[] = Array.from({ length: n }, (_, i) => {
    const { role, ext } = GEN_KINDS[i % GEN_KINDS.length];
    const status = i % 7 === 0 ? "added" : i % 11 === 0 ? "deleted" : "modified";
    return file(`src/generated/file-${i}.${ext}`, "team", role, { status });
  });
  return publishSet([], files);
}

/** Everything-loose: no team, no personal — only the homeless remainder. */
const allLooseSet: SyncViewerPublishSet = publishSet(
  [],
  [
    file("scratch/a.txt", "loose", "human-doc", { status: "added" }),
    file("tmp/b.log", "loose", "human-doc"),
    file("scratch/c.bin", "loose", "binary", { status: "added" }),
  ],
);

/** The canonical empty set — nothing pending. Also the team side of the
 *  empty-team/full-personal pair, and the honest fall-through for an unknown
 *  root ("this repo has nothing to sync"). */
export const EMPTY_SYNC_SET: SyncViewerPublishSet = publishSet([], []);

/** Every `get_publish_set` scenario, keyed by the repo path the FE passes. */
export const PUBLISH_SETS: Record<string, SyncViewerPublishSet> = {
  [FIXTURE_ROOTS.team]: teamSet,
  [FIXTURE_ROOTS.personal]: personalSet,
  [FIXTURE_ROOTS.baseline3]: baseline3Set,
  [FIXTURE_ROOTS.large3000]: generateLargeSet(3000),
  [FIXTURE_ROOTS.zeroChange]: EMPTY_SYNC_SET,
  [FIXTURE_ROOTS.emptyTeam]: EMPTY_SYNC_SET,
  [FIXTURE_ROOTS.allLoose]: allLooseSet,
};

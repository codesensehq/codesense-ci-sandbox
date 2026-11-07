// SPDX-License-Identifier: MIT
/**
 * SW-GS-1 — fixtures for the Git Sync Workbench view-model contracts.
 *
 * These mock the typed shapes SW-GS-2/-2a will produce. They are the ONLY
 * data the Workbench renders in this story (no git, no MCP, no session).
 * SW-GS-2/-4 repoint the `publish-set-source.ts` seam at the real
 * `get_publish_set` wrapper; these fixtures stay as the test/storybook
 * baseline.
 *
 * Branch names use the deterministic `gs/<bucket>-<shortTreeOid>` form
 * (Winston V-10) so V3 renders a stable identity and concurrent in-flight
 * PRs are disambiguable once SW-GS-2 lands.
 */

import type {
  AgentSessionState,
  ConflictRegion,
  GsStatus,
  IncomingSummary,
  PublishSet,
  WorkspaceRef,
} from "./types";

const STORY_REF: WorkspaceRef = {
  id: "ws-story-persistent-terminals",
  label: "story-X · Persistent Terminals",
  kind: "story",
};

const EPIC_REF: WorkspaceRef = {
  id: "ws-epic-seamless-workspaces",
  label: "epic-P · Seamless Workspaces",
  kind: "epic",
};

/** A representative grouped/counted publish set: two change-bearing buckets
 *  + a non-empty General bucket, with every badge kind represented across V2. */
export const PUBLISH_SET_FIXTURE: PublishSet = {
  buckets: [
    {
      workspaceRef: STORY_REF,
      count: 2,
      files: [
        {
          path: "_bmad-output/implementation-artifacts/in-progress/pt/PT-14-agent-state.md",
          status: "modified",
        },
        {
          path: "_bmad-output/implementation-artifacts/in-progress/pt/PT-13-reattach.md",
          status: "modified",
          badges: ["rename"],
        },
      ],
    },
    {
      workspaceRef: EPIC_REF,
      count: 2,
      files: [
        {
          path: "_bmad-output/planning-artifacts/seamless-workspaces/README.md",
          status: "modified",
          badges: ["eol-only"],
        },
        {
          path: "_bmad-output/planning-artifacts/seamless-workspaces/run.sh",
          status: "modified",
          badges: ["mode"],
        },
      ],
    },
  ],
  general: {
    count: 2,
    files: [
      { path: "_bmad-output/scratch/planning-notes.md", status: "added" },
      { path: "_bmad-output/scratch/wip-ideas.md", status: "added" },
    ],
  },
};

/** An empty publish set — nothing pending (drives V1's empty state). */
export const EMPTY_PUBLISH_SET_FIXTURE: PublishSet = {
  buckets: [],
  general: { count: 0, files: [] },
};

/** A glanceable mid-flow status: snapshotted → integrated clean → pushed →
 *  PR opened → merged (no CI checks state, the common case per AC-7). */
export const GS_STATUS_FIXTURE: GsStatus = {
  snapshotted: 3,
  integrated: "clean",
  pushedBranch: "gs/story-abc1234",
  pr: { number: 42, phase: "merged" },
  absorbed: true,
};

/** Variant exercising the conditional merge-when-green chip (AC-7): a repo
 *  WITH required checks shows `waiting-on-checks`. */
export const GS_STATUS_WAITING_CHECKS_FIXTURE: GsStatus = {
  snapshotted: 5,
  integrated: "clean",
  pushedBranch: "gs/epic-9f3c021",
  pr: { number: 43, phase: "waiting-on-checks" },
  absorbed: false,
};

/** A 3-way text conflict region for V4 (markdown content → rendered preview). */
export const CONFLICT_REGION_TEXT_FIXTURE: ConflictRegion = {
  path: "_bmad-output/planning-artifacts/seamless-workspaces/README.md",
  base: "# Seamless Workspaces\n\nOriginal overview.\n",
  ours: "# Seamless Workspaces\n\nOur revised overview with the new wave model.\n",
  theirs: "# Seamless Workspaces\n\nTheir overview emphasizing onboarding.\n",
  binary: false,
};

/** A binary pick-a-side conflict region for V4 (image / `.excalidraw`). */
export const CONFLICT_REGION_BINARY_FIXTURE: ConflictRegion = {
  path: "_bmad-output/planning-artifacts/seamless-workspaces/architecture.excalidraw",
  base: null,
  ours: "<binary ours>",
  theirs: "<binary theirs>",
  binary: true,
};

/** Default mocked session state: agent owns + session live (the steady
 *  "Agent driving" state). */
export const AGENT_DRIVING_FIXTURE: AgentSessionState = {
  owner: "agent",
  connection: "live",
};

/** Session-less pre-spawn state: no owned session yet. The picker renders
 *  from the Tauri command and the indicator reads "no session". */
export const NO_SESSION_FIXTURE: AgentSessionState = {
  owner: "agent",
  connection: "disconnected",
};

/** The quiet "ready to sync — N incoming" surface (AC-19). */
export const INCOMING_FIXTURE: IncomingSummary = {
  groups: [
    { workspaceRef: STORY_REF, count: 2 },
    { workspaceRef: EPIC_REF, count: 1 },
  ],
  total: 3,
};

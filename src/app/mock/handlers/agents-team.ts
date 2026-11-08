// SPDX-License-Identifier: MIT
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME, MOCK_USER, TEAM_CONTENT_PATH } from "../fixtures";
import type { HandlerRegistry } from "../mock-router";
import type { AskUserRequest } from "@/shared/lib/managed-session";
import type { Org, OrgSummary } from "@/shared/types/domain";
import type { LoadedOrg } from "@/shared/types/registry";
import type { Workspace } from "@/shared/stores/viewer-store";
import type {
  ActivitySnapshot,
  AgentSessionRow,
  AvatarPacksView,
  ConversationMessage,
  HumanStatusView,
  InboxDelivery,
  SessionListRow,
} from "@/tauri/commands";

/**
 * The `?agents=team` world: a team at work across several workspaces, so the bottom-workspace
 * Agents list and the tray show every shape they draw. A Manager the team designed (in the
 * team content folder, beside two plain shells) launched the login story's lead, who launched three workers
 * there (one parked on a question); a second story has its own lead and one worker waiting on
 * an approval, beside a tab where someone started `claude` by hand (no launch record, only its
 * hooks say an agent runs there), a `claude` reattached from before the app started and a
 * `codex` (without an observer: only their running processes say so) and a reviewer the login lead launched into that workspace; the scratch folder holds only a shell; and one agent runs in
 * the background with no tab. `?agents=team&manager=none` leaves the Manager out.
 */
export function isAgentTeamWorld(search: string): boolean {
  const agents = new URLSearchParams(search).get("agents");
  return agents === "team" || agents === "teams";
}

/**
 * The `?agents=teams` world: the `?agents=team` world plus a second open team in a second org
 * (Abacus Labs › riverside), so the Agents pane lists two teams as it does for a person who
 * works in more than one. The second team has a story workspace (SV-3) and its content folder,
 * with five agents across them: some working, some ready.
 */
export function isAgentTeamsWorld(search: string): boolean {
  return new URLSearchParams(search).get("agents") === "teams";
}

export const SECOND_ORG_SLUG = "abacus/labs";
export const SECOND_ORG_NAME = "Abacus Labs";
export const SECOND_TEAM_NAME = "riverside";
export const SECOND_TEAM_KEY = `${SECOND_ORG_SLUG}:${SECOND_TEAM_NAME}`;
const SECOND_ORG_ROOT = `/mock/users/${MOCK_USER.userId}/orgs/${SECOND_ORG_SLUG}`;
export const SECOND_TEAM_CONTENT_PATH = `${SECOND_ORG_ROOT}/${SECOND_TEAM_NAME}/content`;
const SECOND_STORY_DIR = `${SECOND_TEAM_CONTENT_PATH}/worktrees/SV-3`;
const SECOND_CONTENT_WS = `generic:${SECOND_TEAM_CONTENT_PATH}`;

export const secondOrg: Org = {
  slug: SECOND_ORG_SLUG,
  display_name: SECOND_ORG_NAME,
  created_at: "2026-01-01T00:00:00Z",
  remote_url: "https://github.com/abacus/labs.git",
  config_flags: {},
  people: [],
  teams: [{ name: SECOND_TEAM_NAME, source_repos: [] }],
  org_root: SECOND_ORG_ROOT,
};

const secondOrgSummary: OrgSummary = {
  slug: SECOND_ORG_SLUG,
  display_name: SECOND_ORG_NAME,
  has_remote: true,
  remote_url: secondOrg.remote_url,
  org_root: SECOND_ORG_ROOT,
};

/** The second team's workspaces, as persisted viewer state holds them. */
export const SECOND_TEAM_WORKSPACES = [
  { id: SECOND_CONTENT_WS, kind: "generic", cwd: SECOND_TEAM_CONTENT_PATH, status: "ready" },
  {
    id: "SV-3",
    kind: "story",
    cwd: SECOND_STORY_DIR,
    status: "ready",
    storyId: "SV-3",
    storyTitle: "Agent navigation redesign",
    storyStatus: "in-progress",
    storyFilePath: `${SECOND_TEAM_CONTENT_PATH}/stories/SV-3.md`,
  },
] satisfies Workspace[];

/** The second team's tabs, in tab order per workspace. */
export const SECOND_TEAM_TABS: readonly AgentTeamTab[] = [
  {
    workspaceId: SECOND_CONTENT_WS,
    kind: "generic",
    dir: SECOND_TEAM_CONTENT_PATH,
    session: "sess-sv-handbook",
    name: "Handbook editor",
  },
  {
    workspaceId: SECOND_CONTENT_WS,
    kind: "generic",
    dir: SECOND_TEAM_CONTENT_PATH,
    session: "sess-sv-groom",
    name: "Backlog groomer",
  },
  {
    workspaceId: "SV-3",
    kind: "story",
    dir: SECOND_STORY_DIR,
    session: "sess-sv-lead",
    name: "Navigation lead",
  },
  {
    workspaceId: "SV-3",
    kind: "story",
    dir: SECOND_STORY_DIR,
    session: "sess-sv-mockups",
    name: "Pane mockups",
  },
  {
    workspaceId: "SV-3",
    kind: "story",
    dir: SECOND_STORY_DIR,
    session: "sess-sv-review",
    name: "Design review",
  },
];

/** The hooks the second team's agents last sent, delivered as the real `terminal-status-update`:
 *  three are mid-turn (Working), two finished their turn (Ready). */
export const SECOND_TEAM_STATUSES = [
  { tab_session_id: "sess-sv-handbook", status: "running", hook_event: "PreToolUse" },
  { tab_session_id: "sess-sv-lead", status: "running", hook_event: "PreToolUse" },
  { tab_session_id: "sess-sv-mockups", status: "running", hook_event: "PreToolUse" },
  { tab_session_id: "sess-sv-groom", status: "done", hook_event: "Stop" },
  { tab_session_id: "sess-sv-review", status: "done", hook_event: "Stop" },
] as const;

/** Agents in the team worlds whose subagents run now, as their host's activity snapshot counts
 *  them: the login lead works with two; in the second team the navigation lead works with three,
 *  and the design reviewer's own turn has ended while one subagent still runs (so it is not Ready). */
export const AGENT_TEAM_SUBAGENTS: readonly {
  session: string;
  state: "working" | "idle";
  subagents: number;
  secondTeam?: true;
}[] = [
  { session: "sess-login-lead", state: "working", subagents: 2 },
  { session: "sess-sv-lead", state: "working", subagents: 3, secondTeam: true },
  { session: "sess-sv-review", state: "idle", subagents: 1, secondTeam: true },
];

/** Agents the host observes with no subagents running: a tab with an observed agent offers its
 *  inbox, as the real host's snapshot says there is an agent to write to. */
const AGENT_TEAM_OBSERVED: readonly { session: string; state: "working" | "idle" }[] = [
  { session: "sess-login-repro", state: "working" },
  { session: "sess-manager", state: "idle" },
];

/** The host's activity snapshots for {@link AGENT_TEAM_SUBAGENTS} the world holds, as
 *  `terminal_activity_snapshot` answers them. */
export function agentTeamActivitySnapshots(search: string): ActivitySnapshot[] {
  const secondTeam = isAgentTeamsWorld(search);
  return AGENT_TEAM_SUBAGENTS.filter((a) => secondTeam || !a.secondTeam).map(activitySnapshot);
}

/** Every activity snapshot the team world's host holds: the subagent ones, and the agents it
 *  observes with none running. */
export function agentTeamHostSnapshots(search: string): ActivitySnapshot[] {
  return [
    ...agentTeamActivitySnapshots(search),
    ...AGENT_TEAM_OBSERVED.map((a) => activitySnapshot({ ...a, subagents: 0 })),
  ];
}

function activitySnapshot(a: { session: string; state: "working" | "idle"; subagents: number }) {
  return {
    session: a.session,
    identity: {
      hostBootId: "mock-boot",
      sessionIncarnation: `${a.session}-1`,
      invocationId: `${a.session}-claude`,
    },
    revision: 1,
    turnGeneration: 1,
    provider: "claude",
    state: a.state,
    observed: true,
    limited: false,
    toolOpen: a.state === "working",
    activityAgeMs: 4_000,
    subagentsOpen: a.subagents,
    exit: null,
    asks: [],
  } satisfies ActivitySnapshot;
}

function secondTeamRows(): AgentSessionRow[] {
  const lead = "SV-3#lead";
  return [
    row(`${SECOND_TEAM_CONTENT_PATH}#handbook`, "sess-sv-handbook", 52, {
      displayName: "Handbook editor",
      agentRevision: "builtin/claude@13",
      startedWith: "Fold the review notes into the team handbook.",
    }),
    row(`${SECOND_TEAM_CONTENT_PATH}#groom`, "sess-sv-groom", 33, {
      displayName: "Backlog groomer",
      agentRevision: "builtin/codex@13",
    }),
    row(lead, "sess-sv-lead", 47, {
      displayName: "Navigation lead",
      agentRevision: "builtin/claude@13",
      story: "SV-3",
      startedWith: "Own SV-3: mockups, review captures, then the pane.",
    }),
    row("SV-3#mockups", "sess-sv-mockups", 21, {
      displayName: "Pane mockups",
      agentRevision: "builtin/claude@13",
      launchedBy: lead,
      story: "SV-3",
      startedWith: "Draw the two-team Agents pane at 280px and 360px.",
    }),
    row("SV-3#review", "sess-sv-review", 12, {
      displayName: "Design review",
      agentRevision: "builtin/claude@13",
      launchedBy: lead,
      story: "SV-3",
    }),
  ];
}

/** The second team's own avatar pack: a few plain faces, so its agents draw unlike the first
 *  team's (which has no packs on disk and draws the built-in ones). */
const SECOND_TEAM_PACK_ID = "riverside-crew";
const SECOND_TEAM_FACE_COLORS = ["#2f7d6d", "#b5562e", "#4b5fb8", "#9a3f8c", "#c29a1e"];
const secondTeamFaceSrc = (color: string) =>
  `data:image/svg+xml;base64,${btoa(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="${color}"/><circle cx="11" cy="13" r="2.5" fill="#fff"/><circle cx="21" cy="13" r="2.5" fill="#fff"/><path d="M10 21q6 5 12 0" stroke="#fff" stroke-width="2" fill="none" stroke-linecap="round"/></svg>`,
  )}`;
export const SECOND_TEAM_AVATARS: AvatarPacksView = {
  packs: [
    {
      id: SECOND_TEAM_PACK_ID,
      name: "Riverside crew",
      scope: "team",
      faces: SECOND_TEAM_FACE_COLORS.map((color, i) => ({
        id: `face-${i + 1}`,
        ref: `${SECOND_TEAM_PACK_ID}:face-${i + 1}`,
        label: null,
        src: secondTeamFaceSrc(color),
      })),
    },
  ],
  default_pack: SECOND_TEAM_PACK_ID,
  agent_avatars: {},
  warnings: [],
};

/** The second org is discoverable and loads by slug, and the second team lists its own avatar
 *  pack; every other slug and team is answered as before. */
export function agentTeamsOrgHandlers(prior: HandlerRegistry): HandlerRegistry {
  const discover = prior.discover_local_orgs;
  const load = prior.load_org_cmd;
  const avatars = prior.avatar_packs_list_for_team;
  return {
    discover_local_orgs: async (args) => {
      const found = ((await discover?.(args)) ?? []) as OrgSummary[];
      return found.some((o) => o.slug === SECOND_ORG_SLUG) ? found : [...found, secondOrgSummary];
    },
    load_org_cmd: (args) =>
      String(args.slug) === SECOND_ORG_SLUG
        ? ({
            slug: SECOND_ORG_SLUG,
            root: SECOND_ORG_ROOT,
            org: secondOrg,
            partial: { org_yml: null, people_yml: null, teams_yml: null },
            file_errors: [],
          } satisfies LoadedOrg)
        : load?.(args),
    avatar_packs_list_for_team: (args) =>
      String(args.teamKey) === SECOND_TEAM_KEY ? SECOND_TEAM_AVATARS : avatars?.(args),
  };
}

export const TEAM_KEY = `${MOCK_ORG_SLUG}:${MOCK_TEAM_NAME}`;
const MANAGER_HANDLE = `${TEAM_CONTENT_PATH}#manager`;
const MINUTE = 60_000;

/** The story workspaces the team's agents work in, as persisted viewer state holds them. */
export const AGENT_TEAM_STORY_WORKSPACES = [
  {
    id: "BUG-142",
    kind: "story",
    cwd: `${TEAM_CONTENT_PATH}/worktrees/BUG-142`,
    status: "ready",
    storyId: "BUG-142",
    storyTitle: "Fix login timeout",
    storyStatus: "in-progress",
    storyFilePath: `${TEAM_CONTENT_PATH}/stories/BUG-142.md`,
  },
  {
    id: "BUG-151",
    kind: "story",
    cwd: `${TEAM_CONTENT_PATH}/worktrees/BUG-151`,
    status: "ready",
    storyId: "BUG-151",
    storyTitle: "Missing notifications",
    storyStatus: "ready-for-dev",
    storyFilePath: `${TEAM_CONTENT_PATH}/stories/BUG-151.md`,
  },
];

/** One terminal tab the host already runs for the team world: an agent or a plain shell. */
export interface AgentTeamTab {
  /** The workspace record it sits in (`generic:<dir>` or a story id). */
  workspaceId: string;
  kind: "generic" | "story";
  dir: string;
  session: string;
  name: string;
}

const STORY_DIR = (id: string) => `${TEAM_CONTENT_PATH}/worktrees/${id}`;

/** The tabs the team world's host holds, in tab order per workspace. The content folder's own
 *  shells are seeded by the plain terminal demo, so only the Manager joins them here. */
export function agentTeamTabs(search: string): AgentTeamTab[] {
  const withManager = new URLSearchParams(search).get("manager") !== "none";
  const story = (id: string, session: string, name: string): AgentTeamTab => ({
    workspaceId: id,
    kind: "story",
    dir: STORY_DIR(id),
    session,
    name,
  });
  return [
    ...(withManager
      ? [
          {
            workspaceId: `generic:${TEAM_CONTENT_PATH}`,
            kind: "generic" as const,
            dir: TEAM_CONTENT_PATH,
            session: "sess-manager",
            name: "Manager",
          },
        ]
      : []),
    story("BUG-142", "sess-login-lead", "Login timeout lead"),
    story("BUG-142", "sess-login-repro", "Reproduce timeout"),
    story("BUG-142", "sess-login-dev", "Implement retry fix"),
    story("BUG-142", "sess-login-qa", "Verify on staging"),
    story("BUG-142", "sess-login-shell", "zsh"),
    story("BUG-151", "sess-notify-lead", "Notifications triage"),
    story("BUG-151", "sess-notify-spec", "Draft the fix spec"),
    story("BUG-151", "sess-login-crosscheck", "Cross-check token refresh"),
    story("BUG-151", HAND_STARTED_SESSION, "remote codesensemcp"),
    story("BUG-151", REATTACHED_CLAUDE_SESSION, "token audit"),
    story("BUG-151", HAND_CODEX_SESSION, "codex review"),
  ];
}

/** The tab where a person typed `claude` themselves: the roster has no row for it. */
export const HAND_STARTED_SESSION = "sess-notify-hand";

/** A `claude` started by hand before this app launched: its session reattached, and the hook
 *  that established it fired long ago, so only its running process says an agent runs there. */
export const REATTACHED_CLAUDE_SESSION = "sess-notify-reattached";

/** A `codex` typed into a tab: Without a CodeSense observer, only its running process says so. */
export const HAND_CODEX_SESSION = "sess-notify-codex";

/** The agent CLI running under each team-world tab's shell, as the process probe finds it. */
export const AGENT_TEAM_PROCESSES: Readonly<Record<string, string>> = {
  [HAND_STARTED_SESSION]: "claude",
  [REATTACHED_CLAUDE_SESSION]: "claude",
  [HAND_CODEX_SESSION]: "codex",
};

/** The hook the hand-started Claude sends, delivered as the real `terminal-status-update`. */
export const HAND_STARTED_STATUS = {
  tab_session_id: HAND_STARTED_SESSION,
  status: "running",
  hook_event: "PreToolUse",
};

function row(
  handle: string,
  sessionId: string,
  minutesAgo: number,
  extra: Partial<AgentSessionRow> = {},
): AgentSessionRow {
  return {
    handle,
    sessionId,
    kind: "dashboard-launched",
    alive: true,
    status: "running",
    backgrounded: false,
    startedWith: null,
    startedAtMs: Date.now() - minutesAgo * MINUTE,
    launcher: "virtual://agents",
    agentRevision: null,
    lastOutputMs: null,
    ...extra,
  };
}

export function agentTeamSessions(search: string): AgentSessionRow[] {
  const params = new URLSearchParams(search);
  // `ended=crosscheck` (a handle, or its part after `#`) shows that session as exited.
  const ended = params.get("ended");
  return [
    ...agentTeamRows(params.get("manager") !== "none"),
    ...(isAgentTeamsWorld(search) ? secondTeamRows() : []),
  ].map((r) =>
    ended && (r.handle === ended || r.handle.endsWith(`#${ended}`)) ? { ...r, alive: false } : r,
  );
}

function agentTeamRows(withManager: boolean): AgentSessionRow[] {
  const lead = "BUG-142#lead";
  const notifyLead = "BUG-151#triage";
  return [
    ...(withManager
      ? [
          row(MANAGER_HANDLE, "sess-manager", 96, {
            displayName: "Manager",
            agentRevision: "builtin/claude@13",
          }),
        ]
      : []),
    row(lead, "sess-login-lead", 41, {
      displayName: "Login timeout lead",
      agentRevision: "builtin/claude@13",
      launchedBy: withManager ? MANAGER_HANDLE : null,
      story: "BUG-142",
      startedWith: "Own BUG-142 end to end: reproduce, fix, verify.",
    }),
    row("BUG-142#repro", "sess-login-repro", 38, {
      displayName: "Reproduce timeout",
      agentRevision: "builtin/claude@13",
      launchedBy: lead,
      story: "BUG-142",
      startedWith: "Reproduce the refresh timeout against a throttled backend.",
    }),
    row("BUG-142#dev", "sess-login-dev", 27, {
      displayName: "Implement retry fix",
      agentRevision: "builtin/codex@13",
      launchedBy: lead,
      story: "BUG-142",
      startedWith: "Retry the refresh with a fresh deadline.",
    }),
    row("BUG-142#qa", "sess-login-qa", 9, {
      displayName: "Verify on staging",
      agentRevision: "builtin/claude@13",
      launchedBy: lead,
      story: "BUG-142",
    }),
    // The login lead reaches into the notifications workspace: its worker there nests under it.
    row("BUG-142#crosscheck", "sess-login-crosscheck", 14, {
      displayName: "Cross-check token refresh",
      agentRevision: "acme/backend/reviewer@1",
      launchedBy: lead,
      story: "BUG-151",
      role: "reviewer",
      startedWith: "Check whether the digest job hits the same expired-token path.",
    }),
    row(notifyLead, "sess-notify-lead", 18, {
      displayName: "Notifications triage",
      agentRevision: "acme/backend/triage@3",
      story: "BUG-151",
      startedWith: "Find why digest notifications stopped on Sep 30.",
    }),
    row("BUG-151#spec", "sess-notify-spec", 6, {
      displayName: "Draft the fix spec",
      agentRevision: "builtin/claude@13",
      launchedBy: notifyLead,
      story: "BUG-151",
      status: "paused",
    }),
    row(`${TEAM_KEY}::docs-sweep`, "sess-docs-sweep", 3, {
      displayName: "Docs link sweep",
      agentRevision: "acme/backend/triage@3",
      backgrounded: true,
      startedWith: "Check every docs link and file the broken ones.",
    }),
  ];
}

/** The question the staging verifier is parked on, delivered as the real `mcp:ask-user`. */
export const AGENT_TEAM_ASK: AskUserRequest = {
  correlationId: "mock-ask-qa",
  sessionId: "sess-login-qa",
  question: "Staging still has the old token TTL. Should I bump it to match production first?",
};

/** One row of the team world's inbox, as the store keeps it. */
interface MockInboxRow {
  messageId: number;
  from: string;
  fromName: string;
  to: string;
  toName: string;
  text: string;
  createdAtMs: number;
  inReplyTo: number | null;
  unread: boolean;
  /** Set for a message sent to a role inbox: where it stands. */
  delivery?: InboxDelivery;
  archivedAtMs?: number | null;
  contentType?: "plain" | "markdown" | "html" | null;
  /** The sender's one-line list preview. */
  summary?: string | null;
  /** What it asks of the human; `update` when unset (SW-AGR-18). */
  kind?: "needs-you" | "update";
}

/** As the store keeps a summary: one trimmed line, control and format
 *  characters dropped, at most 280 characters (code points). */
function storedSummary(v: unknown): string | null {
  if (typeof v !== "string") return null;
  const line = v
    .replace(/\s+/g, " ")
    // eslint-disable-next-line no-control-regex -- dropping control characters is the point
    .replace(/[\u0000-\u001f\u007f\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g, "")
    .trim();
  if (!line) return null;
  const chars = Array.from(line);
  return chars.length > 280 ? `${chars.slice(0, 279).join("").trimEnd()}…` : line;
}

const ROLE_INBOXES = new Set(["triage", "reviewer"]);

function queued(): InboxDelivery {
  return {
    state: "queued",
    pickedUpAtMs: null,
    instance: null,
    acceptedAtMs: null,
    disposition: null,
    note: null,
    withdrawnAtMs: null,
  };
}

/**
 * The team world's inbox: the person asked Triage (role inbox) about the digest, one of its
 * running sessions picked that up, answered and asked back; a follow-up is being read, another
 * still waits in the queue, one was withdrawn, and the login lead asked Triage something it
 * accepted as no work. The Agents tab's per-agent lens, the role send, withdraw and mark-read
 * behave as the backend does. With `simulate`, a message you send is picked up a few seconds
 * later and then accepted, as a running Triage would.
 */
export function agentTeamInboxHandlers(
  emit: (event: string) => void,
  {
    simulate = false,
    sessions = [],
  }: {
    simulate?: boolean;
    /** The world's sessions: the inbox read model names and tracks them. */
    sessions?: readonly AgentSessionRow[];
  } = {},
) {
  const at = (minutesAgo: number) => Date.now() - minutesAgo * MINUTE;
  const rows: MockInboxRow[] = [
    {
      messageId: 1,
      from: "human",
      fromName: "human",
      to: "triage",
      toName: "triage",
      text: "Digest notifications stopped on Sep 30. Can you find which change did it?",
      createdAtMs: at(26 * 60),
      inReplyTo: null,
      unread: true,
      delivery: {
        state: "done",
        pickedUpAtMs: at(26 * 60 - 1),
        instance: "sess-notify-lead",
        acceptedAtMs: at(26 * 60 - 1),
        disposition: "work",
        note: "Bisecting the Sep 30 deploy.",
        withdrawnAtMs: null,
      },
    },
    {
      messageId: 2,
      from: "BUG-151#triage",
      fromName: "Notifications triage",
      to: "human",
      toName: "human",
      text: "Found it: the Sep 30 deploy moved the digest job to the new queue, and its worker never subscribes to `digest.daily`. Drafting a fix spec in BUG-151 now.",
      createdAtMs: at(17),
      inReplyTo: 1,
      unread: false,
    },
    {
      messageId: 3,
      from: "human",
      fromName: "human",
      to: "triage",
      toName: "triage",
      text: "Ignore the Sep 29 deploy, it was rolled back the same evening.",
      createdAtMs: at(3 * 60),
      inReplyTo: null,
      unread: true,
      delivery: { ...queued(), state: "withdrawn", withdrawnAtMs: at(3 * 60 - 2) },
    },
    {
      messageId: 11,
      from: "human",
      fromName: "human",
      to: "BUG-142#lead",
      toName: "Login timeout lead",
      text: "Where are we on BUG-142? I'd like the retry fix on staging before the 3pm deploy window.",
      createdAtMs: at(34),
      inReplyTo: null,
      unread: false,
    },
    {
      messageId: 12,
      from: "human",
      fromName: "human",
      to: "BUG-142#lead",
      toName: "Login timeout lead",
      text: "Post updates here rather than in the channel.",
      createdAtMs: at(33),
      inReplyTo: null,
      unread: false,
    },
    {
      messageId: 9,
      from: "BUG-142#lead",
      fromName: "Login timeout lead",
      to: "human",
      toName: "human",
      // A long report: headings, a wide table and a code block, as agents write them.
      text: [
        "# BUG-142 status",
        "",
        "## Workers",
        "",
        "| worker | task | state | notes |",
        "|---|---|---|---|",
        "| Reproduce timeout | repro on staging | done | fails after 15 min idle |",
        "| Implement retry fix | refresh-token retry | in review | 2 findings addressed |",
        "| Verify on staging | staging check | waiting on you | needs the TTL decision |",
        "",
        "Repro:",
        "",
        "```sh",
        "pnpm e2e login --idle 900 --env staging --trace on",
        "```",
      ].join("\n"),
      createdAtMs: at(30),
      inReplyTo: null,
      unread: false,
    },
    {
      messageId: 4,
      from: "BUG-142#lead",
      fromName: "Login timeout lead",
      to: "human",
      toName: "human",
      text: "Retry fix is in review; staging verification is waiting on your TTL answer.",
      createdAtMs: at(8),
      inReplyTo: null,
      unread: true,
    },
    {
      messageId: 5,
      from: "BUG-142#lead",
      fromName: "Login timeout lead",
      to: "triage",
      toName: "triage",
      text: "Is the token refresh change in BUG-142 related to the digest job? Same deploy window.",
      createdAtMs: at(2 * 60),
      inReplyTo: null,
      unread: true,
      delivery: {
        state: "accepted",
        pickedUpAtMs: at(2 * 60 - 3),
        instance: "BUG-151#triage",
        acceptedAtMs: at(2 * 60 - 3),
        disposition: "no_work",
        note: "Not related: the digest worker never touches tokens.",
        withdrawnAtMs: null,
      },
    },
    {
      messageId: 6,
      from: "BUG-151#triage",
      fromName: "Notifications triage",
      to: "human",
      toName: "human",
      text: "Should the fix also replay the digests users missed since Sep 30, or only resume from today?",
      createdAtMs: at(4),
      kind: "needs-you",
      inReplyTo: 1,
      unread: true,
    },
    {
      messageId: 7,
      from: "human",
      fromName: "human",
      to: "triage",
      toName: "triage",
      text: "Also check whether the weekly digest is affected. It runs on the same scheduler.",
      createdAtMs: at(40),
      inReplyTo: null,
      unread: true,
      delivery: {
        ...queued(),
        state: "reading",
        pickedUpAtMs: at(2),
        instance: "sess-docs-sweep",
      },
    },
    {
      messageId: 8,
      from: "human",
      fromName: "human",
      to: "triage",
      toName: "triage",
      text: "When you're done, post the replay estimate in BUG-151 so the team can plan the backfill.",
      createdAtMs: at(1),
      inReplyTo: null,
      unread: true,
      delivery: queued(),
    },
    // Two Claudes started by hand in plain tabs, talking session to session (each is
    // addressed by its tab's session id: neither has a launch record).
    {
      messageId: 9,
      from: HAND_STARTED_SESSION,
      fromName: "remote codesensemcp",
      to: REATTACHED_CLAUDE_SESSION,
      toName: "token audit",
      text: "Can you check whether the token refresh path also touches the digest scheduler?",
      createdAtMs: at(6),
      inReplyTo: null,
      unread: false,
    },
    {
      messageId: 10,
      from: REATTACHED_CLAUDE_SESSION,
      fromName: "token audit",
      to: HAND_STARTED_SESSION,
      toName: "remote codesensemcp",
      text: "It doesn't: refresh only rotates the session cookie. The scheduler reads its own service token.",
      createdAtMs: at(3),
      inReplyTo: 9,
      unread: false,
    },
  ];
  let next = rows.length + 1;
  let humanStatus: HumanStatusView = { status: "online", updatedAtMs: 0 };
  const toView = (r: MockInboxRow) => ({
    ...r,
    toHuman: r.to === "human",
    inReplyToTeam: null,
    delivery: r.delivery ?? null,
    archivedAtMs: r.archivedAtMs ?? null,
    kind: r.kind ?? "update",
  });
  /** The rows of `agentId`'s conversation: its own, and every reply under them. */
  const threadOf = (agentId: string, sessions: readonly string[] = []) => {
    const ids = new Set<number>();
    const own = new Set([agentId, ...sessions]);
    for (const r of [...rows].sort((a, b) => a.messageId - b.messageId)) {
      if (own.has(r.to) || own.has(r.from) || (r.inReplyTo && ids.has(r.inReplyTo))) {
        ids.add(r.messageId);
      }
    }
    return ids;
  };
  const find = (args: Record<string, unknown>) =>
    rows.find((r) => r.messageId === Number(args.messageId));
  /** A running Triage picks a queued message up, then accepts it as work. */
  const consumeLater = (row: MockInboxRow) => {
    setTimeout(() => {
      if (row.delivery?.state !== "queued") return;
      row.delivery = {
        ...row.delivery,
        state: "reading",
        pickedUpAtMs: Date.now(),
        instance: "sess-notify-lead",
      };
      emit("agent-inbox-changed");
      setTimeout(() => {
        if (row.delivery?.state !== "reading") return;
        row.delivery = {
          ...row.delivery,
          state: "accepted",
          acceptedAtMs: Date.now(),
          disposition: "work",
          note: "On it.",
        };
        emit("agent-inbox-changed");
      }, 6000);
    }, 12000);
  };
  /** A session id's inbox address: its launch handle, else the id itself (a hand-opened tab). */
  const addressOf = (session: string) =>
    agentTeamSessions(globalThis.location?.search ?? "?agents=team").find(
      (r) => r.sessionId === session,
    )?.handle ?? session;
  // The session-keyed read model (`list_inbox_rollup`, `agent_read_sessions`) over the same
  // rows: a role message names its queue, and a claim names the claiming session's handle.
  const handleOf = (instance: string | null) =>
    instance ? (sessions.find((s) => s.sessionId === instance)?.handle ?? instance) : null;
  const claimState = (d: InboxDelivery): NonNullable<ConversationMessage["delivery"]> => ({
    state:
      d.state === "queued"
        ? "pending"
        : d.state === "reading"
          ? "claimed"
          : d.state === "withdrawn"
            ? "withdrawn"
            : d.state === "done"
              ? "acknowledged"
              : "accepted",
    claimedBy: handleOf(d.instance),
  });
  const toConversation = (r: MockInboxRow): ConversationMessage => {
    const role = ROLE_INBOXES.has(r.to) ? r.to : null;
    return {
      id: r.messageId,
      team: { org: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME },
      from: r.from,
      fromName: r.fromName,
      to: role ? null : r.to,
      toName: role ? null : r.toName,
      toRole: role,
      text: r.text,
      contentType: r.contentType ?? "markdown",
      createdAtMs: r.createdAtMs,
      archivedAtMs: r.archivedAtMs ?? null,
      inReplyTo: r.inReplyTo,
      via: null,
      replyTo: null,
      summary: r.summary ?? null,
      unread: r.unread,
      delivery: role ? claimState(r.delivery ?? queued()) : null,
      kind: r.kind ?? "update",
    };
  };
  const sessionRows = (): SessionListRow[] =>
    sessions.map((s) => ({
      handle: s.handle,
      displayName: s.displayName ?? s.handle,
      state: !s.alive ? "exited" : s.status === "paused" ? "idle" : "running",
      launchedBy: s.launchedBy ?? null,
      unread: rows.filter((r) => r.to === s.handle && r.unread).length,
      oldestUnreadAt: null,
      ...(s.story ? { story: s.story } : {}),
      ...(s.role ? { role: s.role } : {}),
      ...(s.agentRevision
        ? {
            agent: s.agentRevision.includes("@")
              ? s.agentRevision.slice(0, s.agentRevision.lastIndexOf("@"))
              : s.agentRevision,
          }
        : {}),
    }));
  return {
    agent_read_sessions: () => sessionRows(),
    get_human_status: () => humanStatus,
    set_human_status: (args: Record<string, unknown>) => {
      // The real command's enum refuses anything else at deserialization.
      if (!["online", "away", "busy", "offline"].includes(String(args.status)))
        throw new Error(`unknown variant \`${String(args.status)}\``);
      const note = typeof args.note === "string" && args.note ? args.note : undefined;
      humanStatus = {
        status: args.status as HumanStatusView["status"],
        ...(note ? { note } : {}),
        updatedAtMs: Date.now(),
      };
      emit("agent-inbox-changed");
      return humanStatus;
    },
    // The read model's `inbox(query)`: a session filter lists every message to or from that
    // session (and its replies); a role adds that role inbox.
    agent_read_inbox: (args: Record<string, unknown>) => {
      const query = (args.query ?? {}) as { sessionId?: string; role?: string; kind?: string };
      const addresses = [
        ...(query.sessionId ? [addressOf(query.sessionId)] : []),
        ...(query.role ? [query.role] : []),
      ];
      const thread = addresses.length ? threadOf(addresses[0]!, addresses.slice(1)) : null;
      const messages = rows
        .filter((r) => !thread || thread.has(r.messageId))
        .filter((r) => !query.kind || (r.kind ?? "update") === query.kind)
        .sort((a, b) => b.messageId - a.messageId)
        .map(toView);
      return { messages, humanUnread: rows.filter((r) => r.to === "human" && r.unread).length };
    },
    // One session's conversation: everything it sent or received, and its role queue's mail.
    list_session_inbox: (args: Record<string, unknown>) => {
      const handle = String(args.handle ?? "");
      const role = sessions.find((s) => s.handle === handle)?.role ?? null;
      const roles = role && ROLE_INBOXES.has(role) ? [role] : [];
      return {
        handle,
        roles,
        messages: [...rows]
          .filter((r) => r.from === handle || r.to === handle || roles.includes(r.to))
          .sort((a, b) => a.createdAtMs - b.createdAtMs)
          .map(toConversation),
      };
    },
    list_inbox_rollup: () => ({
      messages: [...rows].sort((a, b) => a.createdAtMs - b.createdAtMs).map(toConversation),
      roles: [...ROLE_INBOXES].map((role) => {
        const holder = sessions.find((s) => s.alive && s.role === role)?.handle ?? null;
        const name = sessions.find((s) => s.handle === holder)?.displayName ?? holder;
        return {
          role,
          holder,
          pending: rows.filter((r) => r.to === role && (r.delivery ?? queued()).state === "queued")
            .length,
          note: holder ? `held by ${name}` : "no live holder",
        };
      }),
    }),
    send_inbox_as_human: (args: Record<string, unknown>) => {
      const text = String(args.text ?? "");
      if (!text.trim()) return { status: "invalid", message: "text must not be blank" };
      const raw = String(args.to ?? "").trim();
      const bare = raw.replace(/^@/, "");
      // `role:<id>` (the Reply-To spelling, also after `#`) names the role `<id>`.
      const roleName = (r: string) =>
        r
          .trim()
          .replace(/^role:/, "")
          .trim();
      // A bare name is a session when one answers to it, else a role (the real contract).
      const role = raw.startsWith("#")
        ? roleName(raw.slice(1))
        : raw.startsWith("role:")
          ? roleName(raw)
          : !raw.startsWith("@") &&
              ROLE_INBOXES.has(bare) &&
              !sessions.some((s) => s.handle === bare)
            ? bare
            : null;
      if (role === "") return { status: "invalid", message: "role must not be blank" };
      const to = role ?? bare.trim();
      // The real send refuses a blank recipient and a role queue named `human`.
      if (!to) return { status: "invalid", message: "Not sent: no recipient." };
      if (role === "human") return { status: "invalid", message: "Not sent: human is not a role." };
      const messageId = next++;
      const row: MockInboxRow = {
        messageId,
        from: "human",
        fromName: "human",
        to,
        toName: sessions.find((s) => s.handle === to)?.displayName ?? to,
        text,
        createdAtMs: Date.now(),
        inReplyTo: typeof args.inReplyTo === "number" ? args.inReplyTo : null,
        unread: true,
        contentType: (args.contentType as MockInboxRow["contentType"]) ?? null,
        summary: storedSummary(args.summary),
        ...(role ? { delivery: queued() } : {}),
      };
      rows.push(row);
      // Answering a message reads it, as the real send does (`mark_answered`).
      const answered = rows.find((r) => r.messageId === row.inReplyTo && r.to === "human");
      if (answered) answered.unread = false;
      if (simulate && role && ROLE_INBOXES.has(role)) consumeLater(row);
      emit("agent-inbox-changed");
      return { status: "sent", messageId };
    },
    list_inbox_messages: (args: Record<string, unknown>) => {
      const scope = (args.scope as string | undefined) ?? "to-human";
      const agent = typeof args.recipientAgentId === "string" ? args.recipientAgentId.trim() : "";
      const sessions = Array.isArray(args.sessions) ? args.sessions.map(String) : [];
      const thread = agent ? threadOf(agent, sessions) : null;
      const messages = rows
        .filter((r) =>
          scope === "all" ? true : scope === "to-agents" ? r.to !== "human" : r.to === "human",
        )
        .filter((r) => !thread || thread.has(r.messageId))
        .sort((a, b) => b.messageId - a.messageId)
        .map(toView);
      return { messages, humanUnread: rows.filter((r) => r.to === "human" && r.unread).length };
    },
    mark_inbox_message_read: (args: Record<string, unknown>) => {
      const row = find(args);
      if (row && row.to === "human" && row.unread) {
        row.unread = false;
        emit("agent-inbox-changed");
      }
      return undefined;
    },
    mark_inbox_messages_read: (args: Record<string, unknown>) => {
      const ids = new Set(Array.isArray(args.messageIds) ? args.messageIds.map(Number) : []);
      let changed = 0;
      for (const row of rows)
        if (ids.has(row.messageId) && row.to === "human" && row.unread) {
          row.unread = false;
          changed += 1;
        }
      if (changed > 0) emit("agent-inbox-changed");
      return changed;
    },
    mark_inbox_messages_unread: (args: Record<string, unknown>) => {
      const ids = new Set(Array.isArray(args.messageIds) ? args.messageIds.map(Number) : []);
      let changed = 0;
      for (const row of rows)
        if (ids.has(row.messageId) && row.to === "human" && !row.unread) {
          row.unread = true;
          changed += 1;
        }
      if (changed > 0) emit("agent-inbox-changed");
      return changed;
    },
    mark_inbox_messages_archived: (args: Record<string, unknown>) => {
      const ids = new Set(Array.isArray(args.messageIds) ? args.messageIds.map(Number) : []);
      let changed = 0;
      for (const row of rows) {
        if (ids.has(row.messageId) && row.to === "human" && !row.archivedAtMs) {
          row.archivedAtMs = Date.now();
          row.unread = false;
          changed += 1;
        }
      }
      if (changed > 0) emit("agent-inbox-changed");
      return changed;
    },
    unarchive_inbox_messages: (args: Record<string, unknown>) => {
      const ids = new Set(Array.isArray(args.messageIds) ? args.messageIds.map(Number) : []);
      const unread = new Set(
        Array.isArray(args.restoreUnread) ? args.restoreUnread.map(Number) : [],
      );
      let changed = 0;
      for (const row of rows) {
        if (ids.has(row.messageId) && row.to === "human" && row.archivedAtMs) {
          row.archivedAtMs = null;
          if (unread.has(row.messageId)) row.unread = true;
          changed += 1;
        }
      }
      if (changed > 0) emit("agent-inbox-changed");
      return changed;
    },
    withdraw_inbox_message: (args: Record<string, unknown>) => {
      const row = find(args);
      if (!row || row.to === "human") return { status: "notFound" };
      const delivery = row.delivery;
      if (!delivery) {
        if (!row.unread) return { status: "tooLate", state: "done" };
        row.delivery = { ...queued(), state: "withdrawn", withdrawnAtMs: Date.now() };
      } else if (delivery.state === "withdrawn") {
        return { status: "withdrawn", withdrawnAtMs: delivery.withdrawnAtMs };
      } else if (delivery.state !== "queued") {
        return { status: "tooLate", state: delivery.state };
      } else {
        row.delivery = { ...delivery, state: "withdrawn", withdrawnAtMs: Date.now() };
      }
      emit("agent-inbox-changed");
      return { status: "withdrawn", withdrawnAtMs: row.delivery.withdrawnAtMs };
    },
    reply_as_human: (args: Record<string, unknown>) => {
      const text = String(args.text ?? "");
      if (!text.trim()) return { status: "invalid", message: "text must not be blank" };
      const parent = find(args);
      if (!parent) return { status: "invalid", message: "unknown message" };
      const messageId = next++;
      rows.push({
        messageId,
        from: "human",
        fromName: "human",
        to: parent.from,
        toName: parent.fromName,
        text,
        createdAtMs: Date.now(),
        inReplyTo: parent.messageId,
        unread: true,
      });
      emit("agent-inbox-changed");
      return { status: "sent", messageId };
    },
    send_role_message_cmd: (args: Record<string, unknown>) => {
      const text = String(args.text ?? "");
      if (!text.trim()) return { status: "invalid", message: "text must not be blank" };
      const role = String(args.role);
      const messageId = next++;
      const row: MockInboxRow = {
        messageId,
        from: String(args.sender),
        fromName: String(args.sender),
        to: role,
        toName: role,
        text,
        createdAtMs: Date.now(),
        inReplyTo: null,
        unread: true,
        delivery: queued(),
      };
      rows.push(row);
      if (simulate && ROLE_INBOXES.has(role)) consumeLater(row);
      emit("agent-inbox-changed");
      return { status: "sent", messageId, deliveryId: messageId };
    },
  };
}

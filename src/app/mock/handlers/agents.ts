// SPDX-License-Identifier: MIT
import { hermesGolden, hermesGenerated, hermesAdmission } from "./hermes-golden";
import { AGENT_PROVIDERS } from "@/tauri/commands";
import { AGENT_RUNTIMES, isAgentRuntime } from "@/shared/lib/agent-runtimes";
import { readModelArg } from "@/shared/lib/model-arg";
import type { HandlerRegistry } from "../mock-router";
import { agentListHub, agentTeamListWorld } from "./agent-list-hub";
import type { MockEventBus } from "../mock-events";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "../fixtures";
import type {
  AgentContextSuggestion,
  AgentRepoOverride,
  AgentLaunchOpenArgs,
  AgentAccountChoice,
  AgentAccountView,
  AgentContentLayer,
  AgentDefinition,
  AgentDeleteReply,
  AgentDetailView,
  AgentForm,
  AgentGeneratedEntry,
  AgentLaunchWarning,
  AgentManifestEntry,
  AgentPreviewReply,
  AgentRevision,
  AgentRow,
  AgentSaveReply,
  AgentScope,
  AgentSessionRow,
  AgentStartupCatalog,
  AgentWriteReply,
  ResolvedAgentView,
  RoutineAuthor,
  RoleDeliveryState,
  RoleQueue,
} from "@/tauri/commands";
import { AGENTS_CHANGED_EVENT, ROUTINES_CHANGED_EVENT } from "@/tauri/events";
import type { HubRow, HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { SESSION_STATE_CHANGED_EVENT } from "@/features/viewer/lib/session-events";
import {
  AGENT_TEAM_ASK,
  agentTeamInboxHandlers,
  agentTeamSessions,
  HAND_STARTED_STATUS,
  isAgentTeamWorld,
  isAgentTeamsWorld,
  SECOND_TEAM_STATUSES,
} from "./agents-team";
import type { AskUserRequest } from "@/shared/lib/managed-session";
import {
  mockContextUpdate,
  mockRuntimeDefault,
  placesLike,
  resolveMockContext,
  type MockContextAgent,
  type MockContextTiers,
} from "../lib/mock-agent-context";

/**
 * The Agents mock backend: your personal agent and a team's agents (Claude and Codex, one
 * archived, one bound to an account that was detached), the accounts you could run them
 * with, the routines using them (the same keys the routines mock lists) and one session
 * running an agent. Every write updates the in-memory world, records a revision with its
 * author and emits `agents:changed`, as the real service does, and a launch's bind records
 * its agent so "N running" follows.
 *
 * Startup paths resolve against a small in-memory team and personal content tree, the way
 * the backend's resolver does: the personal layer first, a folder expands to every file
 * under it, a file's `requires` follow it as dependencies, a path in no layer is `missing`,
 * and an unusually large entry is a warning, never an error.
 *
 * The starters (`builtin/claude`, `builtin/codex`, `builtin/opencode` and their unattended twins) are always listed, as the backend
 * lists them: read-only (every change is refused with the backend's message), copied to
 * customize, each naming its runtime's root CLAUDE.md / AGENTS.md, which (for any agent) is
 * left out rather than missing where no layer has it.
 */

const ME = "1";
const TEAM_ORG = "acme";
const TEAM = "backend";

/** The Claude mode a headless launch gets when its agent sets none. */
const CLAUDE_HEADLESS_DEFAULT_MODE = "dontAsk";

/** What CodeSense adds to every Claude agent's allow list. */
const CLAUDE_REQUIRED_ALLOW = [
  "mcp__codesense__post_agent_message",
  "mcp__codesense__ask_user",
  "mcp__codesense__signal_done",
];

/** You, as tab writes credit you. */
const YOU: RoutineAuthor = { kind: "user", name: "alice" };

/** The team's content: file → bytes. */
const TEAM_CONTENT: Record<string, number> = {
  "CLAUDE.md": 2_400,
  ".claude/skills/triage-slack/SKILL.md": 1_800,
  ".claude/skills/triage-slack/reference.md": 5_200,
  ".claude/skills/triage-slack/examples/escalation.md": 900,
  ".claude/skills/triage-slack/examples/routing.md": 1_100,
  ".claude/skills/x/SKILL.md": 1_200,
  ".claude/skills/x/helpers.md": 640,
  ".claude/skills/bmad-build/SKILL.md": 3_100,
  ".claude/skills/bmad-build/workflow.md": 7_900,
  ".claude/commands/bmad-help.md": 1_500,
  ".agents/skills/review/SKILL.md": 2_000,
  ".agents/skills/review/checklist.md": 1_300,
  ".agents/skills/review/examples.md": 2_700,
  "docs/guide.md": 4_100,
  "finish-codesense/02-routines/README.md": 6_000,
  "assets/demo-recording.mov": 31_457_280,
  ...Object.fromEntries(
    Array.from({ length: 9 }, (_, i) => [`docs/support/article-${i + 1}.md`, 1_000 + i * 50]),
  ),
  ...Object.fromEntries(
    Array.from({ length: 7 }, (_, i) => [`docs/runbooks/runbook-${i + 1}.md`, 2_000 + i * 75]),
  ),
  // A folder bigger than one picker page.
  ...Object.fromEntries(
    Array.from({ length: 520 }, (_, i) => [
      `archive/note-${String(i).padStart(4, "0")}.md`,
      300 + i,
    ]),
  ),
};

/** SW-XS-16: the widgets repo's own content tier (`repos/github.com/acme/widgets/content/`). */
const WIDGETS_REPO_CONTENT: Record<string, number> = {
  "docs/architecture/adr-widgets.md": 3_300,
  ".claude/skills/x/SKILL.md": 1_400,
};

/** Your personal space's content: file → bytes. */
const PERSONAL_CONTENT: Record<string, number> = {
  "notes/today.md": 420,
  "notes/ideas.md": 980,
};

/** What a file's frontmatter `requires`. */
const REQUIRES: Record<string, string[]> = {
  ".claude/skills/x/SKILL.md": ["docs/guide.md"],
};

/** The MCP servers the team's config lists, the always-present bridge first. */
const BRIDGE = "codesense";
const MCP_SERVERS = [BRIDGE, "slack", "github", "linear"];

/** The most entries one startup catalog page returns, as the backend pages. */
const CATALOG_PAGE = 500;

/** One entry over this many bytes is unusually large (25 MiB), as the backend warns. */
const LARGE_ENTRY_BYTES = 25 * 1024 * 1024;

export interface MockAgentEntry {
  definition: AgentDefinition;
  scope: AgentScope;
  created_by: string;
  /** Routine keys naming it; the mock's routines all run the agent they name, so these are
   *  also its own routines (SW-AGR-1), which archiving it archives. */
  used_by: string[];
  /** Its routines its archive archived, offered back on unarchive. */
  archivedRoutines?: string[];
  /** Your binding, by account id. */
  bound?: string;
  /** Your raw-terminal consent for the agent's interactive launches. */
  captureRaw?: boolean;
  /** Earlier revisions' differences from the current definition, oldest first; each fills in
   *  one revision before the current one. Revisions with none recorded match the current. */
  history?: Partial<AgentDefinition>[];
}

function agent(
  id: string,
  name: string,
  runtime: AgentDefinition["runtime"],
  extra: Partial<AgentDefinition> = {},
): AgentDefinition {
  return {
    id,
    name,
    description: "",
    created_by: ME,
    runtime,
    args: [],
    launch: "foreground",
    mcp: { servers: [] },
    startup: [],
    permissions: null,
    default_prompt: "",
    revision: 1,
    archived_at: null,
    ...extra,
  };
}

function seed(): Record<string, MockAgentEntry> {
  const team = (id: string) => `${TEAM_ORG}/${TEAM}/${id}`;
  return {
    [team("triage")]: {
      definition: agent("triage", "Triage", "claude", {
        description: "Sweeps the support channel and files what needs an owner.",
        args: ["--model", "opus"],
        mcp: { servers: ["slack", "github"] },
        startup: [
          ".claude/skills/triage-slack",
          ".claude/commands/bmad-help.md",
          "CLAUDE.md",
          "docs/support",
          "docs/runbooks",
          "finish-codesense/02-routines/README.md",
        ],
        permissions: {
          claude: {
            allow: ["Bash(gh issue:*)", "Read", "Edit", "WebFetch", "Grep"],
            deny: ["Bash(git push --force:*)", "Bash(rm -rf:*)"],
          },
        },
        default_prompt: "/triage-slack",
        created_by: "3",
        revision: 3,
      }),
      scope: "team",
      created_by: "alice",
      used_by: ["_personal/1/personal/morning-triage"],
      bound: "acct-claude-work",
      history: [
        {
          startup: [".claude/skills/triage-slack", "CLAUDE.md"],
          permissions: null,
          args: [],
        },
        {
          startup: [".claude/skills/triage-slack", ".claude/commands/bmad-help.md", "CLAUDE.md"],
          permissions: { claude: { allow: ["Read", "Grep"] } },
        },
      ],
    },
    [team("scribe")]: {
      definition: agent("scribe", "Scribe", "claude", {
        description: "Writes up standups and digests.",
        launch: "headless",
        mcp: { servers: ["slack"] },
        startup: ["CLAUDE.md", ".claude/commands/standup.md"],
        created_by: "2",
        default_prompt: "/standup",
      }),
      scope: "team",
      created_by: "Sam",
      used_by: ["acme/backend/my-inbox-digest", "acme/backend/standup-notes"],
      bound: "acct-claude-old",
    },
    [team("reviewer")]: {
      definition: agent("reviewer", "Reviewer", "codex", {
        description: "Reviews pull requests.",
        args: ["--model", "gpt-5"],
        startup: [".agents/skills/review"],
        created_by: "2",
      }),
      scope: "team",
      created_by: "Sam",
      used_by: [],
    },
    [team("writer")]: {
      definition: agent("writer", "Writer", "claude", {
        description: "Drafts release notes.",
        archived_at: "2026-09-28T10:00:00Z",
        revision: 2,
      }),
      scope: "team",
      created_by: "you",
      used_by: ["acme/backend/release-notes"],
    },
    "_personal/1/personal/dev": {
      definition: agent("dev", "Dev", "codex", {
        description: "My own dev agent.",
        permissions: { codex: { sandbox_mode: "workspace-write" } },
      }),
      scope: "personal",
      created_by: "you",
      used_by: [],
    },
  };
}

/** SW-XS-16: what the agent editor's Context section shows in mock mode: Scribe (a team
 *  agent on the Claude default) with a retired `projections.yml` suggestion carrying its real
 *  default `dest`s, and an override for the widgets repo. */
const CONTEXT_DEMO: Pick<MockAgentsOptions, "contextSuggestions" | "contextOverrides"> = {
  contextSuggestions: {
    // As `projections.yml` converts: a `dest` defaults to the last segment of the path.
    [`${TEAM_ORG}/${TEAM}/scribe`]: {
      source: "projections.yml",
      entries: [
        { path: ".claude/skills/triage-slack", dest: "triage-slack" },
        { path: "docs/runbooks", dest: "runbooks" },
        { path: "finish-codesense/02-routines", dest: "plans" },
      ],
      fingerprint: "mock-projections-1",
    },
  },
  contextOverrides: {
    [`${TEAM_ORG}/${TEAM}/scribe`]: [
      {
        repo: "github.com/acme/widgets",
        add: [{ path: "docs/support" }],
        remove: [".claude/skills/x/helpers.md"],
      },
    ],
  },
};

/** The starters' revision: the app version as one number, as the backend numbers it. */
const BUILTIN_REVISION = 13;

/** The refusal of any change to a starter, as the backend words it. */
export const BUILTIN_READ_ONLY = "built-in agent — Copy to Mine to customize";

/** The root instructions file each runtime's CLI reads. */
const ROOT_DOC: Record<AgentDefinition["runtime"], string> = {
  claude: "CLAUDE.md",
  codex: "AGENTS.md",
  opencode: "AGENTS.md",
  hermes: "AGENTS.md",
};

/** The starters, keyed as the backend keys them. */
function builtins(): Record<string, MockAgentEntry> {
  const starter = (runtime: AgentDefinition["runtime"], name: string, cli: string) => ({
    definition: agent(runtime, name, runtime, {
      description: `${cli} with its defaults and your own login. Starts with the team's ${ROOT_DOC[runtime]} when it has one.`,
      created_by: "CodeSense",
      revision: BUILTIN_REVISION,
      startup: [ROOT_DOC[runtime]],
    }),
    scope: "builtin" as const,
    created_by: "CodeSense",
    used_by: [],
  });
  const hermes = (unattended: boolean): MockAgentEntry => ({
    definition: hermesGolden(unattended).definition,
    scope: "builtin",
    created_by: "CodeSense",
    used_by: [],
  });
  return {
    "builtin/hermes": hermes(false),
    "builtin/hermes-unattended": hermes(true),
    "builtin/claude": starter("claude", "Claude", "Claude Code"),
    "builtin/codex": starter("codex", "Codex", "Codex"),
    "builtin/opencode": starter("opencode", "OpenCode", "OpenCode"),
    "builtin/claude-unattended": {
      definition: agent("claude-unattended", "Claude (unattended)", "claude", {
        description:
          "Claude Code that runs without asking for permission (permission checks skipped), for unattended background work.",
        created_by: "CodeSense",
        revision: BUILTIN_REVISION,
        startup: [ROOT_DOC.claude],
        args: ["--dangerously-skip-permissions"],
        permissions: { claude: { default_mode: "bypassPermissions" } },
      }),
      scope: "builtin" as const,
      created_by: "CodeSense",
      used_by: [],
    },
    "builtin/codex-unattended": {
      definition: agent("codex-unattended", "Codex (unattended)", "codex", {
        description:
          "Codex that runs without asking for permission (approval never, no sandbox), for unattended background work.",
        created_by: "CodeSense",
        revision: BUILTIN_REVISION,
        startup: [ROOT_DOC.codex],
        permissions: {
          codex: { approval_policy: "never", sandbox_mode: "danger-full-access" },
        },
      }),
      scope: "builtin" as const,
      created_by: "CodeSense",
      used_by: [],
    },
    "builtin/opencode-unattended": {
      definition: agent("opencode-unattended", "OpenCode (unattended)", "opencode", {
        description:
          "OpenCode that runs without asking for permission, for unattended background work.",
        created_by: "CodeSense",
        revision: BUILTIN_REVISION,
        startup: [ROOT_DOC.opencode],
        args: ["--auto"],
        permissions: { opencode: { permission: { "*": "allow" } } },
      }),
      scope: "builtin" as const,
      created_by: "CodeSense",
      used_by: [],
    },
  };
}

function seedAccounts(): AgentAccountView[] {
  const account = (
    id: string,
    provider: AgentAccountView["provider"],
    label: string,
    detachedAt: number | null = null,
  ): AgentAccountView => ({
    id,
    provider,
    label,
    authState: "ready",
    identityVerification: "verified",
    generation: 1,
    createdAt: 0,
    updatedAt: 0,
    detachedAt,
  });
  return [
    account("acct-claude-work", "claude", "claude-work"),
    account("acct-claude-personal", "claude", "claude-personal"),
    account("acct-codex", "codex", "codex-main"),
    account("acct-claude-old", "claude", "claude-old", 1),
  ];
}

export interface MockAgentsOptions {
  /** The assembled backend includes opt-in demo catalog extensions. */
  catalog?: () => AgentRow[];
  seed?: () => Record<string, MockAgentEntry>;
  accounts?: () => AgentAccountView[];
  /** The Active account per CLI (a live object: read at each list, so a test sets it after the
   *  mock is built). An unpinned agent of that CLI runs on it, or is refused when it is broken. */
  active?: Partial<Record<"claude" | "codex", string>>;
  /** Agent stamps (`<key>@<revision>`) of sessions already running. */
  running?: string[];
  /** Sessions already running, as the launch registry lists them; replaces `running`. */
  sessions?: AgentSessionRow[];
  /** When set, "Describe it to an agent" opens nothing and replies with this reason
   *  (no active team, the spawn cap, …). */
  authoringOpenError?: string;
  /** SW-XS-16: each agent's one-time `projections.yml` context suggestion. */
  contextSuggestions?: Record<string, AgentContextSuggestion>;
  /** SW-XS-16: each agent's stored per-repo context overrides. */
  contextOverrides?: Record<string, AgentRepoOverride[]>;
}

/** A mock agents world: its handlers, and what a test (or a mock walk) can do to it. */
export interface MockAgents {
  handlers: HandlerRegistry;
  setManifestAppliesAtLaunch: (applies: boolean) => void;
  /** Change an agent as someone else would (a teammate's pull, or an agent through its MCP
   *  tools): a new revision credited to `author`, announced like any write. */
  editAs: (key: string, patch: Partial<AgentDefinition>, author: RoutineAuthor) => void;
  /** How many authoring agents "Describe it to an agent" opened. */
  authoringOpened: () => number;
  /** Start a session as a launch would, announced like a real bind. */
  addSession: (row: AgentSessionRow) => void;
  /** Put a question to the person from a session, as the real `mcp:ask-user` does. */
  ask: (req: AskUserRequest) => void;
}

function slug(name: string): string {
  const s = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 55);
  return s || "agent";
}

/** A startup path as the backend normalizes it, or why it is refused. */
function normalizePath(raw: string): { path: string } | { error: string } {
  if (/^[/\\]/.test(raw) || /[*?[\]\\]/.test(raw)) {
    return { error: `startup path ${JSON.stringify(raw)} is not a plain relative path` };
  }
  const segments = raw.split("/").filter((p) => p !== "" && p !== ".");
  if (segments.length === 0) return { error: `startup path ${JSON.stringify(raw)} is empty` };
  if (segments.includes("..")) {
    return { error: `startup path ${JSON.stringify(raw)} may not contain a \`..\` segment` };
  }
  return { path: segments.join("/") };
}

const PERSONAL_LAYER: AgentContentLayer = { kind: "personal", account: ME };
const TEAM_LAYER: AgentContentLayer = { kind: "team", org: TEAM_ORG, team: TEAM };

/** The files `path` names in `content`: itself, or everything under it. */
function filesIn(content: Record<string, number>, path: string): string[] {
  if (path in content) return [path];
  return Object.keys(content)
    .filter((f) => f.startsWith(`${path}/`))
    .sort();
}

export function createMockAgents(
  events: MockEventBus,
  options: MockAgentsOptions = {},
): MockAgents {
  const agents = {
    ...(options.seed ?? seed)(),
    ...Object.fromEntries(
      Object.entries(builtins()).filter(([, entry]) => isAgentRuntime(entry.definition.runtime)),
    ),
  };
  const accounts = (options.accounts ?? seedAccounts)();
  const sessions: AgentSessionRow[] =
    options.sessions ??
    (options.running ?? []).map((stamp, i) =>
      sessionRow(`mock-agent-${i + 1}`, `mock-agent-${i + 1}`, stamp),
    );
  let manifestApplies = false;
  let authoringOpens = 0;
  // Every revision of every agent, oldest first, with its author and definition.
  const revisions: Record<string, AgentRevision[]> = {};
  for (const [key, entry] of Object.entries(agents)) {
    const current = entry.definition.revision;
    revisions[key] = Array.from({ length: current }, (_, i) => {
      const n = i + 1;
      const patch = n < current ? (entry.history?.[i] ?? {}) : {};
      return {
        revision: n,
        author: { kind: "user", name: entry.created_by },
        at: `2026-09-${String(20 + i).padStart(2, "0")}T10:00:00Z`,
        agent: { ...entry.definition, ...patch, revision: n },
      };
    });
  }
  const record = (key: string, author: RoutineAuthor) => {
    revisions[key] = [
      ...(revisions[key] ?? []),
      {
        revision: agents[key].definition.revision,
        author,
        at: new Date().toISOString(),
        agent: agents[key].definition,
      },
    ];
  };

  const changed = (key: string) => events.emit(AGENTS_CHANGED_EVENT, { key });
  const find = (key: unknown): MockAgentEntry => {
    const found = agents[String(key)];
    if (!found) throw new Error(`agent ${String(key)} not found`);
    return found;
  };
  /** An agent a write may change: never a starter. */
  const writable = (key: unknown): MockAgentEntry => {
    const found = find(key);
    if (found.scope === "builtin") throw new Error(BUILTIN_READ_ONLY);
    return found;
  };

  const accountChoice = (entry: MockAgentEntry): AgentAccountChoice => {
    const runtime = entry.definition.runtime;
    const provider = AGENT_PROVIDERS.find((provider) => provider === runtime) ?? null;
    if (entry.bound) {
      const bound = accounts.find((a) => a.id === entry.bound);
      const usable = bound && !bound.detachedAt && bound.provider === provider;
      return {
        source: "binding",
        bound_account_id: entry.bound,
        resolved_source: "explicit",
        account_id: usable ? bound.id : null,
        provider,
        label: usable ? bound.label : null,
        reason: !bound
          ? "account-missing"
          : bound.detachedAt
            ? "detached"
            : usable
              ? "explicit-override"
              : "provider-mismatch",
        availability: usable ? "available" : "unavailable",
        spawn_eligible: Boolean(usable),
        capture_raw_terminal: entry.captureRaw === true,
      };
    }
    const activeId = provider === null ? undefined : options.active?.[provider];
    if (activeId !== undefined) {
      const active = accounts.find((a) => a.id === activeId);
      const usable = active && !active.detachedAt && active.authState === "ready";
      return {
        source: "runtime_default",
        bound_account_id: null,
        resolved_source: "active",
        account_id: usable ? active.id : null,
        provider,
        label: usable ? active.label : null,
        reason: !active
          ? "account-missing"
          : active.detachedAt
            ? "detached"
            : usable
              ? "active-selection"
              : "auth-required",
        availability: usable ? "available" : "unavailable",
        spawn_eligible: Boolean(usable),
        capture_raw_terminal: entry.captureRaw === true,
      };
    }
    return {
      source: "runtime_default",
      bound_account_id: null,
      resolved_source: "system_default",
      account_id: null,
      provider,
      label: null,
      reason: "optional-system-default",
      availability: "available",
      spawn_eligible: true,
      capture_raw_terminal: entry.captureRaw === true,
    };
  };

  const running = (key: string) =>
    sessions.filter((s) => s.alive && s.agentRevision?.startsWith(`${key}@`)).length;

  const row = (key: string, entry: MockAgentEntry): AgentRow => {
    const [org, team] =
      entry.scope === "personal"
        ? ["_personal", "personal"]
        : entry.scope === "builtin"
          ? ["builtin", ""]
          : [TEAM_ORG, TEAM];
    return {
      key,
      name: entry.definition.name,
      description: entry.definition.description ?? "",
      runtime: entry.definition.runtime,
      launch: entry.definition.launch ?? "foreground",
      scope: entry.scope,
      org,
      team,
      created_by: entry.created_by,
      created_by_you: entry.definition.created_by === ME,
      archived: Boolean(entry.definition.archived_at),
      revision: entry.definition.revision,
      default_prompt: entry.definition.default_prompt ?? "",
      avatar: entry.definition.avatar ?? null,
      role: entry.definition.role ?? null,
      model: (() => {
        const admission = resolve(key, entry.definition, accountChoice(entry)).launch_admission;
        return admission?.model_endpoint
          ? (admission.model?.default ?? null)
          : readModelArg(entry.definition.args ?? []);
      })(),
      account: accountChoice(entry),
      used_by: entry.used_by,
      routines: entry.used_by.map(owned),
      archived_routines: (entry.archivedRoutines ?? []).map(owned),
      running_now: running(key),
    };
  };

  const generated = (definition: AgentDefinition): AgentGeneratedEntry[] => {
    if (definition.runtime === "hermes")
      return hermesGenerated(
        definition.id.endsWith("-unattended"),
        definition.launch === "headless",
      ).map((entry) => ({
        ...entry,
        content: {
          ...entry.content,
          ...(definition.startup_settings ? { model: definition.startup_settings.model } : {}),
        },
      }));
    if (definition.runtime === "opencode") {
      const permission = { ...(definition.permissions?.opencode?.permission ?? {}) };
      permission["*"] ??= "ask";
      permission["codesense_*"] ??= "deny";
      for (const name of [
        "who_am_i",
        "list_sessions",
        "signal_progress",
        "signal_done",
        "ask_user",
        "post_agent_message",
        "read_messages",
        "reply",
      ]) {
        permission[`codesense_${name}`] ??= "allow";
      }
      return [
        {
          id: "opencode-launch-permissions",
          target: "launch_settings",
          content: {
            permissions: Object.keys(permission)
              .sort()
              .map((action) => ({
                action: action === "bash" ? "shell" : action,
                resource: "*",
                effect: permission[action],
              })),
          },
          hash: "blake3:mock",
          required: [],
        },
      ];
    }
    if (definition.runtime === "codex") {
      const own = definition.permissions?.codex ?? {};
      return [
        {
          id: "codex-scoped-config",
          target: "launch_settings",
          content: { ...own },
          hash: "blake3:mock",
          required: [],
        },
      ];
    }
    const own = definition.permissions?.claude ?? {};
    const mode =
      own.default_mode ??
      (definition.launch === "headless" ? CLAUDE_HEADLESS_DEFAULT_MODE : undefined);
    return [
      {
        id: "claude-launch-settings",
        target: "launch_settings",
        content: {
          permissions: {
            allow: [...CLAUDE_REQUIRED_ALLOW, ...(own.allow ?? [])],
            ...(own.deny?.length ? { deny: own.deny } : {}),
            ...(mode ? { defaultMode: mode } : {}),
          },
        },
        hash: "blake3:mock",
        required: CLAUDE_REQUIRED_ALLOW,
      },
    ];
  };

  /** `definition` resolved the way the backend's resolver does, for a launch by you. */
  const resolve = (
    key: string,
    definition: AgentDefinition,
    account: AgentAccountChoice,
  ): ResolvedAgentView => {
    const reference = { key, revision: definition.revision, definition_hash: "blake3:mock" };
    // The runtime's root instructions file is optional for every agent: where no layer has
    // it, the agent runs without it instead of reporting it missing.
    const rootDoc = ROOT_DOC[definition.runtime];
    const entries: AgentManifestEntry[] = [];
    const errors: ResolvedAgentView["manifest"]["errors"] = [];
    const owned = new Set<string>();
    const lookup = (path: string) => {
      const personal = filesIn(PERSONAL_CONTENT, path);
      if (personal.length)
        return { layer: PERSONAL_LAYER, content: PERSONAL_CONTENT, files: personal };
      const team = filesIn(TEAM_CONTENT, path);
      return team.length ? { layer: TEAM_LAYER, content: TEAM_CONTENT, files: team } : null;
    };
    const push = (path: string, origin: AgentManifestEntry["origin"]): string[] => {
      const found = lookup(path);
      if (!found) {
        if (origin.kind !== "required" && !(origin.kind === "declared" && path === rootDoc)) {
          errors.push({
            path,
            code: "missing",
            message:
              origin.kind === "dependency"
                ? `${path} (needed by ${origin.of}) is in no content layer`
                : `${path} is in no content layer`,
          });
        }
        return [];
      }
      const files = found.files.filter((f) => !owned.has(f));
      files.forEach((f) => owned.add(f));
      entries.push({
        path,
        origin,
        layer: found.layer,
        files: files.map((logical) => ({ logical, bytes: found.content[logical] })),
      });
      return files.flatMap((f) => REQUIRES[f] ?? []);
    };
    for (const raw of definition.startup ?? []) {
      const checked = normalizePath(raw);
      if ("error" in checked) {
        errors.push({ path: raw, code: "invalid", message: checked.error });
        continue;
      }
      for (const req of push(checked.path, { kind: "declared" })) {
        if (!owned.has(req) && !entries.some((e) => e.path === req)) {
          push(req, { kind: "dependency", of: checked.path });
        }
      }
    }
    const known = new Set(MCP_SERVERS);
    for (const server of definition.mcp?.servers ?? []) {
      if (!known.has(server)) {
        errors.push({
          path: null,
          code: "invalid",
          message: `MCP server "${server}" is not in the .mcp.json of ${TEAM_ORG}/${TEAM}; the launch will not have it`,
        });
      }
    }
    // SW-AN-50: the team's offered set, bridge aside. The silent-empty warning
    // below mirrors the backend's.
    const offeredMcpServers = MCP_SERVERS.filter((id) => id !== BRIDGE);
    const files = entries.flatMap((e) => e.files);
    const skillsDir = definition.runtime === "claude" ? ".claude/skills/" : ".agents/skills/";
    const provides = {
      commands: entries
        .filter((e) => e.path.startsWith(".claude/commands/") && e.path.endsWith(".md"))
        .map((e) => e.path.slice(".claude/commands/".length, -3)),
      skills: entries
        .filter(
          (e) => e.path.startsWith(skillsDir) && !e.path.slice(skillsDir.length).includes("/"),
        )
        .map((e) => e.path.slice(skillsDir.length)),
    };
    const generatedEntries = generated(definition);
    const warnings: AgentLaunchWarning[] = [];
    for (const entry of entries) {
      const bytes = entry.files.reduce((n, f) => n + f.bytes, 0);
      if (bytes > LARGE_ENTRY_BYTES) {
        warnings.push({
          code: "large_entry",
          message: `${entry.path} is ${(bytes / (1024 * 1024)).toFixed(1)} MB — unusually large for startup context`,
        });
      }
    }
    const claude = definition.permissions?.claude;
    const blocks = (r: string) =>
      CLAUDE_REQUIRED_ALLOW.includes(r) || ["*", "mcp__codesense", "mcp__codesense__*"].includes(r);
    if (
      definition.runtime === "claude" &&
      [...(claude?.deny ?? []), ...(claude?.ask ?? [])].some(blocks)
    ) {
      warnings.push({
        code: "denies_codesense_tools",
        message:
          "this agent denies codesense tools — it won't be able to signal_done / read routine_context",
      });
    }
    const prompt = definition.default_prompt?.trim() ?? "";
    if (prompt.startsWith("/")) {
      const name = prompt.slice(1).split(/\s/)[0];
      if (!provides.commands.includes(name) && !provides.skills.includes(name)) {
        warnings.push({
          code: "command_not_in_manifest",
          message: `this agent doesn't start with /${name} — add it to the agent or the run won't find it`,
        });
      }
    }
    // SW-AN-50: the agent names no MCP servers but the team's config offers some.
    // Non-blocking — launch succeeds and reaches only the bridge.
    if ((definition.mcp?.servers ?? []).length === 0 && offeredMcpServers.length > 0) {
      warnings.push({
        code: "no_mcp_servers_offered",
        message:
          `This agent has no MCP servers beyond the CodeSense bridge; the team offers: ${offeredMcpServers.join(", ")}. ` +
          `Copy this agent and add servers to grant access.`,
      });
    }
    const args = definition.args ?? [];
    const codex = definition.permissions?.codex;
    const escalating =
      args.includes("--dangerously-skip-permissions") ||
      args.includes("--dangerously-bypass-approvals-and-sandbox") ||
      (definition.runtime === "claude" && claude?.default_mode === "bypassPermissions") ||
      (definition.runtime === "codex" &&
        codex?.approval_policy === "never" &&
        codex?.sandbox_mode === "danger-full-access") ||
      (definition.runtime === "opencode" && args.includes("--auto")) ||
      (definition.runtime === "hermes" &&
        (hermesGolden(definition.id.endsWith("-unattended")).launch.escalating ||
          definition.permissions?.hermes?.approval === "yolo"));
    return {
      agent: reference,
      launch_admission:
        definition.runtime === "hermes"
          ? hermesAdmission(definition)
          : { allowed: true, reason: null, model_endpoint: false, model: null },
      launch: { id: key, binary: definition.runtime, args, escalating },
      account,
      manifest: {
        agent: reference,
        runtime: definition.runtime,
        entries,
        essentials: [
          { id: "codesense-bridge", reason: "CodeSense MCP bridge — required by CodeSense" },
          ...(definition.runtime === "claude"
            ? [
                {
                  id: "codesense-notify-settings",
                  reason: "CodeSense notification hook settings — required by CodeSense",
                },
              ]
            : []),
        ],
        generated: generatedEntries,
        totals: { files: files.length, bytes: files.reduce((n, f) => n + f.bytes, 0) },
        errors,
        manifest_hash: "blake3:mock",
      },
      provides,
      warnings,
      offered_mcp_servers: offeredMcpServers,
      manifest_applies_at_launch: manifestApplies,
      archived: Boolean(definition.archived_at),
    };
  };

  const detail = (key: string): AgentDetailView => {
    const entry = find(key);
    const { definition } = entry;
    return {
      key,
      scope: entry.scope,
      created_by: entry.created_by,
      created_by_you: definition.created_by === ME,
      definition,
      revisions: revisions[key] ?? [],
      resolved: resolve(key, definition, accountChoice(entry)),
    };
  };

  /** A form's fields as a definition on top of `base`, or the input the backend refuses. */
  const fromForm = (
    form: AgentForm,
    base: AgentDefinition,
  ): AgentDefinition | { field: string; message: string } => {
    if (!form.name?.trim()) return { field: "name", message: "an agent needs a name" };
    if (!isAgentRuntime(form.runtime)) {
      return {
        field: "runtime",
        message: `unknown runtime ${JSON.stringify(form.runtime)}; an agent runs on ${AGENT_RUNTIMES.join(", ")}`,
      };
    }
    const startup: string[] = [];
    for (const raw of form.startup ?? []) {
      const checked = normalizePath(raw);
      if ("error" in checked) return { field: "startup", message: checked.error };
      startup.push(checked.path);
    }
    return {
      ...base,
      name: form.name.trim(),
      description: form.description ?? "",
      runtime: form.runtime,
      args: form.args ?? [],
      startup_settings:
        form.startupSettings !== undefined ? form.startupSettings : base.startup_settings,
      launch: form.launch ?? "foreground",
      mcp: { servers: form.mcpServers ?? [] },
      startup,
      permissions: form.permissions ?? null,
      default_prompt: form.defaultPrompt ?? "",
      role: form.role ?? null,
      ...(form.resumeAfterRestart
        ? { resumeAfterRestart: true }
        : { resumeAfterRestart: undefined }),
    };
  };

  /** Where a new agent from `form` goes, and its key. */
  const newKey = (form: AgentForm): { key: string; scope: MockAgentEntry["scope"]; id: string } => {
    const base = slug(form.name ?? "");
    const prefix = form.scope === "team" ? `${form.org}/${form.team}` : `_personal/${ME}/personal`;
    let id = base;
    for (let n = 2; agents[`${prefix}/${id}`]; n += 1) id = `${base}-${n}`;
    return { key: `${prefix}/${id}`, scope: form.scope === "team" ? "team" : "personal", id };
  };

  /** A save's reply, with what a launch of the saved agent is warned about. */
  const savedReply = (key: string): AgentSaveReply => {
    const entry = find(key);
    return {
      outcome: "saved",
      key,
      revision: entry.definition.revision,
      warnings: resolve(key, entry.definition, accountChoice(entry)).warnings,
    };
  };

  const bump = (
    entry: MockAgentEntry,
    next: Partial<AgentDefinition>,
    author: RoutineAuthor = YOU,
  ) => {
    entry.definition = {
      ...entry.definition,
      ...next,
      revision: entry.definition.revision + 1,
    };
    const key = Object.keys(agents).find((k) => agents[k] === entry);
    if (key) record(key, author);
  };

  /** A routine key as the archive confirm lists it: its id, as its name. */
  const owned = (routineKey: string) => ({
    key: routineKey,
    name: routineKey.split("/").pop() ?? routineKey,
  });

  const saved = (key: string, entry: MockAgentEntry): AgentWriteReply => ({
    outcome: "saved",
    key,
    revision: entry.definition.revision,
    archived: Boolean(entry.definition.archived_at),
  });

  // ---- SW-XS-16: the agent's context (the editor's Context section) ----------------------
  const overrides: Record<string, AgentRepoOverride[]> = { ...(options.contextOverrides ?? {}) };
  const dismissed = new Set<string>();
  const contextTiers: MockContextTiers = {
    org: TEAM_ORG,
    team: TEAM,
    teamRoot: `/content/${TEAM_ORG}/${TEAM}`,
    teamFiles: TEAM_CONTENT,
    repoTier: (repo) =>
      repo.toLowerCase() === "github.com/acme/widgets"
        ? {
            root: `/content/${TEAM_ORG}/${TEAM}/repos/github.com/acme/widgets/content`,
            files: WIDGETS_REPO_CONTENT,
          }
        : null,
  };
  const contextAgent = (key: string): MockContextAgent => {
    const { definition } = find(key);
    return {
      key,
      name: definition.name || key.split("/").pop() || key,
      runtime: definition.runtime,
      revision: definition.revision,
      context: definition.context ?? null,
      startup: definition.startup ?? [],
      overrides: overrides[key] ?? [],
    };
  };
  // As `suggestion_for`: an entry is had when the agent's list (or the default accepting
  // would write out) already places it the same.
  const suggestionFor = (a: MockContextAgent): AgentContextSuggestion | null => {
    const suggestion = options.contextSuggestions?.[a.key];
    if (!suggestion || dismissed.has(`${a.key}\n${suggestion.fingerprint}`)) return null;
    const have = a.context ?? mockRuntimeDefault(a.runtime);
    const entries = suggestion.entries.filter((e) => !have.some((h) => placesLike(h, e)));
    return entries.length ? { ...suggestion, entries } : null;
  };

  let definitionRev = 0;
  let routinesRevision = 0;
  let selectedDetail: string | undefined;
  let definitionRows: HubRow[] = [];
  const definitionView = "agentDefinitions:1:native";
  const projectDefinitions = (): HubSnapshot => {
    const rows: HubRow[] = [{ id: "facts", rowType: "facts", routinesRevision }];
    // Demo worlds extend the same catalog handler before the first view is prepared.
    for (const agentRow of options.catalog?.() ??
      (handlers.agents_list!({ includeArchived: true }) as AgentRow[])) {
      const key = agentRow.key;
      rows.push({ id: `definition:${key}`, rowType: "definition", key, row: agentRow });
      rows.push({ id: `account:${key}`, rowType: "account", key, account: agentRow.account });
      if (key === selectedDetail && agents[key])
        rows.push({ id: `detail:${key}`, rowType: "detail", key, detail: detail(key) });
    }
    const before = new Map(definitionRows.map((r) => [r.id, r]));
    const upserts = rows.filter((r) => JSON.stringify(before.get(r.id)) !== JSON.stringify(r));
    const removes = definitionRows
      .filter((r) => !rows.some((next) => next.id === r.id))
      .map((r) => r.id);
    definitionRows = rows;
    if (upserts.length || removes.length) {
      definitionRev++;
      events.emit("codesense:delta", {
        topic: "agentDefinitions",
        viewKey: definitionView,
        rev: definitionRev,
        upserts,
        removes,
      });
    }
    return {
      topic: "agentDefinitions",
      viewKey: definitionView,
      rev: definitionRev,
      snapshot: definitionRows,
    };
  };
  for (const event of [
    AGENTS_CHANGED_EVENT,
    ROUTINES_CHANGED_EVENT,
    SESSION_STATE_CHANGED_EVENT,
    "pty-exit",
  ])
    events.observe(event, () => {
      if (event === ROUTINES_CHANGED_EVENT) routinesRevision++;
      projectDefinitions();
    });

  const handlers: HandlerRegistry = {
    agent_definitions_prepare: (args) => {
      if ("visible" in args)
        selectedDetail =
          args.visible === false ? undefined : (args.detailKey as string | undefined);
      return projectDefinitions();
    },
    live_hub_subscribe: () => projectDefinitions(),
    live_hub_unsubscribe: () => undefined,
    // No avatar packs on disk: the agents list draws the built-in packs.
    avatar_packs_list_for_team: () => ({
      packs: [],
      default_pack: null,
      agent_avatars: {},
      warnings: [],
    }),
    avatar_default_pack_set: () => undefined,
    avatar_pack_share_with_team: () => undefined,
    // Everyone's agents, then the starters, as the backend lists them.
    agents_list: (args) =>
      Object.entries(agents)
        .filter(([, entry]) => args.includeArchived === true || !entry.definition.archived_at)
        .sort(([, a], [, b]) => Number(a.scope === "builtin") - Number(b.scope === "builtin"))
        .map(([key, entry]) => row(key, entry)),
    agent_detail: (args) => detail(String(args.key)),
    agent_context_preview: (args) => {
      const a = contextAgent(String(args.key));
      return resolveMockContext(
        contextTiers,
        a,
        args.repo ? String(args.repo) : null,
        suggestionFor(a),
      );
    },
    agent_context_update: (args) => {
      const key = String(args.key);
      const entry = writable(key);
      const a = contextAgent(key);
      return mockContextUpdate(
        contextTiers,
        a,
        args,
        (accepted) => {
          overrides[key] = a.overrides;
          bump(entry, { context: a.context });
          // Accepting the suggestion dismisses it: one-time (SPEC §12.2).
          if (accepted) dismissed.add(`${key}\n${accepted}`);
          changed(key);
        },
        suggestionFor,
      );
    },
    agent_dismiss_context_suggestion: (args) => {
      dismissed.add(`${String(args.key)}\n${String(args.fingerprint)}`);
      return undefined;
    },
    agent_archive: (args) => {
      const key = String(args.key);
      const entry = writable(key);
      // Its routines go with it, once the user confirmed the list (as the backend refuses).
      if (entry.used_by.length > 0 && args.archiveRoutines !== true) {
        throw new Error(
          `archiving this agent also archives its routines (${entry.used_by
            .map((k) => owned(k).name)
            .join(", ")}): confirm first`,
        );
      }
      const archived = entry.used_by.map(owned);
      if (!entry.definition.archived_at) {
        bump(entry, { archived_at: new Date().toISOString() });
        entry.archivedRoutines = [...(entry.archivedRoutines ?? []), ...entry.used_by];
        entry.used_by = [];
        changed(key);
      }
      return { ...saved(key, entry), ...(archived.length > 0 ? { routines: archived } : {}) };
    },
    agent_unarchive: (args) => {
      const key = String(args.key);
      const entry = writable(key);
      if (entry.definition.revision !== args.baseRevision) {
        return { outcome: "conflict", current_revision: entry.definition.revision };
      }
      bump(entry, { archived_at: null });
      changed(key);
      const offered = (entry.archivedRoutines ?? []).map(owned);
      return { ...saved(key, entry), ...(offered.length > 0 ? { routines: offered } : {}) };
    },
    agent_restore_routines: (args) => {
      const key = String(args.key);
      const entry = writable(key);
      const restored = entry.archivedRoutines ?? [];
      entry.used_by = [...entry.used_by, ...restored];
      entry.archivedRoutines = [];
      changed(key);
      return restored.map(owned);
    },
    agent_delete: (args): AgentDeleteReply => {
      const key = String(args.key);
      const entry = writable(key);
      if (entry.definition.revision !== args.baseRevision) {
        return { outcome: "conflict", current_revision: entry.definition.revision };
      }
      if (entry.used_by.length > 0) {
        return { outcome: "in_use", referrers: entry.used_by, owned: entry.used_by };
      }
      delete agents[key];
      changed(key);
      return { outcome: "deleted", key };
    },
    agent_copy_to_mine: (args) => {
      const source = find(args.key);
      let id = `${source.definition.id}-copy`;
      for (let n = 2; agents[`_personal/${ME}/personal/${id}`]; n += 1) {
        id = `${source.definition.id}-copy-${n}`;
      }
      const key = `_personal/${ME}/personal/${id}`;
      agents[key] = {
        ...source,
        definition: { ...source.definition, id, created_by: ME, revision: 1, archived_at: null },
        scope: "personal",
        created_by: "you",
        used_by: [],
        bound: undefined,
        history: undefined,
      };
      revisions[key] = [];
      record(key, YOU);
      changed(key);
      return saved(key, agents[key]);
    },
    agent_binding_set: (args) => {
      const key = String(args.key);
      const entry = find(key);
      if (args.accountId != null) {
        const account = accounts.find((a) => a.id === args.accountId);
        if (!account) throw new Error(`agent account ${String(args.accountId)} not found`);
        if (account.provider !== entry.definition.runtime) {
          throw new Error(
            `account ${account.id} is a ${account.provider} account but the agent runs on ${entry.definition.runtime}`,
          );
        }
        entry.bound = account.id;
      }
      if (typeof args.captureRawTerminal === "boolean") {
        entry.captureRaw = args.captureRawTerminal;
      }
      changed(key);
      return {
        account_id: entry.bound ?? null,
        account_exists: entry.bound !== undefined && accounts.some((a) => a.id === entry.bound),
        capture_raw_terminal: entry.captureRaw === true,
      };
    },
    agent_binding_clear: (args) => {
      const key = String(args.key);
      const entry = find(key);
      const had = entry.bound !== undefined;
      entry.bound = undefined;
      if (had) changed(key);
      return had;
    },
    agent_save: (args): AgentSaveReply => {
      const form = args.form as AgentForm;
      if (form.key) {
        const entry = writable(form.key);
        if (entry.definition.revision !== args.baseRevision) {
          return { outcome: "conflict", current_revision: entry.definition.revision };
        }
        const next = fromForm(form, entry.definition);
        if ("field" in next) return { outcome: "invalid", ...next };
        bump(entry, next);
        changed(form.key);
        return savedReply(form.key);
      }
      const { key, scope, id } = newKey(form);
      const next = fromForm(form, agent(id, "", "claude"));
      if ("field" in next) return { outcome: "invalid", ...next };
      agents[key] = { definition: next, scope, created_by: "you", used_by: [] };
      revisions[key] = [];
      record(key, YOU);
      changed(key);
      return savedReply(key);
    },
    agent_preview: (args): AgentPreviewReply => {
      const form = args.form as AgentForm;
      const existing = form.key ? find(form.key) : null;
      const target = existing ? { key: form.key as string } : newKey(form);
      const base = existing?.definition ?? agent(slug(form.name ?? ""), "", "claude");
      const next = fromForm(form, base);
      if ("field" in next) return { outcome: "invalid", ...next };
      const account = existing
        ? accountChoice(existing)
        : accountChoice({
            definition: next,
            scope: "personal",
            created_by: "you",
            used_by: [],
          });
      return {
        outcome: "resolved",
        resolved: resolve(target.key, next, account),
        servers: MCP_SERVERS.filter((id) => id !== BRIDGE),
      };
    },
    agent_restore_revision: (args): AgentSaveReply => {
      const key = String(args.key);
      const entry = writable(key);
      if (entry.definition.revision !== args.baseRevision) {
        return { outcome: "conflict", current_revision: entry.definition.revision };
      }
      const old = revisions[key]?.find((r) => r.revision === args.revision);
      if (!old) throw new Error(`agent ${key} has no revision ${String(args.revision)}`);
      bump(entry, { ...old.agent, archived_at: entry.definition.archived_at });
      changed(key);
      return savedReply(key);
    },
    agent_startup_catalog: (args): AgentStartupCatalog => {
      const dir = typeof args.dir === "string" ? args.dir.replace(/^\/+|\/+$/g, "") : "";
      const found = new Map<string, AgentStartupCatalog["entries"][number]>();
      const layers: [AgentContentLayer, Record<string, number>][] = [
        [PERSONAL_LAYER, PERSONAL_CONTENT],
        [TEAM_LAYER, TEAM_CONTENT],
      ];
      for (const [layer, content] of layers) {
        for (const file of Object.keys(content)) {
          const rest = dir
            ? file.startsWith(`${dir}/`)
              ? file.slice(dir.length + 1)
              : null
            : file;
          if (rest === null) continue;
          const [name, ...below] = rest.split("/");
          const path = dir ? `${dir}/${name}` : name;
          if (!found.has(path)) found.set(path, { path, is_dir: below.length > 0, layer });
        }
      }
      const filter = typeof args.filter === "string" ? args.filter.trim().toLowerCase() : "";
      const offset = typeof args.offset === "number" ? args.offset : 0;
      const names = [...found.values()]
        .filter(
          (e) =>
            !filter ||
            e.path
              .slice(dir ? dir.length + 1 : 0)
              .toLowerCase()
              .includes(filter),
        )
        .sort((a, b) => a.path.localeCompare(b.path));
      const end = Math.min(offset + CATALOG_PAGE, names.length);
      const entries = names.slice(offset, end).sort((a, b) => Number(b.is_dir) - Number(a.is_dir));
      const more = end < names.length;
      return { dir, entries, total: names.length, truncated: more, next_offset: more ? end : null };
    },
    agent_open_authoring_agent: () => {
      if (options.authoringOpenError !== undefined) {
        return { opened: false, message: options.authoringOpenError };
      }
      authoringOpens += 1;
      return { opened: true, message: "Opened Agent author in the team workspace." };
    },
    list_agent_accounts: () => accounts.filter((a) => !a.detachedAt),
    // The launch registry, as far as an agent launch and the running counts need it.
    // A fresh array per call, as IPC delivers it, so a roster reload sees the change.
    dashboard_list_sessions: () => sessions.map((row) => ({ ...row })),
    // No durable roster: the live registry is the whole picture, and a read of both answers, so
    // the pane reads the roster as whole (a failed read would keep it from pruning looks and pins).
    wv_list_roster: () => [],
    // A person's launch from the launch sheet: the session joins the roster under its
    // manager, with the name, role and face it was given, as the real open records them.
    agent_launch_open: (raw) => {
      const args = (raw as { args?: AgentLaunchOpenArgs }).args ?? ({} as AgentLaunchOpenArgs);
      const name = (args.name ?? "").trim();
      const slug = name
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-+|-+$/g, "");
      if (!slug) {
        return {
          status: "invalid",
          handle: null,
          name: null,
          message: "Not opened: name is required.",
        };
      }
      const handle =
        args.target === "story"
          ? `${(args.storyId ?? "").trim()}#${slug}`
          : `_worker/${TEAM_ORG}/${TEAM}/${slug}`;
      if (sessions.some((r) => r.handle === handle && r.alive)) {
        return { status: "alreadyLive", handle, name, message: `${name} is already running.` };
      }
      const key = args.agent ?? "builtin/claude";
      const selected = agents[key];
      if (selected) {
        const admission = resolve(
          key,
          selected.definition,
          accountChoice(selected),
        ).launch_admission;
        if (admission?.allowed === false)
          return { status: "invalid", handle: null, name: null, message: admission.reason };
      }
      const revision = selected?.definition.revision ?? BUILTIN_REVISION;
      sessions.push({
        ...sessionRow(handle, `sess-${slug}-${sessions.length}`, `${key}@${revision}`),
        startedAtMs: Date.now(),
        displayName: name,
        launchedBy: args.launchedBy ?? null,
        story: args.target === "story" ? (args.storyId ?? null) : null,
        role: args.role ?? null,
        avatar: args.avatar ?? null,
        startedWith: args.initialPrompt ?? null,
      });
      events.emit(SESSION_STATE_CHANGED_EVENT, null);
      changed(key);
      return { status: "opened", handle, name, message: `Opened ${name}.` };
    },
    dashboard_launch_begin: () => true,
    dashboard_launch_abort: () => undefined,
    // A profile launch's per-session config: nothing to add without a real backend.
    project_mcp_scope_config: () => ({ extraArgs: [], environmentBinding: null }),
    dashboard_kill_by_handle: (args) => {
      const at = sessions.findIndex((s) => s.handle === String(args.handle));
      if (at >= 0) sessions.splice(at, 1);
      events.emit(SESSION_STATE_CHANGED_EVENT, null);
      return undefined;
    },
    dashboard_launch_bind: (args) => {
      const agentRevision = typeof args.agentRevision === "string" ? args.agentRevision : null;
      const handle = String(args.handle);
      const existing = sessions.findIndex((s) => s.handle === handle);
      const next = sessionRow(handle, String(args.sessionId), agentRevision);
      if (existing >= 0) sessions[existing] = next;
      else sessions.push(next);
      // As the real bind does, so the agents roster re-enumerates.
      events.emit(SESSION_STATE_CHANGED_EVENT, null);
      const key = agentRevision?.slice(0, agentRevision.lastIndexOf("@"));
      if (key) changed(key);
      return undefined;
    },
  };

  return {
    handlers,
    setManifestAppliesAtLaunch: (applies) => {
      manifestApplies = applies;
      for (const key of Object.keys(agents)) changed(key);
    },
    editAs: (key, patch, author) => {
      bump(find(key), patch, author);
      changed(key);
    },
    authoringOpened: () => authoringOpens,
    addSession: (row) => {
      sessions.push(row);
      events.emit(SESSION_STATE_CHANGED_EVENT, null);
    },
    ask: (req) => events.emit("mcp:ask-user", req),
  };
}

function sessionRow(
  handle: string,
  sessionId: string,
  agentRevision: string | null,
): AgentSessionRow {
  return {
    handle,
    sessionId,
    kind: "dashboard-launched",
    alive: true,
    status: "running",
    backgrounded: false,
    startedWith: null,
    startedAtMs: Date.now() - 4 * 60_000,
    launcher: "virtual://agents",
    agentRevision,
    lastOutputMs: null,
  };
}

/** The app's mock agents world: one session already runs Triage; under `?agents=team` a
 *  whole team is at work (see `agents-team.ts`), and under `?agents=none` nothing runs. A mock walk can reach it as
 *  `window.__mockAgents` to change an agent the way a teammate or an agent would. */
export function agentHandlers(
  events: MockEventBus,
  search = "",
  catalog?: () => AgentRow[],
): HandlerRegistry {
  const team = isAgentTeamWorld(search);
  const teamSessions = team ? agentTeamSessions(search) : [];
  const world = createMockAgents(events, {
    catalog,
    ...CONTEXT_DEMO,
    ...(team
      ? { sessions: teamSessions }
      : new URLSearchParams(search).get("agents") === "none"
        ? { sessions: [] }
        : { running: [`${TEAM_ORG}/${TEAM}/triage@3`] }),
  });
  (globalThis as { __mockAgents?: MockAgents }).__mockAgents = world;
  if (!team) return world.handlers;
  const list = world.handlers.dashboard_list_sessions;
  const inbox = agentTeamInboxHandlers((event) => events.emit(event, null), {
    simulate: true,
    sessions: teamSessions,
  });
  const hub = agentListHub(events, {
    ...agentTeamListWorld(search, () => teamSessions),
    // Every saved agent has a spend row; Triage has a daily cap it is close to.
    spend: () => {
      const none = {
        today: 0,
        week: 0,
        cap: null,
        state: "ok" as const,
        pending: false,
        requestedCap: null,
        unpricedModels: [],
      };
      const rows = world.handlers.agents_list({ includeArchived: true }) as { key: string }[];
      return Object.fromEntries(
        rows.map((r) => [
          `agent:${r.key}`,
          r.key === `${TEAM_ORG}/${TEAM}/triage`
            ? {
                ...none,
                today: 40,
                week: 80,
                cap: { perDay: 50, perWeek: 200 },
                state: "warn" as const,
              }
            : none,
        ]),
      );
    },
    unread: () =>
      Object.fromEntries(inbox.agent_read_sessions().map((s) => [s.handle, s.unread ?? 0])),
  });
  return {
    ...world.handlers,
    ...inbox,
    // The `sessions` and `agentList` views the live hub serves the app, as the backend projects them.
    live_hub_agent_list_read: (args) => hub.snapshot(String(args.topic), String(args.viewKey)),
    agent_read_roles: (args): { roles: RoleQueue[] } => {
      const team = args.team as { org: string; team: string } | undefined;
      if (team && (team.org !== MOCK_ORG_SLUG || team.team !== MOCK_TEAM_NAME))
        return { roles: [] };
      const { messages, roles } = inbox.list_inbox_rollup();
      const now = Date.now();
      return {
        roles: roles
          .filter((queue) => !args.role || queue.role === args.role)
          .map(({ role }) => {
            const count = (state: RoleDeliveryState) =>
              messages.filter((m) => m.toRole === role && m.delivery?.state === state).length;
            return {
              role,
              roleName: role,
              registrations: teamSessions
                .filter((s) => s.role === role)
                .map((s) => ({
                  instanceId: s.handle,
                  instanceName: s.displayName ?? s.handle,
                  generation: 1,
                  registeredAtMs: now,
                  lastRenewedAtMs: now,
                  expiresAtMs: now + 60_000,
                  live: s.alive,
                })),
              counts: {
                pending: count("pending"),
                claimed: count("claimed"),
                accepted: count("accepted"),
                acknowledged: count("acknowledged"),
              },
              deliveries: [],
              deliveriesTruncated: messages.some((m) => m.toRole === role),
            };
          }),
      };
    },
    // The verifier's question arrives as the real event would, once the app is listening;
    // a repeat is idempotent (same correlation id).
    dashboard_list_sessions: (args) => {
      setTimeout(() => {
        events.emit("mcp:ask-user", AGENT_TEAM_ASK);
        events.emit("terminal-status-update", HAND_STARTED_STATUS);
        if (isAgentTeamsWorld(search))
          for (const hook of SECOND_TEAM_STATUSES) events.emit("terminal-status-update", hook);
      }, 0);
      return list(args);
    },
  };
}

// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { MockState } from "../mock-state";
import type { MockEventBus } from "../mock-events";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME, TEAM_CONTENT_PATH } from "../fixtures";
import { AGENTS_CHANGED_EVENT } from "@/tauri/events";
import type { AgentContextEditReply, AgentRow } from "@/tauri/commands";
import { isLeftNavDemo, leftNavDemoTeamFiles } from "./left-nav-demo";
import {
  mockContextUpdate,
  resolveMockContext,
  type MockContextAgent,
  type MockContextTiers,
} from "../lib/mock-agent-context";

/**
 * SW-XS-15 — the left nav's agent context world, opt-in with `?context=demo` (every other
 * mock surface boots unchanged). It seeds the team's content (so the nav has rows), two team
 * agents (Barb, Bubbles) and a codesense repo tier, and answers `agent_context_preview` /
 * `agent_context_update` the way the backend resolves the want set:
 * `(context | runtime default) + startup + override.add − override.remove`, the repo tier
 * winning per file. `window.__mockAgentContext.editAs(key, false)` bumps an agent's revision as a
 * teammate would, so the revision-conflict toast can be walked.
 */

const ORG = MOCK_ORG_SLUG;
const TEAM = MOCK_TEAM_NAME;
const REPO_TIER = `/mock/repos/github.com/acme/widgets/content`;

const TEAM_FILES: Record<string, number> = {
  "CLAUDE.md": 2_400,
  ".claude/commands/bmad-help.md": 1_500,
  ".claude/skills/review/SKILL.md": 3_100,
  ".claude/skills/review/checklist.md": 1_900,
  ".claude/skills/triage-slack/SKILL.md": 1_800,
  ".claude/settings.local.json": 300,
  "docs/architecture/adr-001-agents.md": 6_400,
  "docs/architecture/adr-002-sync.md": 5_100,
  "docs/guide.md": 4_100,
  "_bmad-output/04-sync-engine/SPEC.md": 48_000,
  "_bmad-output/04-sync-engine/stories/SW-XS-15.md": 7_200,
  "dashboards/sprint.html": 12_000,
};
const REPO_FILES: Record<string, number> = {
  "docs/architecture/adr-003-repo-only.md": 3_300,
  ".claude/skills/review/SKILL.md": 3_400,
};

type ContextAgent = MockContextAgent;

export function isAgentContextWorld(search: string): boolean {
  return new URLSearchParams(search).get("context") === "demo";
}

export function agentContextHandlers(
  state: MockState,
  events: MockEventBus,
  base: HandlerRegistry,
  search = "",
): HandlerRegistry {
  const teamFiles = isLeftNavDemo(search)
    ? { ...TEAM_FILES, ...leftNavDemoTeamFiles() }
    : TEAM_FILES;
  for (const [rel, bytes] of Object.entries(teamFiles)) {
    const path = `${TEAM_CONTENT_PATH}/${rel}`;
    state.contentFiles.put({ id: path, path, content: "x".repeat(Math.min(bytes, 200)) });
  }
  const agents: Record<string, ContextAgent> = {};
  for (const [id, name] of [
    ["barb", "Barb"],
    ["bubbles", "Bubbles"],
  ]) {
    const key = `${ORG}/${TEAM}/${id}`;
    agents[key] = {
      key,
      name,
      runtime: "claude",
      revision: 3,
      // In the left nav's demo, Barb works the inbox and left-nav epics: her context
      // holds those two folders beside the runtime default.
      context:
        id === "barb" && isLeftNavDemo(search)
          ? [
              { path: "CLAUDE.md" },
              { path: ".claude/skills" },
              { path: "26-agent-inbox" },
              { path: "27-left-nav" },
            ]
          : null,
      startup: ["CLAUDE.md"],
      overrides: [],
    };
  }
  const find = (key: unknown) => {
    const a = agents[String(key)];
    if (!a) throw new Error(`agent ${String(key)} not found`);
    return a;
  };
  const tiers: MockContextTiers = {
    org: ORG,
    team: TEAM,
    teamRoot: TEAM_CONTENT_PATH,
    teamFiles,
    repoTier: () => ({ root: REPO_TIER, files: REPO_FILES }),
    // The demo's few files stand in for a real team's want set: scale the totals so the
    // header reads like one.
    scale: { files: 101, bytes: 520 },
  };

  (globalThis as { __mockAgentContext?: unknown }).__mockAgentContext = {
    // `announce: false` is a teammate's edit this Mac has not heard about yet (a pull not
    // yet noticed): the next edit from the nav is then made against a stale revision.
    editAs: (key: string, announce = true) => {
      find(key).revision += 1;
      if (announce) events.emit(AGENTS_CHANGED_EVENT, { key });
    },
  };

  const baseList = base.agents_list;
  return {
    agents_list: (args) => {
      const rows = (baseList ? (baseList(args) as AgentRow[]) : []).slice();
      const template = rows[0];
      for (const a of Object.values(agents)) {
        rows.unshift({
          ...(template ?? ({} as AgentRow)),
          key: a.key,
          name: a.name,
          description: "",
          scope: "team",
          org: ORG,
          team: TEAM,
          archived: false,
          revision: a.revision,
          avatar: null,
          used_by: [],
          routines: [],
          archived_routines: [],
          model: null,
          running_now: 0,
        });
      }
      return rows;
    },
    // Every other agent is the agents world's (its editor's Context section).
    agent_context_preview: (args) =>
      !agents[String(args.key)] && base.agent_context_preview
        ? base.agent_context_preview(args)
        : resolveMockContext(tiers, find(args.key), args.repo ? String(args.repo) : null),
    agent_context_update: (args) => {
      if (!agents[String(args.key)] && base.agent_context_update) {
        return base.agent_context_update(args);
      }
      const a = find(args.key);
      const reply: AgentContextEditReply = mockContextUpdate(tiers, a, args, () =>
        events.emit(AGENTS_CHANGED_EVENT, { key: a.key, revision: a.revision }),
      );
      return reply;
    },
  };
}

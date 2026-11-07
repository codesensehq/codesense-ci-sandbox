// SPDX-License-Identifier: MIT
import { TEAM_CONTENT_PATH } from "../fixtures";
import type { HandlerRegistry } from "../mock-router";
import type { MockState } from "../mock-state";
import type { MockEventBus } from "../mock-events";
import { agentListHub, placeTabs, type AgentListWorld, type Placement } from "./agent-list-hub";
import { TEAM_KEY } from "./agents-team";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import type { Workspace } from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";

/**
 * The `?nav=demo` world: a team's content with epics, stories in every stage (and one outside
 * the taxonomy), not-started stories both ready to start and waiting on dependencies, story
 * workspaces whose agents are in mixed states, and files of every kind, so the left nav's stage
 * rails, agent marks, epic roll-ups, file drawings, lens menu and stage arrangement can all be
 * walked. Combine with `context=demo` for the agent-context scope.
 */
export function isLeftNavDemo(search: string): boolean {
  return new URLSearchParams(search).get("nav") === "demo";
}

const ROOT = TEAM_CONTENT_PATH;

/** A story, and the stories it depends on (by story id). */
type Story = [id: string, title: string, status: string, dependsOn?: string[]];

const EPICS: Array<{ dir: string; stories: Story[]; docs: string[] }> = [
  {
    dir: "26-agent-inbox",
    stories: [
      ["AID-1", "Inbox read model", "done"],
      ["AID-2", "Bridge and MCP tools", "done"],
      ["AID-3", "Rail list and filters", "review"],
      ["AID-4", "Dashboards from the inbox", "in-progress"],
      ["AID-5", "Sender identity", "todo", ["AID-1", "AID-2"]],
    ],
    docs: ["inbox-flow.excalidraw", "rail-mock.png", "dashboards/inbox-board.html"],
  },
  {
    dir: "27-left-nav",
    stories: [
      ["LN-1", "Story rows show their agents", "in-progress"],
      ["LN-2", "Quick filters in the field", "planning", ["LN-1"]],
      ["LN-3", "One icon family", "blocked"],
      ["LN-4", "Stage rail mark", "review"],
      ["LN-5", "Someday: nav themes", "someday"],
      ["LN-6", "Inbox count in the nav", "todo", ["AID-3", "LN-4"]],
    ],
    docs: ["icon-spec.pdf", "walkthrough.mp4"],
  },
  {
    dir: "28-remote-machines",
    stories: [
      ["RM-1", "Machine registry", "done"],
      ["RM-2", "Add a machine", "done"],
      ["RM-3", "Terminal server install", "done"],
      ["RM-4", "Remote tabs", "done"],
      ["RM-7", "Reattach after sleep", "in-progress"],
      ["RM-8", "Machine colours", "qa"],
      ["RM-9", "Machine picker in launch", "in-progress"],
      ["RM-10", "Offline banner", "todo", ["RM-7", "RM-9"]],
      ["RM-11", "Passphrase capture", "planning", ["RM-3"]],
      ["RM-12", "Share a machine", "todo"],
    ],
    docs: ["kickoff.pptx", "server-bundle.zip"],
  },
  {
    dir: "29-routines",
    stories: [
      ["RT-1", "Routine model", "done"],
      ["RT-2", "Cron triggers", "done"],
      ["RT-3", "Run history", "done"],
      ["RT-4", "Owner arms", "done"],
      ["RT-5", "Results lane", "done"],
      ["RT-6", "Routine tab", "done"],
    ],
    docs: [],
  },
];

/** Root-level files the scanner owns (docs) and the ones the viewer-root listing shows. */
const ROOT_DOCS = ["roadmap.html", "README.md"];
const ROOT_FILES = ["team.yaml", "metrics.csv", "setup.sh", "data.json", "dashboards/sprint.html"];

const storyPath = (dir: string, id: string) => `${ROOT}/${dir}/${id}.md`;

function story(dir: string, [id, title, status, dependsOn]: Story): StoryRef {
  return {
    team: null,
    fileName: `${id}.md`,
    filePath: storyPath(dir, id),
    dependsOn: dependsOn ?? null,
    status,
    documentType: "story",
    supportingDocs: null,
    storyId: id,
    sourceRepo: null,
    openTags: { title: [title] },
  };
}

/** The demo's team content, by content-relative path, for the agent-context want set. */
export function leftNavDemoTeamFiles(): Record<string, number> {
  const files: Record<string, number> = {};
  for (const { dir, stories, docs } of EPICS) {
    files[`${dir}/README.md`] = 1_200;
    for (const [id] of stories) files[`${dir}/${id}.md`] = 2_000;
    for (const d of docs) files[`${dir}/${d}`] = 4_000;
  }
  for (const f of [...ROOT_DOCS, ...ROOT_FILES]) files[f] = 800;
  return files;
}

/** The scanned tree the demo team's content resolves to. */
export function leftNavDemoTree(root: string = ROOT): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: root,
    orderError: null,
    mainDoc: null,
    stories: [],
    docs: ROOT_DOCS.map((f) => ({ fileName: f, filePath: `${root}/${f}` })),
    subEpics: EPICS.map(({ dir, stories, docs }) => ({
      dirPath: dir,
      dirName: dir,
      absoluteDirPath: `${root}/${dir}`,
      orderError: null,
      mainDoc: { filePath: `${root}/${dir}/README.md`, supportingDocs: null, purposeText: null },
      stories: stories.map((s) => story(dir, s)),
      docs: docs.map((f) => ({
        fileName: f.split("/").pop()!,
        filePath: `${root}/${dir}/${f}`,
      })),
      subEpics: [],
    })),
  } as DerivedPipelineTree;
}

/** One agent tab in a story workspace, and how it reports. */
interface DemoTab {
  story: string;
  dir: string;
  session: string;
  name: string;
  /** The hook status it sends, or how its process ended. */
  status?: "running" | "needs-permission" | "error" | "done";
  exit?: { code: number };
}

const DEMO_TABS: DemoTab[] = [
  { story: "AID-2", dir: "26-agent-inbox", session: "nav-aid2", name: "claude", status: "done" },
  { story: "AID-3", dir: "26-agent-inbox", session: "nav-aid3", name: "codex", exit: { code: 0 } },
  {
    story: "AID-4",
    dir: "26-agent-inbox",
    session: "nav-aid4-a",
    name: "claude",
    status: "needs-permission",
  },
  {
    story: "AID-4",
    dir: "26-agent-inbox",
    session: "nav-aid4-b",
    name: "codex",
    status: "running",
  },
  { story: "LN-1", dir: "27-left-nav", session: "nav-ln1-a", name: "claude", status: "running" },
  { story: "LN-1", dir: "27-left-nav", session: "nav-ln1-b", name: "reviewer", status: "running" },
  { story: "LN-3", dir: "27-left-nav", session: "nav-ln3", name: "claude", status: "error" },
  { story: "LN-4", dir: "27-left-nav", session: "nav-ln4", name: "claude", exit: { code: 0 } },
  {
    story: "RM-12",
    dir: "28-remote-machines",
    session: "nav-rm12",
    name: "claude",
    status: "done",
  },
  {
    story: "RM-7",
    dir: "28-remote-machines",
    session: "nav-rm7",
    name: "claude",
    status: "running",
  },
];

/** The story workspaces the demo's agents work in, as persisted viewer state holds them. */
export const LEFT_NAV_DEMO_WORKSPACES = [...new Set(DEMO_TABS.map((t) => t.story))].map((id) => {
  const tab = DEMO_TABS.find((t) => t.story === id)!;
  const epic = EPICS.find((e) => e.dir === tab.dir)!;
  const [, title, status] = epic.stories.find(([sid]) => sid === id)!;
  return {
    id,
    kind: "story",
    cwd: `${ROOT}/worktrees/${id}`,
    status: "ready",
    storyId: id,
    storyTitle: title,
    storyStatus: status,
    storyFilePath: storyPath(tab.dir, id),
  };
});

/** The agent CLI running under each live demo tab, as the process probe finds it. */
export function leftNavDemoProcesses(): Record<string, string> {
  return Object.fromEntries(
    DEMO_TABS.filter((t) => !t.exit).map((t) => [
      t.session,
      t.name === "codex" ? "codex" : "claude",
    ]),
  );
}

/** The demo's tabs, as the terminal host seeds them. */
export function leftNavDemoTabs(): Array<{
  workspaceId: string;
  dir: string;
  session: string;
  name: string;
  exit?: { code: number };
}> {
  return DEMO_TABS.map((t) => ({
    workspaceId: t.story,
    dir: `${ROOT}/worktrees/${t.story}`,
    session: t.session,
    name: t.name,
    exit: t.exit,
  }));
}

/** The demo's agents as the backend projects them: each tab's agent in its story workspace, its
 *  status the one its hook reports. */
function leftNavDemoListWorld(): AgentListWorld {
  const sessions: AgentSessionRow[] = DEMO_TABS.map((t) => ({
    handle: t.session,
    sessionId: t.session,
    displayName: t.name,
    kind: "dashboard-launched",
    alive: !t.exit,
    status: "running",
    backgrounded: false,
    startedWith: null,
    startedAtMs: Date.now(),
    launcher: "codesense",
    agentRevision: null,
    lastOutputMs: null,
  }));
  const placements = new Map<string, Placement>();
  placeTabs(
    placements,
    TEAM_KEY,
    DEMO_TABS.map((t) => ({ workspaceId: t.story, session: t.session })),
  );
  const status = {
    running: "running",
    "needs-permission": "blocked",
    error: "failed",
    done: "idle",
  } as const;
  return {
    sessions: () => sessions,
    placements,
    workspaces: { [TEAM_KEY]: LEFT_NAV_DEMO_WORKSPACES as Workspace[] },
    teamOrder: [TEAM_KEY],
    statusFields: (row) => {
      const tab = DEMO_TABS.find((t) => t.session === row.sessionId);
      return { effectiveStatus: tab?.exit ? "completed" : status[tab?.status ?? "running"] };
    },
  };
}

/**
 * The demo's handlers over the base registry: the scanned tree, the root files the viewer
 * listing shows, and each agent's hook status, sent once the app is listening.
 */
export function leftNavDemoHandlers(
  state: MockState,
  events: MockEventBus,
  base: HandlerRegistry,
): HandlerRegistry {
  for (const rel of ROOT_FILES) {
    const path = `${ROOT}/${rel}`;
    if (!state.contentFiles.has(path)) state.contentFiles.put({ id: path, path, content: "x" });
  }
  for (const { dir, stories } of EPICS) {
    for (const [id] of stories) {
      const path = storyPath(dir, id);
      if (!state.contentFiles.has(path)) state.contentFiles.put({ id: path, path, content: "x" });
    }
  }
  const announce = () => {
    for (const t of DEMO_TABS) {
      if (!t.status) continue;
      events.emit("terminal-status-update", {
        tab_session_id: t.session,
        status: t.status,
        hook_event: t.status === "running" ? "PreToolUse" : undefined,
      });
    }
  };
  const list = base.dashboard_list_sessions;
  const hub = agentListHub(events, leftNavDemoListWorld());
  return {
    live_hub_agent_list_read: (args) => hub.snapshot(String(args.topic), String(args.viewKey)),
    scan_pipeline_tree: (args) => leftNavDemoTree(String(args.teamContentPath).replace(/\/+$/, "")),
    dashboard_list_sessions: (args) => {
      setTimeout(announce, 0);
      return list ? list(args) : [];
    },
  };
}

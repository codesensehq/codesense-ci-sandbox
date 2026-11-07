// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, renderHook, screen, within } from "@/test-utils";

import { PipelineStatusNav } from "../pipeline-status-nav";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import {
  ScopedStatusMark,
  SessionStatusMark,
} from "@/features/viewer/components/session-status-mark";
import {
  getNeedsYouState,
  selectNeedsYouKind,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { markOfAgentStatus, markOfNeedsYouKind } from "@/shared/lib/status-mark";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import type { AgentSessionRow } from "@/tauri/commands";

// The nav's agent marks read the SAME scope the workspace pill, the tray, the tabs, the agents
// list and the needs-you notifications read, so for one story workspace in each live state they
// all show one mark, in the Team scope and in an agent's context scope alike.

const TEAM = "acme:platform";

vi.mock("@/shared/lib/send-to-terminal", () => ({ sendTextToTerminal: vi.fn() }));
vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return { ...actual, useActiveTeamKey: () => TEAM, getActiveTeamKey: () => TEAM };
});
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return {
    ...actual,
    listDirectory: () => Promise.resolve([]),
    getTeamNavPins: () => Promise.resolve([]),
  };
});
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

installTerminalLayoutHarness();

const STORY_PATH = "/ctx/26-inbox/AID-4.md";
const SESSION = "sess-aid4";

function seedStoryWorkspace(
  status: AgentStatus,
  needsYouKind: AgentSessionRow["needsYouKind"] = null,
) {
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        {
          id: "AID-4",
          kind: "story",
          cwd: "/wt/AID-4",
          status: "ready",
          storyId: "AID-4",
          storyFilePath: STORY_PATH,
          storyStatus: "in-progress",
        },
      ],
    } as never,
    aiStatusBySession: {},
    statusNavExpandedPaths: { [TEAM]: ["/ctx/26-inbox"] },
  });
  seedTerminalTabs(TEAM, "AID-4", [{ session: SESSION, name: "claude" }]);
  projectSessionStatus(SESSION, status, { needsYouKind });
}

function tree() {
  return buildTree("root", {
    absoluteDirPath: "/ctx",
    subEpics: [
      buildTree("26-inbox", {
        dirPath: "26-inbox",
        absoluteDirPath: "/ctx/26-inbox",
        mainDoc: { filePath: "/ctx/26-inbox/README.md", supportingDocs: null, purposeText: null },
        stories: [
          {
            ...buildStoryRef("AID-4.md", { absolutePath: STORY_PATH, status: "in-progress" }),
            storyId: "AID-4",
          },
        ],
      }),
    ],
  });
}

/** An agent's context in which the story is in the want set. */
const AGENT_CONTEXT: NavAgentContext = {
  agentName: "Barb",
  repoName: null,
  relOf: (abs) => (abs.startsWith("/ctx/") ? abs.slice(5) : null),
  editRelOf: () => null,
  defaulted: false,
  membership: () => ({ state: "in", tier: "team" }) as never,
  plan: () => null,
  edit: () => {},
};

/** Every surface's mark for the seeded session, read once. */
function marksEverywhere(agentContext?: NavAgentContext) {
  render(
    <>
      <PipelineStatusNav
        tree={tree()}
        repoPath="/ctx"
        rootPath="/ctx"
        selectedPath={null}
        onFileSelect={() => {}}
        onOpenStoryWorkspace={() => {}}
        agentContext={agentContext}
      />
      <div data-testid="pill">
        <ScopedStatusMark scope={{ teamKey: TEAM, wsIds: ["AID-4"] }} />
      </div>
      <div data-testid="tab">
        <SessionStatusMark sessionId={SESSION} />
      </div>
    </>,
  );
  const nav = screen.getByTestId("nav-story-agent-mark").getAttribute("data-mark");
  const epic = screen.getByTestId("nav-epic-agent-mark").getAttribute("data-mark");
  const pill = within(screen.getByTestId("pill")).getByRole("img").getAttribute("data-mark");
  const tab = within(screen.getByTestId("tab")).getByRole("img").getAttribute("data-mark");
  const rows = useAgentListProjectionStore.getState().sessions ?? [];
  const statusOf = renderHook(() => useAgentStatusOf(rows)).result.current;
  const list = markOfAgentStatus(statusOf(rows[0]));
  const kind = selectNeedsYouKind(getNeedsYouState(), SESSION);
  const notification = kind ? markOfNeedsYouKind(kind) : null;
  return { nav, epic, pill, tab, list, notification };
}

describe("left nav agent marks equal every other surface's", () => {
  beforeEach(() => {
    resetSessionProjection();
    useViewerStore.setState({ workspaces: {}, aiStatusBySession: {}, progressTickBySession: {} });
  });

  for (const scope of ["Team (everything)", "an agent's context"] as const) {
    const ctx = scope === "Team (everything)" ? undefined : AGENT_CONTEXT;

    it(`needs you: one mark on the row, the epic, the pill, the tab, the list and the alert (${scope})`, () => {
      seedStoryWorkspace("paused", "permission");
      const m = marksEverywhere(ctx);
      // The paused projection is the approval-specific permission mark.
      expect(m).toEqual({
        nav: "permission",
        epic: "permission",
        pill: "permission",
        tab: "permission",
        list: "permission",
        notification: "permission",
      });
    });

    it(`working: the same Working mark everywhere, and no alert (${scope})`, () => {
      seedStoryWorkspace("thinking");
      const m = marksEverywhere(ctx);
      // Generic progress proves activity, but does not prove an open tool.
      expect(m).toEqual({
        nav: "thinking",
        epic: "thinking",
        pill: "thinking",
        tab: "thinking",
        list: "thinking",
        notification: null,
      });
    });

    it(`error: the same Error mark everywhere, alert included (${scope})`, () => {
      seedStoryWorkspace("failed", "failed");
      const m = marksEverywhere(ctx);
      expect(m).toEqual({
        nav: "error",
        epic: "error",
        pill: "error",
        tab: "error",
        list: "error",
        notification: "error",
      });
    });
  }

  it("a story with no workspace draws no mark and keeps no gap", () => {
    useViewerStore.setState({ statusNavExpandedPaths: { [TEAM]: ["/ctx/26-inbox"] } });
    render(
      <PipelineStatusNav
        tree={tree()}
        repoPath="/ctx"
        rootPath="/ctx"
        selectedPath={null}
        onFileSelect={() => {}}
      />,
    );
    expect(screen.getByTestId("nav-story-title")).toBeInTheDocument();
    expect(screen.queryByTestId("nav-story-agent-mark")).toBeNull();
    expect(screen.queryByTestId("nav-epic-agent-mark")).toBeNull();
  });
});

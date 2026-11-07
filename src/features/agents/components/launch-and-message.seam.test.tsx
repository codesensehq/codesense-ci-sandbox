// SPDX-License-Identifier: MIT
import {
  AGENT_RUNTIMES,
  isAgentRuntime,
  PREVIEW_RUNTIME_NOTE,
  registeredCli,
} from "@/shared/lib/agent-runtimes";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { seedAgentListFixture } from "@/features/viewer/lib/agent-list-test-fixture";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pickOption } from "./pick-option";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockAgents } from "@/app/mock/handlers/agents";
import {
  agentTeamInboxHandlers,
  agentTeamSessions,
  agentTeamTabs,
} from "@/app/mock/handlers/agents-team";
import { mockOrg, TEAM_CONTENT_PATH } from "@/app/mock/fixtures";
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { CONTEXT_INDEX_CHANGED_EVENT } from "@/tauri/events";
import type { ContextQuery, ContextQueryResult } from "@/shared/lib/context-query/types";
import { teamFoldKey, workspaceFoldKey } from "@/shared/lib/workspace-key";
import {
  __resetMachineLoadsForTest,
  useMachinesStore,
} from "@/features/machines/stores/machines-store";

import { openLaunchSheet, useLaunchSheetStore } from "../lib/launch-sheet";
import { teamSessionLaunchRequest } from "../lib/reopen-team-launch";
import { resetAgentsStoreForTests, useAgentsStore, watchAgentList } from "../stores/agents-store";
import { AgentLaunchSheet } from "./agent-launch-sheet";
import { AgentDetailPane } from "./agent-detail";

/**
 * The launch sheet and an agent's Messages over the mock backend's team world, through the
 * same `window.__TAURI_INTERNALS__` door the app uses: the launch and the role send are real
 * command round-trips, recorded at the router.
 */

const { goTo, pipeline, branchLaunches, gate, openTabs } = vi.hoisted(() => ({
  /** The management tabs the Agents navigation asked to open. */
  openTabs: [] as string[],
  /** While set, a branch launch waits on it before it reports its session. */
  gate: { hold: null as Promise<void> | null },
  goTo: [] as string[],
  /** What the branch-or-worktree launch dispatched through the open-workspace controller. */
  branchLaunches: [] as Array<{ launcher: string; arg: unknown }>,
  /** The pipeline the sheet's story box reads, and the story files it lists, by story id. */
  pipeline: {
    tree: null as unknown,
    files: {} as Record<string, string>,
    repoPaths: [] as string[],
  },
}));
vi.mock("@/features/dashboard/components/launch-open-real", () => ({
  makeRealLaunchOpen:
    (launcher: string) => async (arg: unknown, hooks: { onHydrated: () => void }) => {
      branchLaunches.push({ launcher, arg });
      if (gate.hold) await gate.hold;
      hooks.onHydrated();
      return {
        sessionId: "sess-1",
        warnings: [
          {
            code: "command_not_in_manifest",
            message:
              "this agent doesn't start with /nope — add it to the agent or the run won't find it",
          },
        ],
      };
    },
  resolveNewBranchBase: async (_launcher: string, repo: string) =>
    repo === "acme/api" ? "trunk" : null,
}));
vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: (path: string) => openTabs.push(path),
}));
vi.mock("@/shared/lib/pipeline-graph/use-pipeline-tree", () => ({
  usePipelineTree: (repoPath: string) => {
    pipeline.repoPaths.push(repoPath);
    return { tree: pipeline.tree };
  },
}));
vi.mock("@/shared/lib/pipeline-graph/resolve-story-file-path", () => ({
  storyFilePathFromTree: (_tree: unknown, storyId: string) => pipeline.files[storyId] ?? null,
}));
vi.mock("@/features/weft/lib/locate-agent", () => ({
  locateAgentByAgentId: (handle: string) => {
    goTo.push(handle);
    return { located: true };
  },
}));

const TEAM = "acme/widgets:default";
const MANAGER = `${TEAM_CONTENT_PATH}#manager`;
const TRIAGE = "acme/backend/triage";
const UNQUALIFIED = {
  code: "cli_version_unqualified",
  message:
    "This CLI version (2.0.1) isn't supported yet (supported: OpenCode 1.18 or a newer 1.x release).",
};

let world: ReturnType<typeof createMockAgents>;
let calls: Array<{ cmd: string; args: Record<string, unknown> }>;
let stopDefinitions: (() => void) | undefined;
/** When set, the launch answers with this refusal, as the backend's spawn cap does. */
let refusal: string | null;
/** The team's machines `list_machines` answers with. */
let machines: unknown[];
/** The warnings the mock `agent_preview` adds to what it resolves, by agent key. */
let previewWarnings: Record<string, Array<{ code: string; message: string }>>;
let previewCwdWarnings: typeof previewWarnings;
let contextWorktrees: unknown[];
let storyQuery: (query?: ContextQuery) => Promise<ContextQueryResult>;
let storyEvents: MockEventBus;

function storyResult(
  rows: ContextQueryResult["rows"] = [],
  patch: Partial<ContextQueryResult> = {},
): ContextQueryResult {
  return {
    schemaVersion: 1,
    rows,
    total: rows.length,
    indexing: false,
    generation: 1,
    textScanTruncated: false,
    totalApproximate: false,
    ...patch,
  };
}

function machine(machineId: string, state: string): unknown {
  return {
    machineId,
    kind: "ssh",
    label: machineId.toUpperCase(),
    scope: { kind: "personal" },
    declaration: { host: `${machineId}.int`, port: 22, mode: "private-account" },
    binding: { hidden: false },
    canPublish: true,
    status: { state, workspaces: 0, lastError: "refused" },
    attention: 0,
    capabilities: { agentCallbacks: true },
  };
}

beforeEach(() => {
  calls = [];
  refusal = null;
  machines = [];
  previewWarnings = {};
  previewCwdWarnings = {};
  contextWorktrees = [];
  useWorktreeRosterStore.setState({ rows: null, error: null });
  storyQuery = async () => storyResult();
  __resetMachineLoadsForTest();
  useMachinesStore.setState({ byTeam: {}, failedTeams: [] });
  goTo.length = 0;
  branchLaunches.length = 0;
  gate.hold = null;
  openTabs.length = 0;
  pipeline.tree = null;
  pipeline.files = {};
  pipeline.repoPaths = [];
  resetAgentsStoreForTests();
  useLaunchSheetStore.setState({ request: null });
  useOrgStore.setState({
    orgs: [mockOrg],
    discoveryComplete: true,
    activeOrgSlug: "acme/widgets",
    activeTeamName: "default",
  });
  const events = new MockEventBus();
  storyEvents = events;
  const sessions = agentTeamSessions("?agents=team");
  world = createMockAgents(events, { sessions });
  world.editAs(TRIAGE, { role: "triage" }, { kind: "user", name: "Alice" });
  const publishOwners = () => {
    const tabs = agentTeamTabs("?agents=team");
    seedAgentListFixture(
      sessions.map((row) => ({
        ...row,
        rowNames: {
          primary: row.displayName ?? row.handle,
          secondary: null,
          full: row.displayName ?? row.handle,
        },
      })),
      new Map(
        sessions.map((row) => [
          row.sessionId ?? row.handle,
          {
            teamKey: TEAM,
            workspaceId:
              useViewerStore
                .getState()
                .workspaces[TEAM]?.find((ws) => !!row.story && ws.storyId === row.story)?.id ??
              tabs.find((tab) => tab.session === row.sessionId)?.workspaceId ??
              row.story ??
              null,
            parentHandle: row.launchedBy,
          },
        ]),
      ),
    );
  };
  const handlers = {
    ...events.handlers(),
    ...world.handlers,
    ...agentTeamInboxHandlers((e) => events.emit(e, null), { sessions }),
    local_profile_status: () => ({ active: null }),
    agent_list_refresh: () => {
      publishOwners();
    },
    roles_list: () => [
      { id: "triage", name: "Triage", icon: "builtin-chess:knight", defaultHandler: null },
      { id: "qa", name: "Quality", icon: "builtin-chess:rook", defaultHandler: null },
    ],
    list_machines: () => machines,
    list_managed_worktrees: () => ({ rows: contextWorktrees, reaped: 0 }),
    query_context: (args: Record<string, unknown>) => storyQuery(args.query as ContextQuery),
  };
  const recorded = Object.fromEntries(
    Object.entries(handlers).map(([cmd, fn]) => [
      cmd,
      (args: Record<string, unknown>) => {
        calls.push({ cmd, args });
        if (cmd === "agent_launch_open" && refusal) {
          return { status: "busy", handle: null, name: null, message: refusal };
        }
        const reply = (fn as (a: Record<string, unknown>) => unknown)(args);
        const form =
          cmd === "agent_preview" ? (args.form as { key?: string; runtime?: string }) : null;
        // An agent on a runtime out of range warns, whatever key a test set warnings for.
        const warned =
          form &&
          (form.runtime === "opencode"
            ? [UNQUALIFIED]
            : (previewCwdWarnings[String(args.cwd)] ?? previewWarnings[form.key ?? ""]));
        if (warned) {
          const r = reply as { resolved: { warnings: unknown[] } };
          return { ...r, resolved: { ...r.resolved, warnings: warned } };
        }
        return reply;
      },
    ]),
  );
  const internals = buildTauriInternals(createInvokeRouter(recorded));
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  publishOwners();
});

afterEach(() => {
  cleanup();
  stopDefinitions?.();
  stopDefinitions = undefined;
  resetAgentsStoreForTests();
});

const launchCalls = () => calls.filter((c) => c.cmd === "agent_launch_open").map((c) => c.args);

function renderSheet(request: Parameters<typeof openLaunchSheet>[0]) {
  const onClose = vi.fn();
  render(
    <TooltipProvider>
      <AgentLaunchSheet request={request} onClose={onClose} />
    </TooltipProvider>,
  );
  return { onClose, sheet: within(screen.getByTestId("agent-launch-sheet")) };
}

describe("launch sheet", () => {
  it("shows the selected worktree's root-document warning before Launch", async () => {
    const cwd = "/worktrees/preview/default/content";
    contextWorktrees = [
      {
        kind: "context",
        id: "preview",
        name: "Preview",
        orgSlug: "acme/widgets",
        team: "default",
        branch: "default/preview",
        path: "/worktrees/preview",
        launchCwd: cwd,
        lastUsedAt: 1,
        agents: [],
      },
    ];
    previewWarnings = {
      [TRIAGE]: [
        {
          code: "shadowing_root_document",
          message: ".hermes.md takes precedence over AGENTS.md; team instructions may be shadowed",
        },
      ],
    };
    const { sheet } = renderSheet({
      teamKey: TEAM,
      agentKey: TRIAGE,
      contextWorktreeId: "preview",
    });
    await waitFor(() =>
      expect(calls.some((call) => call.cmd === "agent_preview" && call.args.cwd === cwd)).toBe(
        true,
      ),
    );
    const shown = await sheet.findByTestId("launch-warnings");
    expect(shown.textContent).toContain(".hermes.md");
    expect(launchCalls()).toHaveLength(0);
  });

  it("shows Hermes starters in the Launch sheet picker exactly when its runtime is listed", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await waitFor(() => expect(sheet.getByTestId("launch-agent")).toHaveTextContent("Triage"));
    fireEvent.pointerDown(sheet.getByTestId("launch-agent"), { button: 0, ctrlKey: false });
    const choices = await screen.findAllByRole("menuitem");
    expect(choices.some((choice) => choice.textContent?.includes("Hermes"))).toBe(
      isAgentRuntime("hermes"),
    );
    expect(launchCalls()).toHaveLength(0);
  });

  it("previews a listed no-login CLI version and launches with a selected queue role", async () => {
    const key = TRIAGE;
    world.editAs(
      key,
      {
        runtime: "opencode",
        args: [],
        permissions: null,
      },
      { kind: "user", name: "Alice" },
    );
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: key });
    const shown = await sheet.findByTestId("launch-warnings");
    expect(shown).toHaveTextContent("OpenCode");
    expect(shown).toHaveTextContent("2.0.1");
    expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage");
    expect(sheet.getByTestId("launch-account")).toHaveTextContent("No CodeSense login");
    expect(sheet.getByTestId("launch-account")).not.toHaveTextContent("Charges");
    expect(sheet.getByTestId("launch-account")).not.toHaveTextContent("Change it on Overview");
    fireEvent.change(sheet.getByTestId("launch-task"), { target: { value: "literal task" } });
    await waitFor(() => expect(sheet.getByTestId("launch-submit")).not.toBeDisabled());
    await pickOption(sheet, "Other queue role", "Quality");
    fireEvent.keyDown(sheet.getByTestId("launch-task"), { key: "Enter", metaKey: true });
    await waitFor(() => expect(launchCalls()).toHaveLength(1));
    expect(launchCalls()[0].args).toMatchObject({ agent: key, queueRole: "qa" });
  });

  it("shows the unqualified-CLI warning from the agent's preview before Launch is pressed", async () => {
    const warning = {
      code: "cli_version_unqualified",
      message:
        "This CLI version (2.0.1) isn't supported yet (supported: OpenCode 1.18 or a newer 1.x release). Status, inbox wake and usage may not work.",
    };
    previewWarnings = {
      [TRIAGE]: [warning, { code: "no_mcp_servers_offered", message: "editor advice" }],
    };
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    const shown = await sheet.findByTestId("launch-warnings");
    expect(shown.textContent).toContain(warning.message);
    expect(shown.textContent).not.toContain("editor advice");
    expect(launchCalls()).toHaveLength(0);
  });

  it("drops the warning when another agent is picked, not holding the previous agent's until its own preview lands", async () => {
    previewWarnings = { [TRIAGE]: [UNQUALIFIED] };
    const user = (await import("@testing-library/user-event")).default.setup();
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await sheet.findByTestId("launch-warnings");
    await user.click(sheet.getByTestId("launch-agent"));
    const items = await screen.findAllByRole("menuitem");
    await user.click(items.find((i) => i.querySelector(".font-medium")?.textContent === "Claude")!);
    // The very next paint: the other agent's preview has not landed, and shows nothing.
    expect(sheet.queryByTestId("launch-warnings")).toBeNull();
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    await new Promise((r) => setTimeout(r, 500));
    expect(sheet.queryByTestId("launch-warnings")).toBeNull();
  });

  it("shows no local CLI warning for a launch that runs on another machine", async () => {
    machines = [machine("box", "connected")];
    previewWarnings = { [TRIAGE]: [UNQUALIFIED] };
    const user = (await import("@testing-library/user-event")).default.setup();
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE, storyId: "SW-8" });
    await sheet.findByTestId("launch-warnings");
    await user.click(sheet.getByTestId("launch-machine"));
    await user.click(await screen.findByTestId("launch-machine-box"));
    expect(sheet.queryByTestId("launch-warnings")).toBeNull();
  });

  it("updates the warning when the open sheet's saved agent changes runtime", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await waitFor(() => expect(calls.some((c) => c.cmd === "agent_preview")).toBe(true));
    expect(sheet.queryByTestId("launch-warnings")).toBeNull();
    await act(async () =>
      world.editAs(TRIAGE, { runtime: "opencode" }, { kind: "user", name: "Alice" }),
    );
    await waitFor(() =>
      expect(sheet.getByTestId("launch-warnings").textContent).toContain(UNQUALIFIED.message),
    );
  });

  it("shows no warning block when the preview has none", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await waitFor(() => expect(calls.some((c) => c.cmd === "agent_preview")).toBe(true));
    expect(sheet.queryByTestId("launch-warnings")).toBeNull();
  });

  it("opened under a manager: the manager is prefilled, the saved role default is selected, the name follows the agent", async () => {
    const { sheet } = renderSheet({
      teamKey: TEAM,
      managerHandle: MANAGER,
    });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    expect(sheet.getByTestId("launch-manager").textContent).toContain("Manager");
    expect(sheet.getByRole("radio", { name: "Saved default (none)" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(sheet.getByTestId<HTMLInputElement>("launch-name").value).toBe("Claude");
    // The task box has the focus: what to ask comes first.
    expect(document.activeElement).toBe(sheet.getByTestId("launch-task"));
    // The preview draws it under its manager.
    expect(sheet.getByTestId("launch-preview").textContent).toContain("Manager");
  });

  it("launches with everything it was given: target, name, agent, manager, role, face, task", async () => {
    const { sheet, onClose } = renderSheet({
      teamKey: TEAM,
      agentKey: TRIAGE,
      managerHandle: "BUG-151#triage",
    });
    // A manager working on a story puts the new agent in that story.
    await waitFor(() =>
      expect(sheet.getByTestId<HTMLInputElement>("launch-story").value).toBe("BUG-151"),
    );
    fireEvent.change(sheet.getByTestId("launch-task"), {
      target: { value: "Check the digest replay." },
    });
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Replay checker" } });
    await pickOption(sheet, "Other queue role", "Quality");
    const face = sheet
      .getAllByRole("radio")
      .find((r) => r.dataset.testid?.startsWith("launch-face-builtin"));
    fireEvent.click(face!);
    fireEvent.keyDown(sheet.getByTestId("launch-task"), { key: "Enter", metaKey: true });

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(launchCalls()).toEqual([
      {
        args: {
          target: "story",
          storyId: "BUG-151",
          name: "Replay checker",
          agent: TRIAGE,
          initialPrompt: "Check the digest replay.",
          launchedBy: "BUG-151#triage",
          queueRole: "qa",
          avatar: face!.dataset.testid!.slice("launch-face-".length),
          machineId: "local",
        },
      },
    ]);
    // The new agent joins the roster under its manager.
    await waitFor(() =>
      expect(
        useAgentListProjectionStore
          .getState()
          .sessions?.find((r) => r.handle === "BUG-151#replay-checker")?.launchedBy,
      ).toBe("BUG-151#triage"),
    );
  });

  it("its first prompt is passed to the launch as the agent's starting prompt, a /command too", async () => {
    const { sheet, onClose } = renderSheet({
      teamKey: TEAM,
      agentKey: TRIAGE,
      managerHandle: "BUG-151#triage",
    });
    await waitFor(() =>
      expect(sheet.getByTestId<HTMLInputElement>("launch-story").value).toBe("BUG-151"),
    );
    // The box says what it is and what the agent does with it.
    expect(sheet.getByText("First prompt")).toBeTruthy();
    const box = sheet.getByTestId("launch-task");
    expect(document.getElementById(box.getAttribute("aria-describedby")!)?.textContent).toBe(
      "Passed to the agent as its starting prompt — a /command works too",
    );
    fireEvent.change(box, { target: { value: "/triage-slack BUG-151" } });
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Replay checker" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    // Launched once, with the text as its starting prompt; nothing goes to an inbox and
    // nothing is typed into a terminal.
    expect(launchCalls()).toHaveLength(1);
    expect(launchCalls()[0]).toMatchObject({
      args: { initialPrompt: "/triage-slack BUG-151" },
    });
    expect(
      calls.some((c) =>
        /send_to_session|terminal_write|inject|send_message|inbox_post|agent_first_message/.test(
          c.cmd,
        ),
      ),
    ).toBe(false);
  });

  it("shows what is missing inline and launches nothing until it is fixed", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    fireEvent.click(sheet.getByTestId("launch-where-story"));
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "  " } });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    expect(sheet.getByText("Pick the story it works on.")).toBeTruthy();
    expect(sheet.getByText(/it becomes the tab's name/)).toBeTruthy();
    expect(launchCalls()).toEqual([]);

    // A name already running there is caught before the launch.
    fireEvent.change(sheet.getByTestId("launch-story"), { target: { value: "BUG-151" } });
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Spec" } });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    expect(sheet.getByText(/already running there/)).toBeTruthy();
    expect(launchCalls()).toEqual([]);
  });

  it("surfaces a refusal from the backend in the sheet and stays open", async () => {
    refusal = "Docs was not opened: too many agent sessions are running.";
    const { sheet, onClose } = renderSheet({ teamKey: TEAM });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Docs" } });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    expect((await sheet.findByTestId("launch-error")).textContent).toContain(
      "too many agent sessions",
    );
    expect(launchCalls()).toHaveLength(1);
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe("launch sheet: who is being launched", () => {
  it("shows the selected agent's face and name in the header, and follows the Agent picker", async () => {
    const user = (await import("@testing-library/user-event")).default.setup();
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    const header = sheet.getByTestId("launch-header");
    await waitFor(() =>
      expect(within(header).getByRole("heading").textContent).toBe("Launch Triage"),
    );
    // The face is the saved agent's, as the list and the profile header draw it.
    expect(
      within(header).getByTestId("launch-header-face").querySelector("[data-face]"),
    ).toBeTruthy();
    expect(header.textContent).toContain("Sweeps the support channel");
    const faceBefore = within(header)
      .getByTestId("launch-header-face")
      .querySelector("[data-face]")!
      .getAttribute("data-face");
    await user.click(sheet.getByTestId("launch-agent"));
    await user.click(await screen.findByRole("menuitem", { name: /^Reviewer/ }));
    await waitFor(() =>
      expect(within(header).getByRole("heading").textContent).toBe("Launch Reviewer"),
    );
    const faceAfter = within(header)
      .getByTestId("launch-header-face")
      .querySelector("[data-face]")!
      .getAttribute("data-face");
    expect(faceAfter).not.toBe(faceBefore);
  });
});

describe("launch sheet: one launch window", () => {
  it("says which account pays, read-only, with no Fix when it can launch", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-account").textContent).toContain("claude-work"),
    );
    expect(sheet.getByTestId("launch-account").textContent).toContain("Charges");
    expect(sheet.getByTestId("launch-account").textContent).toContain("Change it on Overview");
    expect(sheet.queryByRole("button", { name: /^Fix the account/ })).toBeNull();
  });

  it("offers Fix when the account can't launch: it closes the sheet and opens the agent", async () => {
    const SCRIBE = "acme/backend/scribe";
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, agentKey: SCRIBE });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-account").textContent).toContain("detached"),
    );
    fireEvent.click(sheet.getByRole("button", { name: "Fix the account for Scribe" }));
    expect(onClose).toHaveBeenCalled();
    // The Agents tab itself opens on the agent, and its account picker is asked for.
    expect(openTabs).toContain("virtual://agents");
    await waitFor(() => expect(useAgentsStore.getState().selectedKey).toBe(SCRIBE));
    expect(useAgentsStore.getState().tabView).toBe("agents");
    expect(useAgentsStore.getState().request).toMatchObject({
      kind: "account-picker",
      agentKey: SCRIBE,
    });
  });

  it("doesn't block a launch on this Mac's account preview: the destination decides", async () => {
    const SCRIBE = "acme/backend/scribe";
    const { sheet, onClose } = renderSheet({
      teamKey: TEAM,
      agentKey: SCRIBE,
      managerHandle: "BUG-151#triage",
    });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-account").textContent).toContain("detached"),
    );
    await waitFor(() =>
      expect(sheet.getByTestId<HTMLInputElement>("launch-story").value).toBe("BUG-151"),
    );
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(launchCalls()).toHaveLength(1);
    expect(launchCalls()[0]).toMatchObject({ args: { agent: SCRIBE } });
  });

  it("Branch or worktree: a new branch from the repo's base, an existing branch, an existing worktree", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage"));
    // Where and Target are the kit's segmented control, not a one-off.
    expect(
      sheet.getByTestId("launch-where-branch").closest('[data-slot="segmented"]'),
    ).toBeTruthy();
    expect(sheet.getByText(/wearing the face you pick/)).toBeTruthy();
    fireEvent.click(sheet.getByTestId("launch-where-branch"));
    expect(
      sheet.getByTestId("launch-target-kind-newWorktree").closest('[data-slot="segmented"]'),
    ).toBeTruthy();
    // The subtitle stops promising a manager and a face, which a branch launch doesn't take.
    expect(sheet.queryByText(/wearing the face you pick/)).toBeNull();
    expect(sheet.getAllByText(/in a workspace of its own/).length).toBeGreaterThan(0);
    // A branch launch opens a workspace of its own: no manager, name or face to pick.
    expect(sheet.queryByTestId("launch-name")).toBeNull();
    expect(sheet.queryByTestId("launch-manager")).toBeNull();

    fireEvent.change(sheet.getByTestId("launch-task"), { target: { value: "/nope go" } });
    fireEvent.click(sheet.getByTestId("launch-target-kind-newWorktree"));
    expect(sheet.getByTestId("launch-new-branch-base").textContent).toBe(
      "from the repo's local default branch — nothing is fetched",
    );
    fireEvent.change(sheet.getByTestId("launch-field-repo"), { target: { value: "acme/api" } });
    fireEvent.change(sheet.getByTestId("launch-field-branch"), {
      target: { value: "feat/fresh" },
    });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-new-branch-base").textContent).toBe(
        "from trunk — nothing is fetched",
      ),
    );
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(branchLaunches).toHaveLength(1));
    expect(branchLaunches[0]).toEqual({
      launcher: "virtual://agents",
      arg: {
        target: {
          kind: "branch",
          repo: "acme/api",
          branch: "feat/fresh",
          agent: TRIAGE,
          checkout: "new",
        },
        headed: true,
        reviewPrompt: "/nope go",
      },
    });
    await waitFor(() => expect(sheet.getByTestId("launch-progress").dataset.phase).toBe("running"));
    // The launch's warnings show once it started; they never stop it.
    expect(sheet.getByTestId("launch-run-warnings").textContent).toContain(
      "this agent doesn't start with /nope",
    );

    fireEvent.click(sheet.getByTestId("launch-target-kind-repoBranch"));
    expect(sheet.queryByTestId("launch-new-branch-base")).toBeNull();
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(branchLaunches).toHaveLength(2));
    expect(branchLaunches[1].arg).toMatchObject({
      target: { kind: "branch", checkout: "existing" },
    });

    fireEvent.click(sheet.getByTestId("launch-target-kind-existingWorktree"));
    fireEvent.change(sheet.getByTestId("launch-field-path"), {
      target: { value: "/Users/me/wt/feat" },
    });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(branchLaunches).toHaveLength(3));
    expect(branchLaunches[2].arg).toEqual({
      target: { kind: "dir", path: "/Users/me/wt/feat", agent: TRIAGE },
      headed: true,
      initialPrompt: "/nope go",
    });
  });

  it.each([
    { mode: "existingWorktree", remoteStory: false, cwd: "/Users/me/wt/feat" },
    { mode: "existingWorktree", remoteStory: true, cwd: "/Users/me/wt/feat" },
    { mode: "newWorktree", remoteStory: false, cwd: undefined },
    { mode: "repoBranch", remoteStory: false, cwd: undefined },
    { mode: "story", remoteStory: false, cwd: "/worktrees/SW-7" },
  ])(
    "previews the active $mode target (hidden remote story: $remoteStory)",
    async ({ mode, remoteStory, cwd }) => {
      const storyCwd = "/worktrees/SW-7";
      machines = [machine("box", "connected")];
      useViewerStore.setState({
        workspaces: {
          [TEAM]: [
            {
              id: "SW-7-fix",
              kind: "story",
              storyId: "SW-7",
              cwd: storyCwd,
              ...(remoteStory
                ? {
                    location: {
                      kind: "machine",
                      machineId: "box",
                      requestId: "r",
                      handle: "h",
                      cwd: "/remote/SW-7",
                    },
                  }
                : {}),
            } as never,
          ],
        },
      });
      for (const directory of [storyCwd, "/Users/me/wt/feat"]) {
        previewCwdWarnings[directory] = [
          {
            code: "shadowing_root_document",
            message: `${directory}/HERMES.md takes precedence over AGENTS.md`,
          },
        ];
      }
      const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE, storyId: "SW-7" });
      await waitFor(() =>
        expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage"),
      );
      if (mode !== "story") {
        fireEvent.click(sheet.getByTestId("launch-where-branch"));
        fireEvent.click(sheet.getByTestId(`launch-target-kind-${mode}`));
        if (mode === "existingWorktree") {
          fireEvent.change(sheet.getByTestId("launch-field-path"), {
            target: { value: "  /Users/me/wt/feat  " },
          });
        } else {
          fireEvent.change(sheet.getByTestId("launch-field-repo"), {
            target: { value: "acme/api" },
          });
          fireEvent.change(sheet.getByTestId("launch-field-branch"), {
            target: { value: "feat/fresh" },
          });
        }
      }
      // Only requests after the picker settles count; the hidden target may have previewed earlier.
      if (mode !== "story") calls.length = 0;
      await waitFor(() => {
        const previews = calls.filter((call) => call.cmd === "agent_preview");
        expect(previews.length).toBeGreaterThan(0);
        expect(previews.at(-1)?.args.cwd).toBe(cwd);
      });
      if (cwd) {
        await waitFor(() =>
          expect(sheet.getByTestId("launch-warnings").textContent).toContain(`${cwd}/HERMES.md`),
        );
      } else {
        expect(sheet.queryByTestId("launch-warnings")).toBeNull();
      }
      expect(launchCalls()).toHaveLength(0);
      expect(branchLaunches).toHaveLength(0);
    },
  );

  it("a branch launch in flight can't be reset, repeated or dismissed from the sheet", async () => {
    let release!: () => void;
    gate.hold = new Promise<void>((r) => (release = r));
    const SCRIBE = "acme/backend/scribe";
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, agentKey: SCRIBE });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-account").textContent).toContain("detached"),
    );
    fireEvent.click(sheet.getByTestId("launch-where-branch"));
    fireEvent.click(sheet.getByTestId("launch-target-kind-existingWorktree"));
    fireEvent.change(sheet.getByTestId("launch-field-path"), {
      target: { value: "/Users/me/wt/feat" },
    });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(branchLaunches).toHaveLength(1));
    // In flight: Where, Cancel, Fix and the launch itself are all held.
    for (const id of ["launch-where-story", "launch-where-team-workspace", "launch-where-branch"])
      expect(sheet.getByTestId(id)).toBeDisabled();
    expect(sheet.getByRole("button", { name: "Cancel" })).toBeDisabled();
    expect(sheet.getByRole("button", { name: "Fix the account for Scribe" })).toBeDisabled();
    fireEvent.click(sheet.getByTestId("launch-where-branch"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    fireEvent.keyDown(sheet.getByTestId("launch-task"), { key: "Enter", metaKey: true });
    fireEvent.keyDown(sheet.getByTestId("launch-task"), { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(branchLaunches).toHaveLength(1);
    release();
    await waitFor(() => expect(sheet.getByTestId("launch-progress").dataset.phase).toBe("running"));
    // Done: the controls come back.
    expect(sheet.getByRole("button", { name: "Cancel" })).toBeEnabled();
    expect(sheet.getByTestId("launch-where-story")).toBeEnabled();
  });

  it("launches nothing until the branch and repo are given", async () => {
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage"));
    fireEvent.click(sheet.getByTestId("launch-where-branch"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    expect(branchLaunches).toHaveLength(0);
    expect(sheet.getByText(/Fix the highlighted fields/)).toBeTruthy();
  });
});

describe("launch sheet: the machine", () => {
  afterEach(() => {
    useViewerStore.setState({ workspaces: {}, softClosedWorkspaces: {} });
  });

  it("a story whose workspace lives on a machine holds the Machine there and launches there", async () => {
    machines = [machine("box", "connected")];
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          {
            id: "SW-7-fix",
            kind: "story",
            storyId: "SW-7",
            cwd: "",
            location: { kind: "machine", machineId: "box", requestId: "r", handle: "h", cwd: "/w" },
          } as never,
        ],
      },
    });
    const { sheet, onClose } = renderSheet({
      teamKey: TEAM,
      storyId: "SW-7",
    });
    await waitFor(() => expect(sheet.getByTestId("launch-machine").textContent).toContain("BOX"));
    expect(sheet.getByTestId("launch-machine")).toBeDisabled();
    expect(sheet.getByTestId("launch-machine-note").textContent).toContain(
      "SW-7's workspace lives on BOX, so its agents run there.",
    );
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(launchCalls()[0]).toEqual({
      args: expect.objectContaining({ target: "story", storyId: "SW-7", machineId: "box" }),
    });
  });

  it("a story with no workspace yet picks where it is created; an unreachable machine is disabled", async () => {
    machines = [machine("box", "connected"), machine("down", "connectionFailed")];
    const user = (await import("@testing-library/user-event")).default.setup();
    const { sheet, onClose } = renderSheet({
      teamKey: TEAM,
      storyId: "SW-8",
      task: "Read the story and complete it.",
    });
    expect(sheet.getByTestId<HTMLTextAreaElement>("launch-task").value).toBe(
      "Read the story and complete it.",
    );
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    expect(sheet.getByTestId("launch-machine")).not.toBeDisabled();
    await user.click(sheet.getByTestId("launch-machine"));
    const down = await screen.findByTestId("launch-machine-down");
    expect(down).toHaveAttribute("data-disabled");
    expect(down.textContent).toContain("DOWN is unreachable");
    await user.click(screen.getByTestId("launch-machine-box"));
    expect(sheet.getByTestId("launch-machine-note").textContent).toContain(
      "Creates the story's workspace on BOX",
    );
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(launchCalls()[0]).toEqual({
      args: expect.objectContaining({
        storyId: "SW-8",
        machineId: "box",
        initialPrompt: "Read the story and complete it.",
      }),
    });
  });

  it("on a remote machine the line says whose login it uses, not this Mac's account, and offers no Fix", async () => {
    machines = [machine("box", "connected")];
    const user = (await import("@testing-library/user-event")).default.setup();
    const SCRIBE = "acme/backend/scribe";
    const { sheet } = renderSheet({ teamKey: TEAM, agentKey: SCRIBE, storyId: "SW-8" });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-account").textContent).toContain("detached"),
    );
    expect(sheet.getByRole("button", { name: "Fix the account for Scribe" })).toBeTruthy();
    await user.click(sheet.getByTestId("launch-machine"));
    await user.click(await screen.findByTestId("launch-machine-box"));
    const line = sheet.getByTestId("launch-account");
    expect(line.textContent).toContain("Uses the Claude login on BOX");
    expect(line.textContent).not.toContain("Charges");
    expect(line.textContent).not.toContain("detached");
    expect(sheet.queryByRole("button", { name: /^Fix the account/ })).toBeNull();
    // Back on this Mac: the account line is this Mac's again.
    await user.click(sheet.getByTestId("launch-machine"));
    await user.click(await screen.findByTestId("launch-machine-local"));
    expect(sheet.getByTestId("launch-account").textContent).toContain("detached");
  });

  it("after a launch, the new agent's team and its story's card unfold in the Agents pane", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: "SW-7-fix", kind: "story", storyId: "SW-7", cwd: "/w" } as never],
      },
      agentsPaneFolds: {
        [teamFoldKey(TEAM)]: true,
        [workspaceFoldKey(TEAM, "SW-7-fix")]: true,
        [workspaceFoldKey(TEAM, "other")]: true,
      },
    });
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, storyId: "SW-7" });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    // Nothing unfolds before the launch.
    expect(useViewerStore.getState().agentsPaneFolds[teamFoldKey(TEAM)]).toBe(true);
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    // The pane is revealed once the roster lists the new agent.
    await waitFor(() =>
      expect(useViewerStore.getState().agentsPaneFolds[teamFoldKey(TEAM)]).toBeUndefined(),
    );
    const folds = useViewerStore.getState().agentsPaneFolds;
    expect(folds[workspaceFoldKey(TEAM, "SW-7-fix")]).toBeUndefined();
    // Another card keeps its fold.
    expect(folds[workspaceFoldKey(TEAM, "other")]).toBe(true);
    useViewerStore.setState({ agentsPaneFolds: {} });
  });

  it("a team-folder launch shows its hidden team and unfolds its no-workspace card, keeping the search and Show filter", async () => {
    useViewerStore.setState({
      agentsPaneHiddenTeams: [TEAM, "acme/widgets:other"],
      agentsPaneSearch: { query: "claude", status: "needs" },
      agentsPaneFolds: {
        [teamFoldKey(TEAM)]: true,
        [workspaceFoldKey(TEAM, "")]: true,
        [workspaceFoldKey(TEAM, "other")]: true,
      },
    });
    const { sheet, onClose } = renderSheet({ teamKey: TEAM });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Docs" } });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    await waitFor(() =>
      expect(useViewerStore.getState().agentsPaneHiddenTeams).toEqual(["acme/widgets:other"]),
    );
    const state = useViewerStore.getState();
    expect(state.agentsPaneFolds[teamFoldKey(TEAM)]).toBeUndefined();
    expect(state.agentsPaneFolds[workspaceFoldKey(TEAM, "")]).toBeUndefined();
    expect(state.agentsPaneFolds[workspaceFoldKey(TEAM, "other")]).toBe(true);
    expect(state.agentsPaneSearch).toEqual({ query: "claude", status: "needs" });
    useViewerStore.setState({
      agentsPaneHiddenTeams: [],
      agentsPaneSearch: { query: "", status: "all" },
      agentsPaneFolds: {},
    });
  });

  it("a launch clears an agent chip naming another agent and an arrow on another card, keeping the search", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: "SW-7-fix", kind: "story", storyId: "SW-7", cwd: "/w" } as never],
      },
      agentsListFilter: { agentKey: "acme/backend/other", name: "Other" },
      agentsWorkspaceFilter: { teamKey: TEAM, workspaceId: "elsewhere" },
      agentsPaneSearch: { query: "fix", status: "working" },
    });
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE, storyId: "SW-7" });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    await waitFor(() => expect(useViewerStore.getState().agentsListFilter).toBeNull());
    const state = useViewerStore.getState();
    expect(state.agentsWorkspaceFilter).toBeNull();
    expect(state.agentsPaneSearch).toEqual({ query: "fix", status: "working" });
    useViewerStore.setState({
      workspaces: {},
      agentsPaneSearch: { query: "", status: "all" },
      agentsPaneFolds: {},
    });
  });

  it("a launch keeps an agent chip on its own agent and an arrow on the card it lands in", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: "SW-7-fix", kind: "story", storyId: "SW-7", cwd: "/w" } as never],
      },
      agentsListFilter: { agentKey: TRIAGE, name: "Triage" },
      agentsWorkspaceFilter: { teamKey: TEAM, workspaceId: "SW-7-fix" },
    });
    useViewerStore.setState({ agentsPaneFolds: { [teamFoldKey(TEAM)]: true } });
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE, storyId: "SW-7" });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    await waitFor(() =>
      expect(useViewerStore.getState().agentsPaneFolds[teamFoldKey(TEAM)]).toBeUndefined(),
    );
    const state = useViewerStore.getState();
    expect(state.agentsListFilter).toEqual({ agentKey: TRIAGE, name: "Triage" });
    expect(state.agentsWorkspaceFilter).toEqual({ teamKey: TEAM, workspaceId: "SW-7-fix" });
    useViewerStore.setState({
      workspaces: {},
      agentsListFilter: null,
      agentsWorkspaceFilter: null,
      agentsPaneFolds: {},
    });
  });

  it("a launch into a story with no workspace yet clears an arrow, as its card does not exist yet", async () => {
    useViewerStore.setState({
      agentsWorkspaceFilter: { teamKey: TEAM, workspaceId: "SW-7-fix" },
      agentsPaneFolds: { [workspaceFoldKey(TEAM, "SW-8-new")]: true },
    });
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, storyId: "SW-8" });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    await waitFor(() => expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull());
    const state = useViewerStore.getState();
    // No card's fold is touched: the new one is drawn unfolded with no saved fold.
    expect(state.agentsPaneFolds[workspaceFoldKey(TEAM, "SW-8-new")]).toBe(true);
    useViewerStore.setState({ agentsPaneFolds: {} });
  });

  it("a launch into a pipeline story with no workspace yet unfolds the card it will open as and keeps an arrow on it", async () => {
    pipeline.tree = { stories: [{ storyId: "SW-8", title: "New thing" }], subEpics: [] };
    pipeline.files = { "SW-8": "/stories/SW-8-new.md" };
    useViewerStore.setState({
      agentsWorkspaceFilter: { teamKey: TEAM, workspaceId: "SW-8-new" },
      agentsPaneFolds: { [workspaceFoldKey(TEAM, "SW-8-new")]: true },
    });
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, storyId: "SW-8" });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        useViewerStore.getState().agentsPaneFolds[workspaceFoldKey(TEAM, "SW-8-new")],
      ).toBeUndefined(),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: TEAM,
      workspaceId: "SW-8-new",
    });
    useViewerStore.setState({ agentsWorkspaceFilter: null, agentsPaneFolds: {} });
  });

  it("lists the stories of the sheet's own team, not the active one's", async () => {
    const OTHER = "acme/widgets:other";
    useViewerStore.setState({
      workspaces: {
        [OTHER]: [
          { id: "root", kind: "generic", cwd: "/o", sourceRepoSlug: "acme/other-repo" } as never,
        ],
      },
      activeWorkspace: { [OTHER]: "root" },
    });
    renderSheet({ teamKey: OTHER });
    await waitFor(() => expect(pipeline.repoPaths.length).toBeGreaterThan(0));
    const read = pipeline.repoPaths[pipeline.repoPaths.length - 1];
    expect(read).toContain("acme/other-repo");
    expect(read).toContain("other");
    useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
  });

  it("reveals the launched agent again when it is listed after a newer roster read settled without it", async () => {
    const roster = useAgentListProjectionStore.getState();
    const realReload = roster.reload;
    // The read after the launch is superseded: it settles with the roster as it was (a copy, as
    // the mock world adds the launch to its own list in place).
    useAgentListProjectionStore.setState({
      reload: async () => {},
      sessions: [...(roster.sessions ?? [])],
    });
    try {
      const { sheet, onClose } = renderSheet({ teamKey: TEAM });
      await waitFor(() =>
        expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"),
      );
      fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Late" } });
      fireEvent.click(sheet.getByTestId("launch-submit"));
      await waitFor(() => expect(onClose).toHaveBeenCalled());
      await waitFor(() =>
        expect(useViewerStore.getState().agentsPaneFolds[teamFoldKey(TEAM)]).toBeUndefined(),
      );
      // The person folds the team again; the agent then shows up in the roster.
      act(() => useViewerStore.getState().setAgentsPaneFold(teamFoldKey(TEAM), true));
      await act(() => realReload());
      await waitFor(() =>
        expect(useViewerStore.getState().agentsPaneFolds[teamFoldKey(TEAM)]).toBeUndefined(),
      );
      // Revealed once: a later read leaves the folds alone.
      act(() => useViewerStore.getState().setAgentsPaneFold(teamFoldKey(TEAM), true));
      await act(() => realReload());
      expect(useViewerStore.getState().agentsPaneFolds[teamFoldKey(TEAM)]).toBe(true);
    } finally {
      useAgentListProjectionStore.setState({ reload: realReload });
      useViewerStore.setState({ agentsPaneFolds: {} });
    }
  });

  it("a launch under a manager unfolds the manager's card too", async () => {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          {
            id: "BUG-151-fix",
            kind: "story",
            storyId: "BUG-151",
            storyFilePath: "/stories/BUG-151.md",
            status: "ready",
            storyStatus: "in-progress",
            cwd: "/w",
          } as never,
        ],
      },
      agentsPaneFolds: { [workspaceFoldKey(TEAM, "BUG-151-fix")]: true },
    });
    useAgentListProjectionStore.setState((state) => ({
      references: state.references.map((ref) =>
        state.sessions?.find((row) => row.sessionId === ref.sessionRef)?.handle === "BUG-151#triage"
          ? { ...ref, workspaceRef: "BUG-151-fix", storyRef: "BUG-151-fix" }
          : ref,
      ),
    }));
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, managerHandle: "BUG-151#triage" });
    await waitFor(() =>
      expect(sheet.getByTestId<HTMLInputElement>("launch-story").value).toBe("BUG-151"),
    );
    // Launch it in the team folder: only its manager's card names BUG-151's workspace.
    fireEvent.click(sheet.getByTestId("launch-where-team-workspace"));
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Helper" } });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    await waitFor(() =>
      expect(
        useViewerStore.getState().agentsPaneFolds[workspaceFoldKey(TEAM, "BUG-151-fix")],
      ).toBeUndefined(),
    );
    useViewerStore.setState({ workspaces: {}, agentsPaneFolds: {} });
  });

  it("a team folder runs on This Mac", async () => {
    machines = [machine("box", "connected")];
    const { sheet } = renderSheet({ teamKey: TEAM });
    await waitFor(() => expect(sheet.getByTestId("launch-agent").textContent).toContain("Claude"));
    expect(sheet.getByTestId("launch-machine")).toBeDisabled();
    expect(sheet.getByTestId("launch-machine").textContent).toContain("This Mac");
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Docs" } });
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(launchCalls()).toHaveLength(1));
    expect(launchCalls()[0].args).not.toHaveProperty("machineId");
  });
});

describe("an agent's Running tab", () => {
  async function openTriageRunning() {
    stopDefinitions = watchAgentList();
    await act(async () => {
      await useAgentsStore.getState().loadList();
    });
    act(() => {
      useAgentsStore.getState().select(TRIAGE);
      useAgentsStore.getState().showRunning();
    });
    render(
      <TooltipProvider>
        <AgentDetailPane contentRoot={TEAM_CONTENT_PATH} />
      </TooltipProvider>,
    );
    await waitFor(() => expect(screen.getAllByTestId("agent-instance").length).toBeGreaterThan(0));
    return within(screen.getByTestId("agent-running-view"));
  }

  it("lists Triage's live sessions, and nothing of other agents", async () => {
    const pane = await openTriageRunning();
    const cards = pane.getAllByTestId("agent-instance");
    expect(cards.map((c) => c.dataset.handle).sort()).toEqual([
      "BUG-151#triage",
      "acme/widgets:default::docs-sweep",
    ]);
    expect(pane.queryByText("Draft the fix spec")).toBeNull();
    expect(pane.queryByText(/Retry fix is in review/)).toBeNull();
    // Reading marks nothing read and claims nothing.
    expect(
      calls.some((c) => /mark_inbox_message_read|claim_role|accept_role|ack_role/.test(c.cmd)),
    ).toBe(false);
  });

  it("says where each runs and opens that instance's terminal", async () => {
    const pane = await openTriageRunning();
    const notify = pane
      .getAllByTestId("agent-instance")
      .find((c) => c.dataset.handle === "BUG-151#triage")!;
    expect(notify.textContent).toContain("Notifications triage");
    expect(notify.textContent).toContain("Story BUG-151");
    fireEvent.click(within(notify).getByTestId("agent-instance-open"));
    expect(goTo).toEqual(["BUG-151#triage"]);
  });

  it("counts the live instances on the tab", async () => {
    await openTriageRunning();
    const tab = screen.getByRole("tab", { name: /^Running/ });
    expect(tab.textContent).toContain("2");
  });

  it("offers Launch another, which opens the launch sheet for this agent", async () => {
    const pane = await openTriageRunning();
    fireEvent.click(pane.getByRole("button", { name: "Launch another Triage" }));
    expect(useLaunchSheetStore.getState().request).toMatchObject({ agentKey: TRIAGE });
  });
});

describe("launch sheet: story choices", () => {
  const chooseStories = () => {
    const view = renderSheet({ teamKey: TEAM });
    fireEvent.click(view.sheet.getByTestId("launch-where-story"));
    const input = view.sheet.getByTestId("launch-story");
    fireEvent.focus(input);
    return { ...view, input };
  };
  const indexedStory = (id: string, title: string, source = "acme/widgets") => ({
    id,
    title,
    path: `/stories/${id}.md`,
    relPath: `${id}.md`,
    fileName: `${id}.md`,
    org: "acme/widgets",
    team: "default",
    layer: "team" as const,
    kind: "story" as const,
    mtime: null,
    fields: { story_id: id, source_repo: source, team: "default", type: "story" },
  });
  it("finds a real indexed title when the pipeline scanner does not supply titles", async () => {
    pipeline.tree = {
      stories: [{ storyId: "SV-2", sourceRepo: "acme/widgets", team: "default" }],
      subEpics: [],
    };
    storyQuery = async () => storyResult([indexedStory("SV-2", "Story workspace picker")]);
    const { input } = chooseStories();
    fireEvent.change(input, { target: { value: "workspace picker" } });
    expect(await within(await screen.findByRole("listbox")).findByRole("option")).toHaveTextContent(
      "SV-2",
    );
    fireEvent.change(input, { target: { value: "SV-2 Story" } });
    expect(within(screen.getByRole("listbox")).getByRole("option")).toHaveTextContent("SV-2");
  });
  it("reads all launchable sources for the captured team even without a focused workspace", async () => {
    useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
    const org = {
      ...mockOrg,
      teams: [
        {
          name: "default",
          source_repos: [
            { owner: "acme", repo: "widgets" },
            { owner: "acme", repo: "api" },
          ],
        },
      ],
    };
    useOrgStore.setState({ orgs: [org] });
    storyQuery = async () =>
      storyResult([
        indexedStory("API-1", "API", "acme/api"),
        indexedStory("WEB-1", "Web"),
        indexedStory("HOST-1", "Host-qualified source", "github.com/acme/widgets"),
        indexedStory("GONE-1", "Removed", "acme/gone"),
        indexedStory("NONE-1", "No source", ""),
      ]);
    chooseStories();
    await waitFor(() =>
      expect(within(screen.getByRole("listbox")).getAllByRole("option")).toHaveLength(3),
    );
    expect(screen.getByRole("listbox")).toHaveTextContent("API-1");
    expect(screen.getByRole("listbox")).toHaveTextContent("WEB-1");
    expect(screen.getByRole("listbox")).toHaveTextContent("HOST-1");
    expect(calls.find((c) => c.cmd === "query_context")?.args).toMatchObject({
      query: {
        where: {
          orgs: ["acme/widgets"],
          teams: ["default"],
          kinds: ["story", "bug", "spike"],
          layers: ["team"],
        },
      },
    });
  });
  it("shows the captured team's stories while another team and source workspace are active", async () => {
    const other = "other";
    useOrgStore.setState({
      activeTeamName: other,
      orgs: [
        {
          ...mockOrg,
          teams: [...mockOrg.teams, { name: other, source_repos: mockOrg.teams[0].source_repos }],
        },
      ],
    });
    useViewerStore.setState({
      activeWorkspace: { [TEAM]: "api", [`acme/widgets:${other}`]: "elsewhere" },
      workspaces: {
        [TEAM]: [{ id: "api", kind: "generic", cwd: "/api", sourceRepoSlug: "acme/api" } as never],
        [`acme/widgets:${other}`]: [
          {
            id: "elsewhere",
            kind: "generic",
            cwd: "/elsewhere",
            sourceRepoSlug: "acme/elsewhere",
          } as never,
        ],
      },
    });
    storyQuery = async (query) =>
      storyResult(
        query?.where?.teams?.includes("default")
          ? [indexedStory("CAPTURED-1", "Captured team's story")]
          : [
              {
                ...indexedStory("OTHER-1", "Active team's story"),
                team: other,
                fields: {
                  story_id: "OTHER-1",
                  source_repo: "acme/widgets",
                  team: other,
                  type: "story",
                },
              },
            ],
      );
    const { input } = chooseStories();
    expect(await within(await screen.findByRole("listbox")).findByRole("option")).toHaveTextContent(
      "CAPTURED-1",
    );
    expect(screen.queryByText("OTHER-1")).toBeNull();
    act(() => useViewerStore.setState({ activeWorkspace: {}, workspaces: {} }));
    fireEvent.change(input, { target: { value: "Captured" } });
    expect(within(screen.getByRole("listbox")).getByRole("option")).toHaveTextContent("CAPTURED-1");
    cleanup();
    const reopened = chooseStories();
    fireEvent.change(reopened.input, { target: { value: "Captured" } });
    expect(await within(await screen.findByRole("listbox")).findByRole("option")).toHaveTextContent(
      "CAPTURED-1",
    );
    expect(screen.queryByText("OTHER-1")).toBeNull();
  });
  it("shows loading instead of partial open-workspace choices while indexing", async () => {
    storyQuery = async () => storyResult([indexedStory("PART-1", "Partial")], { indexing: true });
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          {
            id: "open",
            kind: "story",
            storyId: "OPEN-1",
            storyTitle: "Open story",
            cwd: "/w",
          } as never,
        ],
      },
    });
    const { input, onClose } = chooseStories();
    expect(await screen.findByText("Loading stories…")).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();
    storyQuery = async () => storyResult([indexedStory("SV-2", "Picker")], { generation: 2 });
    act(() =>
      storyEvents.emit(CONTEXT_INDEX_CHANGED_EVENT, {
        orgs: ["acme/widgets"],
        allOrgs: false,
        truncated: false,
      }),
    );
    await waitFor(() =>
      expect(within(screen.getByRole("listbox")).getAllByRole("option")).toHaveLength(2),
    );
    expect(screen.getByRole("listbox")).toHaveTextContent("OPEN-1");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    useViewerStore.setState({ workspaces: {} });
  });
  it("waits for all result pages and searches a story from the last page", async () => {
    let finish!: (result: ContextQueryResult) => void;
    storyQuery = async (query) =>
      query?.cursor
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : storyResult([indexedStory("FIRST-1", "First")], { total: 2, nextCursor: "next" });
    const { input } = chooseStories();
    fireEvent.change(input, { target: { value: "last page" } });
    await waitFor(() => expect(finish).toBeDefined());
    expect(screen.getByText("Loading stories…")).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();
    await act(async () => finish(storyResult([indexedStory("LAST-1", "Last page")])));
    expect(await within(await screen.findByRole("listbox")).findByRole("option")).toHaveTextContent(
      "LAST-1",
    );
  });
  it.each(["initial", "later page"])(
    "shows an error when the %s story query fails",
    async (page) => {
      storyQuery = async (query) => {
        if (page === "later page" && !query?.cursor)
          return storyResult([indexedStory("FIRST-1", "First")], { nextCursor: "next" });
        throw new Error("Story index unavailable");
      };
      chooseStories();
      expect(await screen.findByRole("alert")).toHaveTextContent("Story index unavailable");
      expect(screen.queryByText("Loading stories…")).toBeNull();
      expect(screen.queryByRole("listbox")).toBeNull();
    },
  );
});

describe("launch sheet existing controls", () => {
  it.each(["story", "team-workspace"] as const)(
    "keeps defaults, edits, face search and keyboard submit for %s",
    async (where) => {
      const { sheet, onClose } = renderSheet({
        teamKey: TEAM,
        agentKey: TRIAGE,
        ...(where === "story" ? { storyId: "SW-8" } : {}),
      });
      await waitFor(() =>
        expect(sheet.getByTestId("launch-agent").textContent).toContain("Triage"),
      );
      expect(sheet.getByTestId("launch-name")).toHaveValue("Triage");
      expect(sheet.getByRole("radio", { name: "Saved default (Triage)" })).toHaveAttribute(
        "aria-checked",
        "true",
      );
      expect(sheet.getByTestId("launch-face-auto")).toHaveAttribute("aria-checked", "true");
      expect(sheet.getByTestId("launch-machine")).toHaveTextContent("This Mac");
      const face = sheet
        .getAllByRole("radio")
        .find((r) => r.dataset.testid?.startsWith("launch-face-builtin"))!;
      const label = face.getAttribute("aria-label")!;
      fireEvent.change(sheet.getByTestId("launch-face-search"), { target: { value: label } });
      const found = sheet.getByRole("radio", { name: label });
      fireEvent.click(found);
      expect(found).toHaveAttribute("aria-checked", "true");
      fireEvent.keyDown(sheet.getByTestId("launch-face-search"), { key: "Escape" });
      expect(sheet.getByTestId("launch-face-search")).toHaveValue("");
      expect(onClose).toHaveBeenCalledTimes(1);
      onClose.mockClear();
      fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Field contract" } });
      await pickOption(sheet, "Other queue role", "Quality");
      fireEvent.change(sheet.getByTestId("launch-task"), {
        target: { value: "  /resume\nContinue.  " },
      });
      fireEvent.keyDown(sheet.getByTestId("launch-task"), { key: "Enter", metaKey: true });
      await waitFor(() => expect(onClose).toHaveBeenCalled());
      expect(launchCalls()[0]).toEqual({
        args: {
          target: where,
          ...(where === "team-workspace"
            ? {
                org: "acme/widgets",
                team: "default",
                worktree: "Field contract",
                createWorktree: true,
              }
            : {}),
          ...(where === "story" ? { storyId: "SW-8", machineId: "local" } : {}),
          name: "Field contract",
          agent: TRIAGE,
          initialPrompt: "  /resume\nContinue.  ",
          queueRole: "qa",
          avatar: face.dataset.testid!.slice("launch-face-".length),
        },
      });
    },
  );
});

describe("launch sheet: context worktrees", () => {
  const context = (id: string, name: string, lastUsedAt: number) => ({
    kind: "context",
    id,
    name,
    orgSlug: "acme/widgets",
    team: "default",
    branch: `default/${id}`,
    path: `/worktrees/${id}`,
    launchCwd: `/worktrees/${id}/default/content`,
    lastUsedAt,
    agents: [],
  });
  it("waits for placement after sessions before revealing a folded context workspace and keeps its filter", async () => {
    const worktree = context("own", "Sprint 1", 2);
    const workspaceId = `generic:${worktree.launchCwd}`;
    const filter = { teamKey: TEAM, workspaceId };
    contextWorktrees = [worktree];
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [{ id: workspaceId, kind: "generic", cwd: worktree.launchCwd, status: "ready" }],
      },
      agentsPaneFolds: { [workspaceFoldKey(TEAM, workspaceId)]: true },
      agentsWorkspaceFilter: filter,
    });
    const realReload = useAgentListProjectionStore.getState().reload;
    // The refresh command acknowledges before either independently delivered topic updates.
    useAgentListProjectionStore.setState({ reload: async () => {} });
    try {
      const { sheet, onClose } = renderSheet({ teamKey: TEAM, contextWorktreeId: "own" });
      await waitFor(() =>
        expect(sheet.getByTestId("launch-worktree")).toHaveTextContent("Sprint 1"),
      );
      fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Placement wait" } });
      fireEvent.click(sheet.getByTestId("launch-submit"));
      await waitFor(() => expect(onClose).toHaveBeenCalled());
      expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual(filter);

      const { dashboardListSessions } = await import("@/tauri/commands");
      const sessions = await dashboardListSessions();
      const launched = sessions.find((row) => row.displayName === "Placement wait")!;
      expect(launched).toBeDefined();
      // The sessions owner arrives first, as it does in the live hub's publication order.
      act(() => useAgentListProjectionStore.setState({ sessions }));
      expect(useViewerStore.getState().agentsPaneFolds[workspaceFoldKey(TEAM, workspaceId)]).toBe(
        true,
      );
      expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual(filter);

      act(() =>
        useAgentListProjectionStore.setState((state) => ({
          references: [
            ...state.references,
            {
              id: JSON.stringify([TEAM, launched.handle]),
              sessionRef: launched.sessionId!,
              inboxRef: "counts:team:acme/widgets/default",
              teamKey: TEAM,
              parentId: null,
              parentSource: null,
              order: state.references.length,
              workspaceRef: workspaceId,
              storyRef: null,
              workspaceRank: 0,
            },
          ],
        })),
      );
      expect(
        useViewerStore.getState().agentsPaneFolds[workspaceFoldKey(TEAM, workspaceId)],
      ).toBeUndefined();
      expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual(filter);
      // A subsequent placement update must not undo a fold the person makes afterwards.
      act(() =>
        useViewerStore.getState().setAgentsPaneFold(workspaceFoldKey(TEAM, workspaceId), true),
      );
      act(() =>
        useAgentListProjectionStore.setState((state) => ({ references: [...state.references] })),
      );
      expect(useViewerStore.getState().agentsPaneFolds[workspaceFoldKey(TEAM, workspaceId)]).toBe(
        true,
      );
    } finally {
      useAgentListProjectionStore.setState({ reload: realReload });
      useViewerStore.setState({ workspaces: {}, agentsPaneFolds: {}, agentsWorkspaceFilter: null });
    }
  });
  it("preselects the most recent closed worktree and sends its id with the captured team", async () => {
    contextWorktrees = [context("old", "Copy pass", 1), context("recent", "Sprint 1", 2)];
    const { sheet, onClose } = renderSheet({ teamKey: TEAM });
    await waitFor(() => expect(sheet.getByTestId("launch-worktree")).toHaveTextContent("Sprint 1"));
    expect(sheet.getByTestId("launch-preview")).toHaveTextContent("Sprint 1");
    expect(sheet.getByTestId("launch-where-team-workspace")).toHaveAccessibleName("Team folder");
    act(() => useOrgStore.setState({ activeTeamName: "other", activeOrgSlug: "other" }));
    fireEvent.click(sheet.getByTestId("launch-submit"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(launchCalls()[0]).toEqual({
      args: {
        target: "team-workspace",
        worktree: "recent",
        team: "default",
        org: "acme/widgets",
        name: "Claude",
        agent: "builtin/claude",
      },
    });
  });
  it("preselects the pill's worktree and supports New, editable suggestions, duplicate errors and Escape", async () => {
    contextWorktrees = [context("recent", "Claude", 2), context("own", "Copy pass", 1)];
    const user = (await import("@testing-library/user-event")).default.setup();
    const { sheet, onClose } = renderSheet({ teamKey: TEAM, contextWorktreeId: "own" });
    await waitFor(() =>
      expect(sheet.getByTestId("launch-worktree")).toHaveTextContent("Copy pass"),
    );
    await user.click(sheet.getByTestId("launch-worktree"));
    await user.click(screen.getByRole("menuitem", { name: "New worktree…" }));
    const input = sheet.getByTestId("launch-worktree-name");
    expect(input).toHaveValue("Claude-2");
    expect(document.activeElement).toBe(input);
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "New agent" } });
    expect(input).toHaveValue("New agent");
    fireEvent.change(input, { target: { value: "cLaUdE" } });
    fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Other agent" } });
    expect(input).toHaveValue("cLaUdE");
    fireEvent.click(sheet.getByTestId("launch-submit"));
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(document.getElementById(input.getAttribute("aria-describedby")!)).toHaveTextContent(
      "already exists",
    );
    expect(launchCalls()).toEqual([]);
    fireEvent.keyDown(input, { key: "Escape" });
    expect(sheet.getByTestId("launch-worktree")).toHaveTextContent("Copy pass");
    expect(onClose).not.toHaveBeenCalled();
  });
  it("relaunches a missing worktree with the most recent selected and says so", async () => {
    contextWorktrees = [context("recent", "Sprint 1", 2)];
    const { sheet } = renderSheet({
      teamKey: TEAM,
      contextWorktreeId: "gone",
      agentKey: TRIAGE,
      name: "Review pass",
      queueRole: "qa",
      managerHandle: MANAGER,
      task: "/resume",
    });
    await waitFor(() => expect(sheet.getByTestId("launch-worktree")).toHaveTextContent("Sprint 1"));
    expect(sheet.getByText(/previous worktree no longer exists/)).toBeInTheDocument();
    expect(sheet.getByTestId("launch-name")).toHaveValue("Review pass");
    expect(sheet.getByTestId("launch-task")).toHaveValue("/resume");
    expect(sheet.getByRole("combobox", { name: "Other queue role" })).toHaveTextContent("Quality");
  });
  it.each([
    ["qa", TRIAGE],
    [null, "builtin/claude"],
  ] as const)(
    "submits a relaunch's queue role %j, hierarchy and face without reselecting them",
    async (queueRole, agentKey) => {
      contextWorktrees = [context("own", "Sprint 1", 2)];
      const request = teamSessionLaunchRequest(
        TEAM,
        {
          id: "own",
          kind: "context",
          contextWorktreeId: "own",
          cwd: "/work/default/content",
          status: "ready",
        },
        {
          displayName: "Review pass",
          agentRevision: `${agentKey}@7`,
          role: "investigator",
          queueRole,
          avatar: "builtin-funny:sysadmin-goblin",
          launchedBy: MANAGER,
          startedWith: "/resume",
        } as import("@/tauri/commands").AgentSessionRow,
        "fallback",
      );
      expect(request).not.toBeNull();
      const { sheet, onClose } = renderSheet(request!);
      await waitFor(() =>
        expect(sheet.getByTestId("launch-worktree")).toHaveTextContent("Sprint 1"),
      );
      if (queueRole === null) {
        expect(sheet.getByRole("radio", { name: "No role" })).toHaveAttribute(
          "aria-checked",
          "true",
        );
      } else {
        expect(sheet.getByRole("combobox", { name: "Other queue role" })).toHaveTextContent(
          queueRole === "qa" ? "Quality" : String(queueRole),
        );
      }
      expect(sheet.getByTestId("launch-face-builtin-funny:sysadmin-goblin")).toHaveAttribute(
        "aria-checked",
        "true",
      );
      fireEvent.click(sheet.getByTestId("launch-submit"));
      await waitFor(() => expect(onClose).toHaveBeenCalled());
      expect(launchCalls()[0]).toEqual({
        args: {
          target: "team-workspace",
          worktree: "own",
          team: "default",
          org: "acme/widgets",
          name: "Review pass",
          agent: agentKey,
          role: "investigator",
          queueRole,
          avatar: "builtin-funny:sysadmin-goblin",
          launchedBy: MANAGER,
          initialPrompt: "/resume",
        },
      });
    },
  );
});

describe("launch sheet picker contract", () => {
  it.each(["story", "team-workspace"] as const)(
    "preserves Agent, Manager, queue role, Name, Face and submit keys for %s",
    async (where) => {
      const user = (await import("@testing-library/user-event")).default.setup();
      const { sheet, onClose } = renderSheet({
        teamKey: TEAM,
        ...(where === "story" ? { storyId: "SW-8" } : {}),
      });
      await waitFor(() => expect(sheet.getByTestId("launch-agent")).toHaveTextContent("Claude"));
      await user.click(sheet.getByTestId("launch-agent"));
      await user.click(screen.getByRole("menuitem", { name: /^Triage/ }));
      expect(sheet.getByTestId("launch-name")).toHaveValue("Triage");
      fireEvent.change(sheet.getByTestId("launch-name"), { target: { value: "Sticky name" } });
      await user.click(sheet.getByTestId("launch-manager"));
      await user.click(screen.getByRole("menuitem", { name: /^Manager/ }));
      expect(sheet.getByTestId("launch-manager")).toHaveTextContent("Manager");
      await pickOption(sheet, "Other queue role", "Quality");
      const auto = sheet.getByTestId("launch-face-auto");
      auto.focus();
      fireEvent.keyDown(auto, { key: "ArrowRight" });
      const face = document.activeElement as HTMLElement;
      expect(face.dataset.testid).toMatch(/^launch-face-builtin/);
      await user.keyboard("{Enter}");
      expect(face).toHaveAttribute("aria-checked", "true");
      expect(sheet.getByTestId("launch-name")).toHaveValue("Sticky name");
      fireEvent.change(sheet.getByTestId("launch-task"), {
        target: { value: "  /resume\nKeep spacing.  " },
      });
      fireEvent.keyDown(sheet.getByTestId("launch-task"), { key: "Enter", ctrlKey: true });
      await waitFor(() => expect(onClose).toHaveBeenCalled());
      expect(launchCalls()[0]).toMatchObject({
        args: {
          target: where,
          name: "Sticky name",
          agent: TRIAGE,
          launchedBy: MANAGER,
          queueRole: "qa",
          avatar: face.dataset.testid!.slice("launch-face-".length),
          initialPrompt: "  /resume\nKeep spacing.  ",
        },
      });
      if (where === "story")
        expect(launchCalls()[0].args).toEqual({
          target: "story",
          storyId: "SW-8",
          machineId: "local",
          name: "Sticky name",
          agent: TRIAGE,
          launchedBy: MANAGER,
          queueRole: "qa",
          avatar: face.dataset.testid!.slice("launch-face-".length),
          initialPrompt: "  /resume\nKeep spacing.  ",
        });
    },
  );
});

describe("the launch sheet names the picked CLI honestly", () => {
  it.each(AGENT_RUNTIMES)(
    "%s: Preview on the picker, and an endpoint it lacks",
    async (runtime) => {
      world.editAs(
        TRIAGE,
        { runtime, args: [], permissions: null, startup_settings: null },
        { kind: "user", name: "Alice" },
      );
      const cli = registeredCli(runtime)!;
      const label = agentCliLabel(runtime);
      const { sheet, onClose } = renderSheet({ teamKey: TEAM, agentKey: TRIAGE });
      const trigger = sheet.getByTestId("launch-agent");
      await waitFor(() => expect(trigger).toHaveTextContent(label));
      expect(trigger.querySelector('[data-testid="preview-badge"]') !== null).toBe(cli.preview);
      if (cli.preview) expect(trigger).toHaveAccessibleDescription(PREVIEW_RUNTIME_NOTE);

      fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false });
      const item = (await screen.findAllByRole("menuitem")).find((choice) =>
        choice.textContent?.startsWith("Triage"),
      )!;
      expect(item).toHaveTextContent(label);
      expect(item.querySelector('[data-testid="preview-badge"]') !== null).toBe(cli.preview);
      expect(item).not.toHaveAttribute("aria-disabled");
      fireEvent.keyDown(item, { key: "Escape" });

      const model = sheet.getByTestId("launch-account");
      if (cli.modelEndpoint) {
        expect(item).toHaveTextContent("Needs a model endpoint");
        await waitFor(() => expect(model).toHaveTextContent("Needs a model endpoint"));
        expect(model).not.toHaveTextContent("CLI default");
        fireEvent.click(sheet.getByRole("button", { name: "Fix in Edit → Runs on" }));
        expect(onClose).toHaveBeenCalled();
        await waitFor(() => expect(useAgentsStore.getState().view).toBe("edit"));
        expect(useAgentsStore.getState().selectedKey).toBe(TRIAGE);
      } else {
        expect(item).not.toHaveTextContent("Needs a model endpoint");
        expect(model).not.toHaveTextContent("Needs a model endpoint");
      }
    },
  );
});

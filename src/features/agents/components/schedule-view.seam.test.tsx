// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { runtimeLabel } from "../lib/agent-display";
import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockAgents } from "@/app/mock/handlers/agents";
import {
  agentRoutinesSeed,
  createMockRoutines,
  type MockRoutine,
} from "@/app/mock/handlers/routines";
import {
  resetRoutinesStoreForTests,
  useRoutinesStore,
} from "@/features/routines/stores/routines-store";
import { UNKNOWN_COPY } from "@/features/routines/lib/run-status";
import { openRoutineDraft, openRoutines } from "@/features/routines/routines-virtual-tab";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { MachineSummary } from "@/tauri/commands";

import { resetAgentsStoreForTests, useAgentsStore } from "../stores/agents-store";
import { AgentsTab } from "./agents-tab";

/**
 * SW-AGR-5: the Agents tab's Schedule view (design §4.1, §4.8, SPEC §10). Driven through the
 * app's own door (`window.__TAURI_INTERNALS__`) over the agents, routines and machines
 * mocks: every read is a real command round-trip and every write emits its change event.
 */

const { openTabs, results } = vi.hoisted(() => ({
  openTabs: [] as string[],
  results: [] as Array<{ run_id: string; sessionId: string | null }>,
}));
vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: (path: string) => openTabs.push(path),
}));
vi.mock("@/features/viewer/components/agent-result-panel", () => ({
  openAgentResult: (arg: { run_id: string; sessionId: string | null }) => results.push(arg),
}));

const TRIAGE = "acme/backend/triage";

let MACHINES: MachineSummary[] = [];
const ALL_MACHINES: MachineSummary[] = [
  {
    machineId: "local",
    kind: "local",
    label: "This Mac",
    scope: null,
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: 1 },
    attention: 0,
    capabilities: { agentCallbacks: true },
  },
  {
    machineId: "codesense-test-1",
    kind: "ssh",
    label: "codesense-test-1",
    scope: { kind: "team", orgSlug: "acme", team: "backend" },
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: 1 },
    attention: 0,
    capabilities: { agentCallbacks: true },
  },
];

let calls: Array<{ cmd: string; args: Record<string, unknown> }>;

/** The demo routines, waking on the machine this test's team shares. */
const demo = (now: number) => agentRoutinesSeed(now, "codesense-test-1");

function boot(seed: (now: number) => MockRoutine[] = demo) {
  calls = [];
  const events = new MockEventBus();
  const agents = createMockAgents(events, { running: [`${TRIAGE}@3`] });
  const routines = createMockRoutines(events, {
    seed,
    runMs: null,
    agents: {
      triage: { name: "Triage" },
      scribe: { name: "Scribe" },
      reviewer: { name: "Reviewer" },
      dev: { name: "Dev" },
    },
  });
  const router = createInvokeRouter({
    ...events.handlers(),
    ...agents.handlers,
    ...routines.handlers,
    list_machines: () => [...MACHINES],
  });
  const internals = buildTauriInternals(async (cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args });
    return router(cmd, args);
  });
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
}

beforeEach(() => {
  MACHINES = [...ALL_MACHINES];
  openTabs.length = 0;
  results.length = 0;
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useViewerStore.setState({ agentsListFilter: null });
  useLocalProfileStore.setState({
    status: { active: { principal: "1", displayName: "Alice" } },
  } as unknown as Parameters<typeof useLocalProfileStore.setState>[0]);
});

afterEach(() => {
  cleanup();
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
});

function renderTab() {
  return render(
    <TooltipProvider>
      <AgentsTab filePath="virtual://agents" teamContentPath="" />
    </TooltipProvider>,
  );
}

async function openSchedule() {
  renderTab();
  await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
  fireEvent.mouseDown(screen.getByRole("tab", { name: /^Schedule/ }));
  const view = await screen.findByTestId("schedule-view");
  await within(view).findAllByTestId("schedule-row");
  return view;
}

describe("Agents | Schedule", () => {
  it("creates and edits a command from the live Schedule toolbar and Commands group", async () => {
    boot();
    const view = await openSchedule();
    fireEvent.click(within(view).getByRole("button", { name: "New routine" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Sync feed" } });
    fireEvent.change(screen.getByLabelText("When"), { target: { value: "5m" } });
    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "command" } });
    fireEvent.change(screen.getByLabelText("Program"), { target: { value: "node" } });
    fireEvent.click(screen.getByRole("button", { name: "Add an argument" }));
    fireEvent.change(screen.getByLabelText("Argument 1"), { target: { value: "sync.js" } });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create routine" })).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await screen.findByTestId("routine-detail");
    const group = within(view).getByRole("region", { name: "Commands" });
    expect(within(group).queryByRole("button", { name: /Open/ })).toBeNull();
    expect(within(group).getByText("node")).toBeInTheDocument();
    expect(screen.queryByTestId("routine-agent-unavailable")).toBeNull();
    expect(screen.queryByTestId("routine-agent-fixed")).toBeNull();
    fireEvent.mouseDown(screen.getByRole("tab", { name: /^Agents$/ }));
    fireEvent.mouseDown(screen.getByRole("tab", { name: /^Schedule/ }));
    await screen.findByTestId("schedule-view");
    fireEvent.click(screen.getByRole("button", { name: /Sync feed.*Runs/ }));
    expect(screen.getByLabelText("Program")).toHaveValue("node");
    fireEvent.change(screen.getByLabelText("Argument 1"), { target: { value: "refresh.js" } });
    fireEvent.click(screen.getByRole("button", { name: "New routine" }));
    const guard = await screen.findByTestId("routine-discard-confirm");
    fireEvent.click(within(guard).getByRole("button", { name: "Cancel" }));
    expect(screen.getByLabelText("Argument 1")).toHaveValue("refresh.js");
    await waitFor(() => expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() =>
      expect(
        calls.some(
          (c) => c.cmd === "routine_save" && JSON.stringify(c.args).includes("refresh.js"),
        ),
      ).toBe(true),
    );
    fireEvent.mouseDown(screen.getByRole("tab", { name: /^Recent$/ }));
    expect(screen.queryByText(/needs an agent/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Run now" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "routine_run_now")).toBe(true));
    fireEvent.keyDown(screen.getByRole("button", { name: "More for Sync feed" }), {
      key: "ArrowDown",
    });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Archive routine" }));
    const archive = await screen.findByTestId("routine-archive-confirm");
    fireEvent.click(within(archive).getByRole("button", { name: /^Archive$/ }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "routine_archive")).toBe(true));
  }, 20_000);

  it("follows Then links into a command and then the next agent's own routine page", async () => {
    boot((now) => {
      const seed = demo(now);
      const upstream = seed.find((r) => r.row.agent_key === TRIAGE && r.row.scope === "team")!;
      upstream.row.def.name = "Upstream";
      const nextAgent = seed.find(
        (r) => r.row.agent_key === "acme/backend/scribe" && r.row.scope === "team",
      )!;
      nextAgent.row.def.name = "Next agent";
      nextAgent.row.def.after = { routine: "next-command", on: "succeeded" };
      const nextCommand: MockRoutine = {
        row: {
          ...upstream.row,
          key: "acme/backend/next-command",
          agent_key: null,
          effective_mode: "fresh",
          def: {
            ...upstream.row.def,
            id: "next-command",
            name: "Next command",
            action: "command",
            where: { kind: "team_workspace", machine: "local" },
            agent: undefined,
            task: undefined,
            mode: undefined,
            command: ["printf", "next"],
            after: { routine: upstream.row.def.id, on: "succeeded" },
          },
        },
        runs: [],
      };
      return [upstream, nextCommand, nextAgent];
    });
    await openSchedule();
    fireEvent.click(
      screen
        .getByText("Upstream", { selector: "[data-testid=schedule-row-name]" })
        .closest("button")!,
    );
    await screen.findByTestId("routine-page");
    fireEvent.click(screen.getByRole("button", { name: /^Next command$/ }));
    expect(await screen.findByTestId("routine-detail")).toHaveTextContent("Next command");
    expect(useRoutinesStore.getState().routineOwner).toBeNull();
    expect(useAgentsStore.getState().tabView).toBe("schedule");
    fireEvent.click(screen.getByRole("button", { name: /^Next agent$/ }));
    const page = await screen.findByTestId("routine-page");
    expect(within(page).getByTestId("routine-breadcrumb-back")).toHaveTextContent("Scribe");
    expect(within(page).getByTestId("routine-agent-fixed")).toHaveTextContent("Scribe");
    expect(useRoutinesStore.getState().routineOwner).toBe("acme/backend/scribe");
    expect(useAgentsStore.getState().tabView).toBe("agents");
  });

  it("labels command spaces in Upcoming and Recent when multiple spaces are shown", async () => {
    boot((now) =>
      ["backend", "frontend"].map((team) => {
        const source = demo(now).find((r) => r.runs.length > 0)!;
        const key = `acme/${team}/sync`;
        return {
          row: {
            ...source.row,
            key,
            scope: "team",
            agent_key: null,
            effective_mode: "fresh",
            unassigned: false,
            def: {
              ...source.row.def,
              id: "sync",
              name: "Sync",
              action: "command",
              where: { kind: "team_workspace", machine: "local" },
              command: ["printf", "sync"],
              agent: undefined,
              task: undefined,
              mode: undefined,
            },
          },
          runs: [
            {
              ...source.runs[0],
              run_id: `sync-${team}`,
              routine_key: key,
              action: "command",
              created_at: now - 1000,
              started_at: now - 1000,
              ended_at: now,
              phase: "succeeded",
            },
          ],
        } as MockRoutine;
      }),
    );
    await openSchedule();
    let group = screen.getByRole("region", { name: "Commands" });
    expect(within(group).getByText("acme/backend")).toBeInTheDocument();
    expect(within(group).getByText("acme/frontend")).toBeInTheDocument();
    fireEvent.mouseDown(screen.getByRole("tab", { name: "Recent" }));
    await screen.findAllByTestId("schedule-recent-row");
    group = screen.getByRole("region", { name: "Commands" });
    expect(within(group).getByText("acme/backend")).toBeInTheDocument();
    expect(within(group).getByText("acme/frontend")).toBeInTheDocument();
  });

  it("switches views from the header; Schedule carries what needs attention", async () => {
    boot();
    renderTab();
    await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
    const tabs = screen.getByRole("tablist", { name: "Agents tab views" });
    const agents = within(tabs).getByRole("tab", { name: "Agents" });
    expect(agents.getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("button", { name: /New agent/ })).toBeTruthy();
    // Unassigned + the unknown and undelivered latest runs: 3.
    const schedule = await within(tabs).findByRole("tab", { name: "Schedule, 3 need attention" });
    fireEvent.keyDown(agents, { key: "ArrowRight" });
    await waitFor(() => expect(schedule.getAttribute("aria-selected")).toBe("true"));
    expect(document.activeElement).toBe(schedule);
    const scheduleView = await screen.findByTestId("schedule-view");
    expect(screen.queryByRole("button", { name: /New agent/ })).toBeNull();
    // The needs-attention filter is offered once, in the Schedule's own header, never in the
    // tab strip above it; and its chip does the same when it is on.
    const offer = within(scheduleView).getByRole("button", {
      name: /Show the 3 that need attention/,
    });
    expect(screen.getAllByRole("button", { name: /that need attention/ })).toHaveLength(1);
    expect(tabs.contains(offer)).toBe(false);
    fireEvent.click(offer);
    const chip = await within(scheduleView).findByTestId("schedule-filter-chip");
    expect(screen.getAllByTestId("schedule-filter-chip")).toHaveLength(1);
    expect(tabs.contains(chip)).toBe(false);
    fireEvent.click(within(chip).getByRole("button", { name: "Show every agent's routines" }));
    expect(screen.getByTestId("routines-status-strip").textContent).toContain(
      "Routines on This Mac run while CodeSense is open.",
    );
  });

  it("groups by agent then machine, Unassigned first; never by routine", async () => {
    boot();
    const view = await openSchedule();
    const unassigned = within(view).getByTestId("schedule-unassigned");
    expect(unassigned.textContent).toContain("1 needs an agent");
    expect(unassigned.textContent).toContain("was: pockets");
    const groups = within(view).getAllByTestId("schedule-group");
    expect(groups.map((g) => g.dataset.agent)).toEqual([
      "_personal/1/personal/dev",
      "acme/backend/reviewer",
      "acme/backend/scribe",
      TRIAGE,
    ]);
    const triage = within(groups[3]);
    expect(triage.getAllByTestId("schedule-machine").map((m) => m.textContent)).toEqual([
      "on This Mac",
      "on codesense-test-1",
    ]);
    // A cross-agent trigger shows under the agent it runs, as "after Triage".
    expect(within(groups[1]).getByTestId("schedule-row").textContent).toContain("after");
  });

  it("an agent's N routines link opens Schedule filtered to it; × shows every agent", async () => {
    boot();
    renderTab();
    await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
    const triage = screen.getAllByTestId("agent-row").find((r) => r.dataset.agentKey === TRIAGE)!;
    fireEvent.click(within(triage).getByRole("button", { name: "Open Triage" }));
    fireEvent.click(await screen.findByTestId("agent-routines-link"));
    const view = await screen.findByTestId("schedule-view");
    await waitFor(() =>
      expect(
        within(view)
          .getAllByTestId("schedule-group")
          .map((g) => g.dataset.agent),
      ).toEqual([TRIAGE]),
    );
    fireEvent.click(screen.getByRole("button", { name: "Show every agent's routines" }));
    await waitFor(() => expect(within(view).getAllByTestId("schedule-group")).toHaveLength(4));
  });

  it("a row opens the routine page on the right, named by its agent", async () => {
    boot();
    const view = await openSchedule();
    const row = within(view)
      .getAllByTestId("schedule-row")
      .find((r) => r.textContent?.includes("Inbox sweep"))!;
    fireEvent.click(within(row).getByRole("button", { name: /Inbox sweep/ }));
    const page = await screen.findByTestId("routine-page");
    expect(within(page).getByTestId("routine-breadcrumb-back").textContent).toBe("Triage");
    fireEvent.click(within(page).getByTestId("routine-breadcrumb-back"));
    await screen.findByText("Pick a routine to see its runs.");
  });

  it("Pick an agent moves an Unassigned routine under that agent", async () => {
    boot();
    const view = await openSchedule();
    fireEvent.click(within(view).getByTestId("schedule-pick-agent"));
    const picker = await screen.findByTestId("schedule-pick-agent-popover");
    // The routine's own team's saved agents by name, then (it starts a new session) the
    // built-in starters (design §4.8.1).
    expect(
      within(picker)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ).toEqual([
      "CodexReviewerbackend",
      "ClaudeScribebackend",
      "ClaudeTriagebackend",
      ...useAgentsStore
        .getState()
        .rows!.filter((row) => row.scope === "builtin")
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((row) => `${runtimeLabel(row.runtime)}${row.name}`),
    ]);
    fireEvent.click(within(picker).getByRole("button", { name: /Triage/ }));
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "routine_save")?.args).toMatchObject({
        key: "acme/backend/weekly-changelog",
        form: { agent: "triage" },
      }),
    );
    await waitFor(() => expect(within(view).queryByTestId("schedule-unassigned")).toBeNull());
  });

  it("Recent lists runs with honest outcomes; unknown has Details and no Retry", async () => {
    boot();
    const view = await openSchedule();
    fireEvent.mouseDown(within(view).getByRole("tab", { name: "Recent" }));
    await within(view).findByText(UNKNOWN_COPY);
    const triage = within(view)
      .getAllByTestId("schedule-group")
      .find((g) => g.dataset.agent === TRIAGE)!;
    expect(within(triage).getByText(/\d+ runs?/)).toBeTruthy();
    expect(within(triage).getAllByRole("button", { name: "Details" }).length).toBeGreaterThan(0);
    expect(within(triage).queryByRole("button", { name: /retry/i })).toBeNull();
    expect(within(triage).getByText("+1 merged or skipped")).toBeTruthy();
  });

  it("Show the N that need attention filters to them", async () => {
    boot();
    const view = await openSchedule();
    fireEvent.click(screen.getByRole("button", { name: "Show the 3 that need attention" }));
    await waitFor(() =>
      expect(
        within(view)
          .getAllByTestId("schedule-row")
          .map((r) => within(r).getByTestId("schedule-row-name").textContent),
      ).toEqual(["Weekly changelog", "Review after triage", "Inbox sweep"]),
    );
    expect(screen.getByTestId("schedule-filter-chip").textContent).toBe("Needs attention");
  });
});

describe("retired Routines entry points (SPEC §10)", () => {
  it("openRoutines opens Agents → Schedule, filtered when it names an agent", () => {
    openRoutines({ agentKey: TRIAGE, name: "Triage", routineKeys: [] });
    expect(openTabs).toEqual(["virtual://agents"]);
    expect(useAgentsStore.getState().tabView).toBe("schedule");
    expect(useAgentsStore.getState().scheduleFilter).toEqual({
      kind: "agent",
      agentKey: TRIAGE,
      name: "Triage",
    });
    act(() => openRoutines());
    expect(useAgentsStore.getState().scheduleFilter).toBeNull();
  });
});

describe("review r1", () => {
  it("Keep this… from the Schedule view switches to Agents → the agent → New routine", async () => {
    boot();
    renderTab();
    await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
    act(() => useAgentsStore.getState().setTabView("schedule"));
    await screen.findByTestId("schedule-view");
    act(() =>
      openRoutineDraft({
        draft: { agent: TRIAGE, name: "Inbox nudge", task: "read your inbox", when: "15m" },
        mode: "wake",
        machineId: "codesense-test-1",
      }),
    );
    await waitFor(() => expect(useAgentsStore.getState().tabView).toBe("agents"));
    expect(useAgentsStore.getState().selectedKey).toBe(TRIAGE);
    const page = await screen.findByTestId("routine-page");
    expect(within(page).getByLabelText("Name")).toHaveValue("Inbox nudge");
    expect(screen.queryByTestId("schedule-view")).toBeNull();
  });

  it("a routine archived from the Schedule's page is not reopened by a view switch", async () => {
    boot();
    const view = await openSchedule();
    const row = within(view)
      .getAllByTestId("schedule-row")
      .find((r) => r.textContent?.includes("Weekly retro"))!;
    fireEvent.click(within(row).getByRole("button", { name: /Weekly retro/ }));
    await screen.findByTestId("routine-page");
    await act(() => useRoutinesStore.getState().archiveRoutine("acme/backend/weekly-retro"));
    expect(useAgentsStore.getState().scheduleRoutine).toBeNull();
    fireEvent.mouseDown(screen.getByRole("tab", { name: "Agents" }));
    fireEvent.mouseDown(screen.getByRole("tab", { name: /^Schedule/ }));
    await screen.findByText("Pick a routine to see its runs.");
  });

  it("Recent is one bounded read, not one per routine", async () => {
    boot();
    const view = await openSchedule();
    calls.length = 0;
    fireEvent.mouseDown(within(view).getByRole("tab", { name: "Recent" }));
    await within(view).findByText(UNKNOWN_COPY);
    expect(calls.filter((c) => c.cmd === "routine_runs_since")).toHaveLength(1);
    // The backend command reads `perRoutine` (per_routine): the cap travels by that name.
    expect(calls.find((c) => c.cmd === "routine_runs_since")?.args).toMatchObject({
      perRoutine: 50,
    });
    expect(calls.filter((c) => c.cmd === "routine_runs")).toHaveLength(0);
  });

  it("opening an unknown run's Details counts it as seen", async () => {
    boot();
    renderTab();
    await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
    const tabs = screen.getByRole("tablist", { name: "Agents tab views" });
    await within(tabs).findByRole("tab", { name: "Schedule, 3 need attention" });
    fireEvent.mouseDown(within(tabs).getByRole("tab", { name: /^Schedule/ }));
    const view = await screen.findByTestId("schedule-view");
    const sweep = (await within(view).findAllByTestId("schedule-row")).find((r) =>
      r.textContent?.includes("Inbox sweep"),
    )!;
    fireEvent.click(within(sweep).getByRole("button", { name: "Details" }));
    await screen.findByTestId("run-details");
    await within(tabs).findByRole("tab", { name: "Schedule, 2 need attention" });
  });

  it("an offline machine's sub-head says so", async () => {
    MACHINES = [ALL_MACHINES[0], { ...ALL_MACHINES[1], status: { state: "disconnected" } }];
    boot();
    const view = await openSchedule();
    const heads = within(view)
      .getAllByTestId("schedule-machine")
      .map((m) => m.textContent);
    expect(heads).toContain("on codesense-test-1 · offline");
  });
});

describe("SW-XS-25", () => {
  it("a machine-armed routine out of date there shows why and Refresh on the Schedule", async () => {
    boot((now) =>
      demo(now).map((r) =>
        r.row.def.name === "Inbox sweep"
          ? {
              ...r,
              row: {
                ...r.row,
                def: { ...r.row.def, armed_machine: "codesense-test-1" },
                content_stale: {
                  machine: "codesense-test-1",
                  since_ms: now,
                  why: "not prepared on the machine yet",
                },
              },
            }
          : r,
      ),
    );
    const view = await openSchedule();
    const row = within(view)
      .getAllByTestId("schedule-row")
      .find((r) => r.textContent?.includes("Inbox sweep"))!;
    const stale = within(row).getByTestId("routine-stale");
    expect(within(stale).getByTestId("routine-stale-why").textContent).toBe(
      "Not prepared on the machine yet.",
    );
    fireEvent.click(
      within(stale).getByRole("button", { name: "Refresh Inbox sweep on codesense-test-1" }),
    );
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "routine_refresh_on_machine")).toBe(true),
    );
    await waitFor(() => expect(within(view).queryByTestId("routine-stale")).toBeNull());
  });
});

describe("retained hidden team", () => {
  /** A hidden warm team's earlier copy of a repeating-id control, ahead of the shown one. */
  function hiddenCopy(attrs: Record<string, string>, tag = "button") {
    const wrap = document.createElement("div");
    wrap.style.display = "none";
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    wrap.append(el);
    document.body.prepend(wrap);
    return { el, remove: () => wrap.remove() };
  }

  it("Schedule list arrow keys focus the shown team's tab, not a hidden copy of its id", async () => {
    boot();
    const decoy = hiddenCopy({ id: "schedule-list-tab-recent" });
    const view = await openSchedule();
    const upcoming = within(view).getByRole("tab", { name: /Upcoming/ });
    fireEvent.keyDown(upcoming, { key: "ArrowRight" });
    const recent = within(view).getByRole("tab", { name: /Recent/ });
    await waitFor(() => expect(document.activeElement).toBe(recent));
    expect(document.activeElement).not.toBe(decoy.el);
    decoy.remove();
  });

  it("the routine page's Back returns focus to a shown control, never a hidden copy", async () => {
    boot();
    const view = await openSchedule();
    const row = within(view)
      .getAllByTestId("schedule-row")
      .find((r) => r.textContent?.includes("Inbox sweep"))!;
    fireEvent.click(within(row).getByRole("button", { name: /Inbox sweep/ }));
    const page = await screen.findByTestId("routine-page");
    const key = useRoutinesStore.getState().selectedKey;
    const decoy = hiddenCopy(
      key ? { "data-routine-open": key } : { "data-testid": "agent-routine-add" },
    );
    fireEvent.click(within(page).getByTestId("routine-breadcrumb-back"));
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    expect(document.activeElement).not.toBe(decoy.el);
    decoy.remove();
  });
});

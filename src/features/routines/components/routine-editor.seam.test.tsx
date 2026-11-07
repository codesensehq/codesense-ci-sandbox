// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import {
  createMockRoutines,
  type MockRoutine,
  type MockRoutines,
} from "@/app/mock/handlers/routines";
import { createMockAgents } from "@/app/mock/handlers/agents";
import { resetAgentsStoreForTests, useAgentsStore } from "@/features/agents/stores/agents-store";
import { useOrgStore } from "@/shared/stores/org-store";
import type { AgentRow, RoutineRow } from "@/tauri/commands";
import { AGENTS_CHANGED_EVENT, ROUTINES_CHANGED_EVENT } from "@/tauri/events";

import { RunsView as RoutineRunsView } from "./routine-detail";
import { useRoutinesStore, resetRoutinesStoreForTests } from "../stores/routines-store";
import { RoutineOverview } from "./routine-editor";
import { RoutinesTab } from "./routines-tab";

const openTabs: string[] = [];
vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: (path: string) => openTabs.push(path),
}));

/**
 * The routine editor driven through `window.__TAURI_INTERNALS__` over the mock
 * routines backend: create through the sentence form, edit, lose a race to an
 * agent's edit, read History and restore — every step a real command round-trip.
 */

function teammates(): MockRoutine[] {
  return [
    {
      row: {
        key: "acme/backend/standup",
        scope: "team",
        armed_here: false,
        owner_display: "Sam",
        enabled: true,
        created_by: { kind: "user", name: "sam" },
        schedule_in_words: "weekdays at 8:00",
        next_run_at: null,
        def: {
          id: "standup",
          name: "Standup",
          owner: "2",
          agent: "scribe",
          task: "Notes.",
          where: { kind: "team_workspace" },
          triggers: [{ schedule: { cron: "0 8 * * 1-5", tz: "UTC" } }],
          policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: false },
          revision: 1,
          armed_host: "sams-mac",
        },
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh" as const,
        mine: false,
      },
      runs: [],
    },
    {
      row: {
        key: "acme/backend/release-notes",
        scope: "team",
        armed_here: false,
        owner_display: "Sam",
        enabled: true,
        created_by: { kind: "user", name: "sam" },
        schedule_in_words: "Fridays at 16:00",
        next_run_at: null,
        def: {
          id: "release-notes",
          name: "Release notes",
          owner: "2",
          agent: "writer",
          task: "Draft the notes.",
          where: { kind: "team_workspace" },
          triggers: [{ schedule: { cron: "0 16 * * 5", tz: "UTC" } }],
          policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: false },
          revision: 1,
          armed_host: "sams-mac",
        },
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh" as const,
        mine: false,
      },
      runs: [],
    },
  ];
}

function overviewRow(): RoutineRow {
  return {
    ...teammates()[0].row,
    agent_display: { name: "Scribe", archived: false, resolvable: true },
    last_run: null,
    needs_you: false,
    needs_you_run: null,
  };
}

let world: MockRoutines;
let calls: string[];
/** A test's stand-in for one command's backend reply. */
let overrides: Record<string, (args: Record<string, unknown>) => unknown>;

let bus: MockEventBus;

let orgBefore: ReturnType<typeof useOrgStore.getState>;

beforeEach(() => {
  resetRoutinesStoreForTests();
  resetAgentsStoreForTests();
  openTabs.length = 0;
  orgBefore = useOrgStore.getState();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  calls = [];
  overrides = {};
  const events = new MockEventBus();
  bus = events;
  world = createMockRoutines(events, { seed: teammates, missed: 0, runMs: null });
  agents = createMockAgents(events);
  const handlers = {
    ...events.handlers(),
    ...agents.handlers,
    ...world.handlers,
    query_context: () => ({
      schemaVersion: 1,
      total: 0,
      rows: [],
      indexing: false,
      generation: 1,
      textScanTruncated: false,
      totalApproximate: false,
    }),
  };
  const traced = Object.fromEntries(
    Object.entries(handlers).map(([name, fn]) => [
      name,
      (args: Record<string, unknown>) => {
        calls.push(name);
        if (overrides[name]) return overrides[name](args);
        return (fn as (a: Record<string, unknown>) => unknown)(args);
      },
    ]),
  );
  const internals = buildTauriInternals(createInvokeRouter(traced as typeof handlers));
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
});

afterEach(() => {
  cleanup();
  resetRoutinesStoreForTests();
  resetAgentsStoreForTests();
  useOrgStore.setState(orgBefore, true);
  vi.restoreAllMocks();
});

let agents: ReturnType<typeof createMockAgents>;

function renderTab() {
  return render(
    <TooltipProvider>
      <RoutinesTab filePath="virtual://routines" teamContentPath="" />
    </TooltipProvider>,
  );
}

const type = (label: string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });

/** Pick the agent whose option reads `name` from the sentence's Agent slot. */
async function pickAgent(name: string) {
  fireEvent.click(screen.getByRole("combobox", { name: "Agent" }));
  fireEvent.click(await screen.findByRole("option", { name: new RegExp(`${name}$`) }));
}

/** The Agent slot's groups and their options, as listed. */
function agentGroups() {
  return within(screen.getByRole("listbox"))
    .getAllByRole("group")
    .map((g) => [
      g.getAttribute("aria-label"),
      within(g)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ]);
}

const revisions = () =>
  screen.getAllByTestId("routine-revision").map((r) => ({
    revision: r.dataset.revision,
    author: within(r).getByTestId("routine-revision-author").textContent,
  }));

async function createEcho(enabled = true) {
  fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
  type("Name", "Echo");
  type("When", "5m");
  await pickAgent("Scribe");
  type("Task", "echo hi");
  if (!enabled) fireEvent.click(screen.getByRole("switch", { name: "Enabled" }));
  fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
  await screen.findByTestId("routine-overview");
}

describe("Routine editor over the mock backend", () => {
  it("disables machine-armed upstream routines in the after picker", async () => {
    renderTab();
    const create = await screen.findByRole("button", { name: "New routine" });
    act(() =>
      useRoutinesStore.setState((state) => ({
        rows:
          state.rows?.map((row) =>
            row.def.id === "standup"
              ? { ...row, def: { ...row.def, armed_machine: "remote" } }
              : row,
          ) ?? null,
      })),
    );
    fireEvent.click(create);
    fireEvent.click(screen.getByRole("button", { name: "After a routine" }));
    expect(screen.getByRole("option", { name: "Standup (armed on remote)" })).toBeDisabled();
    expect(screen.getByRole("option", { name: "Release notes" })).toBeEnabled();
  });

  it("creates a command with a completion trigger and keeps each argument literal", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    type("Name", "Feed command");
    fireEvent.change(screen.getByRole("combobox", { name: "Mode" }), {
      target: { value: "command" },
    });
    expect(screen.queryByLabelText("Agent")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Task")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Max runtime")).toHaveValue("5m");
    expect(screen.getByLabelText("Overlap")).toHaveValue("queue");
    expect(screen.queryByLabelText("Visible")).not.toBeInTheDocument();
    type("Program", "printf");
    fireEvent.click(screen.getByRole("button", { name: "Add an argument" }));
    expect(screen.getByLabelText("Argument 1")).toHaveFocus();
    type("Argument 1", "a | $HOME with spaces");
    fireEvent.keyDown(screen.getByLabelText("Argument 1"), { key: "Enter" });
    expect(screen.getByLabelText("Argument 2")).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Remove argument 2" }));
    expect(screen.getByLabelText("Argument 1")).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "After a routine" }));
    fireEvent.change(screen.getByLabelText("After routine"), { target: { value: "standup" } });
    fireEvent.change(screen.getByLabelText("After ending"), { target: { value: "finished" } });
    expect(screen.getByText(/Not when it is cancelled or skipped/)).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create routine" })).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await screen.findByTestId("routine-overview");
    expect(world.defOf("Feed command")).toMatchObject({
      action: "command",
      command: ["printf", "a | $HOME with spaces"],
      after: { routine: "standup", on: "finished" },
      agent: "",
      task: "",
      policy: { overlap: "queue", max_runtime: "5m" },
    });
    expect(screen.queryByTestId("routine-agent-unavailable")).not.toBeInTheDocument();
    type("Argument 1", "edited");
    fireEvent.click(screen.getByRole("button", { name: "Remove after trigger" }));
    type("When", "5m");
    await waitFor(() => expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(world.defOf("Feed command")?.command).toEqual(["printf", "edited"]));
    expect(world.defOf("Feed command")?.after).toBeUndefined();
  });

  it("creates every-5m echo from the sentence, edits it, and keeps an edit that lost a race", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Who can use it" }), {
      target: { value: "only-me" },
    });
    type("When", "5m");
    const fires = await screen.findByTestId("routine-next-fires");
    expect(fires).toHaveTextContent("Every 5 minutes · next:");
    expect(fires.textContent!.split("next: ")[1].split("; ")).toHaveLength(5);
    type("Name", "Echo");
    await pickAgent("Scribe");
    type("Task", "echo hi");
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));

    const mine = await screen.findByRole("region", { name: "Mine" });
    await waitFor(() => expect(within(mine).getByText("Echo")).toBeInTheDocument());
    const key = "_personal/1/personal/echo";
    const row = within(mine).getByTestId("routine-row");
    expect(row.dataset.routineKey).toBe(key);
    expect(row.dataset.selected).toBe("true");
    expect(screen.getByLabelText("When")).toHaveValue("5m");
    expect(screen.getByLabelText("Task")).toHaveValue("echo hi");

    // An edit by the user.
    type("Task", "echo there");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.getByText("Revision 2")).toBeInTheDocument());

    // The user starts another edit; an agent changes the routine meanwhile.
    type("Task", "echo mine");
    world.agentEdit(key, "triage-bot", { task: "echo from the agent" });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    const conflict = await screen.findByTestId("routine-conflict");
    expect(conflict).toHaveTextContent("Changed elsewhere — reload");
    expect(screen.getByLabelText("Task")).toHaveValue("echo mine");

    fireEvent.click(within(conflict).getByRole("button", { name: "Reload" }));
    await waitFor(() => expect(screen.getByLabelText("Task")).toHaveValue("echo from the agent"));
    expect(screen.queryByTestId("routine-conflict")).toBeNull();

    // History: who changed what, newest first.
    fireEvent.click(screen.getByRole("tab", { name: "History" }));
    await waitFor(() => expect(revisions()).toHaveLength(3));
    expect(revisions().map((r) => [r.revision, r.author?.split(" · ")[0]])).toEqual([
      ["3", "by triage-bot"],
      ["2", "by octocat"],
      ["1", "by octocat"],
    ]);
    const second = screen.getAllByTestId("routine-revision")[1];
    expect(within(second).getByTestId("routine-revision-diff")).toHaveTextContent(
      "Taskecho hi → echo there",
    );

    // Restore revision 1: a new revision with its body.
    const first = screen.getAllByTestId("routine-revision")[2];
    fireEvent.click(within(first).getByRole("button", { name: /Restore/ }));
    await waitFor(() => expect(revisions()).toHaveLength(4));
    expect(revisions()[0]).toEqual({ revision: "4", author: expect.stringMatching(/^by octocat/) });
    expect(
      within(screen.getAllByTestId("routine-revision")[0]).getByTestId("routine-revision-diff"),
    ).toHaveTextContent("Taskecho from the agent → echo hi");
    fireEvent.click(screen.getByRole("tab", { name: "Overview" }));
    await waitFor(() => expect(screen.getByLabelText("Task")).toHaveValue("echo hi"));
  });

  it("sets wake limits under a Limits section that starts collapsed, and clears one (SW-AGR-13)", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    const limits = screen.getByTestId("routine-limits");
    expect(limits).not.toHaveAttribute("open");
    expect(limits).toHaveTextContent("Limits · none");
    fireEvent.click(within(limits).getByText("Limits"));
    expect(limits).toHaveAttribute("open");
    type("Name", "Echo");
    type("When", "5m");
    await pickAgent("Scribe");
    type("Task", "echo hi");
    // A routine that starts a new agent each fire: the section says limits don't apply.
    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "fresh" } });
    expect(screen.getByTestId("routine-limits-hint")).toHaveTextContent("don't apply");
    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "wake" } });
    expect(screen.getByTestId("routine-limits-hint")).toHaveTextContent(
      "Each Mac and machine keeps its own count",
    );
    type("Max wakes", "20");
    type("Min gap", "4h");
    expect(limits).toHaveTextContent("Limits · 20 wakes in all · 4h apart");
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await screen.findByTestId("routine-overview");
    const saved = () => world.defOf("Echo")!.policy;
    await waitFor(() => expect(saved()).toMatchObject({ max_wakes: 20, min_gap: "4h" }));
    expect(saved().max_wakes_per_day).toBeUndefined();

    // Not a whole number: said in the form, and Save waits for a fix.
    type("Max wakes a day", "lots");
    expect(within(screen.getByTestId("routine-limits")).getByRole("alert")).toHaveTextContent(
      "Max wakes a day must be a whole number of 1 or more.",
    );
    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();

    // Edited: one limit cleared, another set.
    type("Max wakes", "");
    type("Max wakes a day", "3");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(saved().max_wakes_per_day).toBe(3));
    expect(saved().max_wakes).toBeUndefined();
    expect(saved().min_gap).toBe("4h");
  });

  it("keeps an unsaved edit across a look at Runs and History", async () => {
    renderTab();
    await createEcho();
    type("Task", "echo half-done");
    fireEvent.click(screen.getByRole("tab", { name: "Runs" }));
    fireEvent.click(screen.getByRole("tab", { name: "History" }));
    await screen.findByTestId("routine-history");
    fireEvent.click(screen.getByRole("tab", { name: "Overview" }));
    expect(screen.getByLabelText("Task")).toHaveValue("echo half-done");
    expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled();
  });

  it("asks before dropping an unsaved edit to open another routine or New routine", async () => {
    renderTab();
    await createEcho();
    type("Task", "echo half-done");

    // Selecting another routine asks first; Cancel keeps the edit where it was.
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    const ask = await screen.findByTestId("routine-discard-confirm");
    expect(ask).toHaveTextContent("Discard unsaved changes?");
    fireEvent.click(screen.getByTestId("routine-discard-confirm-cancel"));
    await waitFor(() => expect(screen.queryByTestId("routine-discard-confirm")).toBeNull());
    expect(screen.getByRole("heading", { name: "Echo" })).toBeInTheDocument();
    expect(screen.getByLabelText("Task")).toHaveValue("echo half-done");

    // New routine asks too; Discard drops the edit and opens the form.
    fireEvent.click(screen.getByRole("button", { name: "New routine" }));
    fireEvent.click(await screen.findByTestId("routine-discard-confirm-confirm"));
    expect(await screen.findByTestId("routine-create")).toBeInTheDocument();

    // Back on Echo the edit is gone, and with nothing unsaved a switch asks nothing.
    fireEvent.click(screen.getByRole("button", { name: "Open Echo" }));
    await waitFor(() => expect(screen.getByLabelText("Task")).toHaveValue("echo hi"));
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Standup" })).toBeInTheDocument(),
    );
    expect(screen.queryByTestId("routine-discard-confirm")).toBeNull();
  });

  it("asks before dropping a half-filled New routine form to open another routine", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    // An untouched form has nothing to lose: a switch asks nothing.
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    expect(screen.queryByTestId("routine-discard-confirm")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "New routine" }));
    type("Name", "Half");
    type("Task", "half a task");

    // Picking another routine asks first; Cancel keeps the form as it was.
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    const ask = await screen.findByTestId("routine-discard-confirm");
    expect(ask).toHaveTextContent("This new routine isn't saved yet.");
    fireEvent.click(screen.getByTestId("routine-discard-confirm-cancel"));
    await waitFor(() => expect(screen.queryByTestId("routine-discard-confirm")).toBeNull());
    expect(screen.getByTestId("routine-create")).toBeInTheDocument();
    expect(screen.getByLabelText("Task")).toHaveValue("half a task");

    // Turning a fresh form into a draft is an unsaved choice too.
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    fireEvent.click(await screen.findByTestId("routine-discard-confirm-confirm"));
    await screen.findByRole("heading", { name: "Standup" });
    fireEvent.click(screen.getByRole("button", { name: "New routine" }));
    fireEvent.click(screen.getByRole("switch", { name: "Enabled" }));
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    await screen.findByTestId("routine-discard-confirm");
    fireEvent.click(screen.getByTestId("routine-discard-confirm-cancel"));
    await waitFor(() => expect(screen.queryByTestId("routine-discard-confirm")).toBeNull());
    type("Name", "Half");
    type("Task", "half a task");

    // New routine again keeps the same form; Discard on a switch drops it.
    fireEvent.click(screen.getByRole("button", { name: "New routine" }));
    expect(screen.queryByTestId("routine-discard-confirm")).toBeNull();
    expect(screen.getByLabelText("Name")).toHaveValue("Half");
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    fireEvent.click(await screen.findByTestId("routine-discard-confirm-confirm"));
    await screen.findByRole("heading", { name: "Standup" });
    expect(screen.queryByTestId("routine-create")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "New routine" }));
    expect(screen.getByLabelText("Task")).toHaveValue("");
  });

  it("tells the setter, under the New routine sentence, what the task controls", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    const help = screen.getByTestId("routine-task-help");
    expect(help).toHaveTextContent("The task is the agent's whole prompt; CodeSense adds nothing");
    expect(help).toHaveTextContent("exits, calls signal_done, or hits its max runtime");
    expect(help).toHaveTextContent("otherwise the run simply ends");
    expect(help).toHaveTextContent("lasts until you close the tab or it hits its max runtime");
    expect(help).toHaveTextContent("routine_context");
    expect(help).toHaveTextContent("Routines never deliver results");
    // Whoever edits an existing routine sees it too, a teammate's included.
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await createEcho();
    expect(screen.getByTestId("routine-task-help")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    expect(screen.getByTestId("routine-task-help")).toBeInTheDocument();
  });

  // Closing the tab while the prompt is up must not leave it armed: a reopened tab with nothing
  // unsaved shows no prompt and stays where it was.
  it("drops a pending discard prompt when the edited Overview goes away", async () => {
    const view = renderTab();
    await createEcho();
    type("Task", "echo half-done");
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    await screen.findByTestId("routine-discard-confirm");

    view.unmount();
    renderTab();
    await screen.findByRole("heading", { name: "Echo" });
    expect(screen.queryByTestId("routine-discard-confirm")).toBeNull();
    expect(screen.getByLabelText("Task")).toHaveValue("echo hi");
  });

  it("saves a draft when enabled is off", async () => {
    renderTab();
    await createEcho(false);
    const mine = screen.getByRole("region", { name: "Mine" });
    const toggle = within(mine).getByRole("switch", { name: "Echo enabled" });
    expect(toggle).not.toBeChecked();
  });

  it("shows a schedule it cannot read, and the backend's refusal, without saving", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    type("Name", "Too often");
    type("Task", "t");
    await pickAgent("Scribe");
    type("When", "3m");
    expect(await screen.findByTestId("routine-preview-error")).toHaveTextContent(
      "unrecognized schedule",
    );
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    expect(await screen.findByTestId("routine-save-error")).toHaveTextContent(
      "unrecognized schedule",
    );
    expect(screen.getByTestId("routine-create")).toBeInTheDocument();
  });

  it("refuses a New routine with no agent picked, and offers no way to type one", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    type("Name", "Sweep");
    type("When", "5m");
    type("Task", "t");
    expect(screen.queryByRole("textbox", { name: "Agent" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    expect(await screen.findByTestId("routine-save-error")).toHaveTextContent("pick an agent");
    expect(calls).not.toContain("routine_save");
    expect(screen.getByTestId("routine-create")).toBeInTheDocument();
  });

  it("offers a saved Hermes agent in the routine form", async () => {
    agents.editAs(
      "acme/backend/triage",
      { runtime: "hermes", permissions: null },
      { kind: "user", name: "Alice" },
    );
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    const slot = screen.getByRole("combobox", { name: "Agent" });
    await waitFor(() => expect(slot).toHaveTextContent("pick an agent"));
    fireEvent.click(slot);
    const option = await screen.findByRole("option", { name: /Triage$/ });
    expect(within(option).getByTestId("agent-runtime").dataset.runtime).toBe("hermes");
    fireEvent.click(option);
    expect(slot).toHaveTextContent("Triage");
  });

  it("offers a New routine your agents and the team's, by name with their runtime, never archived ones", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    // Who can use it: the open team by default; this one is for me only.
    fireEvent.change(screen.getByRole("combobox", { name: "Who can use it" }), {
      target: { value: "only-me" },
    });
    const slot = screen.getByRole("combobox", { name: "Agent" });
    await waitFor(() => expect(slot).toHaveTextContent("pick an agent"));
    fireEvent.click(slot);
    await screen.findByRole("listbox");
    // Templates (builtins) are deliberately absent from the routine picker — a routine is
    // a configured, saved agent; use the Agents tab's New agent dialog to make one from a
    // template first.
    expect(agentGroups()).toEqual([
      ["Mine", ["CodexDev"]],
      ["Team · backend · acme", ["CodexReviewer", "ClaudeScribe", "ClaudeTriage"]],
    ]);
    const badges = within(screen.getByRole("listbox")).getAllByTestId("agent-runtime");
    expect(badges.map((b) => b.dataset.runtime)).toEqual(["codex", "codex", "claude", "claude"]);

    // A personal routine names its own agent by id and a team agent by its key.
    fireEvent.click(screen.getByRole("option", { name: /Dev$/ }));
    expect(slot).toHaveTextContent("Dev");
    await pickAgent("Triage");
    type("Name", "Morning");
    type("When", "5m");
    type("Task", "sweep");
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await screen.findByTestId("routine-overview");
    const created = world.handlers.routine_list({}) as { key: string; def: { agent: string } }[];
    expect(created.find((r) => r.key === "_personal/1/personal/morning")?.def.agent).toBe(
      "acme/backend/triage",
    );
  });

  it("a New routine shows its agent's default mode and saves what it shows (SW-AGR-2)", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    await pickAgent("Triage");
    const mode = screen.getByRole("combobox", { name: "Mode" });
    // Headless by default: a new instance each time.
    expect(mode).toHaveValue("fresh");
    expect(screen.getByTestId("routine-mode-hint")).toHaveTextContent(
      "Starts a new Triage session",
    );
    // In a tab, on a saved agent that launches in a tab: wake, marked as the default.
    fireEvent.click(screen.getByRole("checkbox", { name: "Visible" }));
    expect(mode).toHaveValue("wake");
    expect(screen.getByTestId("routine-mode-hint")).toHaveTextContent("(default for Triage)");
    type("Name", "Waker");
    type("When", "5m");
    type("Task", "sweep");
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await screen.findByTestId("routine-overview");
    const created = world.handlers.routine_list({}) as { key: string; def: { mode?: string } }[];
    expect(created.find((r) => r.key === "acme/backend/waker")?.def.mode).toBe("wake");
  });

  it("a New routine goes to the open team by default (Who can use it)", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    expect(screen.getByRole("combobox", { name: "Who can use it" })).toHaveValue("acme/backend");
    await pickAgent("Triage");
    type("Name", "Weekly");
    type("When", "5m");
    type("Task", "sweep");
    fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
    await screen.findByTestId("routine-overview");
    const created = world.handlers.routine_list({}) as { key: string; scope: string }[];
    expect(created.find((r) => r.key === "acme/backend/weekly")?.scope).toBe("team");
  });

  it("offers a team routine only its team's agents, and keeps an archived current agent", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    const slot = screen.getByRole("combobox", { name: "Agent" });
    await waitFor(() => expect(slot).toHaveTextContent("Scribe"));
    fireEvent.click(slot);
    await screen.findByRole("listbox");
    expect(agentGroups()).toEqual([
      ["Team · backend · acme", ["CodexReviewer", "ClaudeScribe", "ClaudeTriage"]],
    ]);
    // Picking another team agent writes its bare id.
    fireEvent.click(screen.getByRole("option", { name: /Triage$/ }));
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Revision 2");
    const listed = world.handlers.routine_list({}) as { key: string; def: { agent: string } }[];
    expect(listed.find((r) => r.key === "acme/backend/standup")?.def.agent).toBe("triage");

    fireEvent.click(screen.getByRole("button", { name: "Open Release notes" }));
    await screen.findByRole("heading", { name: "Release notes" });
    const archived = screen.getByRole("combobox", { name: "Agent" });
    await waitFor(() => expect(archived).toHaveTextContent("Writer (archived)"));
    fireEvent.click(archived);
    await screen.findByRole("listbox");
    // An archived current agent still appears under Current so the slot shows what the
    // draft holds; the picker still won't offer templates.
    expect(agentGroups()).toEqual([
      ["Current", ["ClaudeWriter (archived)"]],
      ["Team · backend · acme", ["CodexReviewer", "ClaudeScribe", "ClaudeTriage"]],
    ]);

    // Picking an offered team agent switches to it.
    fireEvent.click(screen.getByRole("option", { name: /Triage$/ }));
    await waitFor(() => expect(archived).toHaveTextContent("Triage"));
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => {
      const after = world.handlers.routine_list({}) as { key: string; def: { agent: string } }[];
      expect(after.find((r) => r.key === "acme/backend/release-notes")?.def.agent).toBe("triage");
    });
  });

  it("says why no agent can be picked when the agents can't be read", async () => {
    overrides.agents_list = () => {
      throw new Error("store unreadable");
    };
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    expect(await screen.findByTestId("routine-agents-error")).toHaveTextContent(
      "Couldn't load agents: store unreadable",
    );
    expect(screen.getByRole("combobox", { name: "Agent" })).toHaveTextContent("agents unavailable");
  });

  it("re-picking the agent a routine names by key is no edit", async () => {
    world.agentEdit("acme/backend/standup", "triage-bot", { agent: "acme/backend/scribe" });
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "Agent" })).toHaveTextContent("Scribe"),
    );
    await pickAgent("Scribe");
    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();
  });

  it("links the picked agent to the Agents tab, with how many routines use it", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    const summary = await screen.findByTestId("routine-agent-summary");
    expect(within(summary).getByTestId("agent-runtime").dataset.runtime).toBe("claude");
    expect(within(summary).getByTestId("routine-agent-used-by")).toHaveTextContent(
      "used by 2 routines",
    );
    fireEvent.click(within(summary).getByTestId("routine-agent-open"));
    expect(openTabs).toEqual(["virtual://agents"]);
    expect(useAgentsStore.getState().selectedKey).toBe("acme/backend/scribe");
  });

  it("re-reads who uses an agent when any routine is saved elsewhere, not when a run moves", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    const usedBy = await screen.findByTestId("routine-agent-used-by");
    expect(usedBy).toHaveTextContent("used by 2 routines");
    const reads = () => calls.filter((c) => c === "agents_list").length;
    const before = reads();

    bus.emit(ROUTINES_CHANGED_EVENT, { routine_key: "acme/backend/standup", run_id: "run-1" });
    await new Promise((r) => setTimeout(r, 300));
    expect(reads()).toBe(before);

    // Another routine retargeted onto Scribe through an agent's MCP tools.
    const listed = createMockAgents(new MockEventBus()).handlers.agents_list({
      includeArchived: true,
    }) as AgentRow[];
    overrides.agents_list = () =>
      listed.map((a) =>
        a.key === "acme/backend/scribe"
          ? { ...a, used_by: [...a.used_by, "acme/backend/release-notes"] }
          : a,
      );
    world.agentEdit("acme/backend/release-notes", "triage-bot", { agent: "scribe" });
    await waitFor(() =>
      expect(screen.getByTestId("routine-agent-used-by")).toHaveTextContent("used by 3 routines"),
    );
  });

  it("opens an archived agent with archived agents shown, so the Agents tab can show it", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Release notes" }));
    await screen.findByRole("heading", { name: "Release notes" });
    fireEvent.click(await screen.findByTestId("routine-agent-open"));
    expect(openTabs).toEqual(["virtual://agents"]);
    expect(useAgentsStore.getState().includeArchived).toBe(true);
    expect(useAgentsStore.getState().selectedKey).toBe("acme/backend/writer");
  });

  it("keeps showing a picked agent that went away before the edit was saved", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    await pickAgent("Triage");
    const listed = createMockAgents(new MockEventBus()).handlers.agents_list({
      includeArchived: true,
    }) as AgentRow[];
    overrides.agents_list = () => listed.filter((a) => a.key !== "acme/backend/triage");
    bus.emit(AGENTS_CHANGED_EVENT, { key: "acme/backend/triage" });
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "Agent" })).toHaveTextContent(
        "triage (not found)",
      ),
    );
  });

  it("explains which agents the slot offers instead of hiding the rest silently", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    // A new routine is the team's by default, so it offers the team's agents.
    expect(await screen.findByTestId("routine-agent-offer")).toHaveTextContent(
      "A team routine runs only its own team's agents",
    );
    fireEvent.change(screen.getByRole("combobox", { name: "Who can use it" }), {
      target: { value: "only-me" },
    });
    expect(await screen.findByTestId("routine-agent-offer")).toHaveTextContent(
      "Offers your agents and team backend's; to pick another team's agent, switch to that team.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    expect(await screen.findByTestId("routine-agent-offer")).toHaveTextContent(
      "A team routine runs only its own team's agents",
    );
  });

  it("saves a task its agent cannot start and shows the warning under it", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    type("Task", "/triage-slack today");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Revision 2");
    expect(screen.getByTestId("routine-save-warnings")).toHaveTextContent(
      "this agent doesn't start with /triage-slack — add it to the agent or the run won't find it",
    );
    expect(screen.queryByTestId("routine-save-error")).toBeNull();
    fireEvent.click(screen.getByTestId("routine-warning-open-agent"));
    expect(openTabs).toEqual(["virtual://agents"]);
    expect(useAgentsStore.getState().selectedKey).toBe("acme/backend/scribe");
  });

  it("keeps a routine's agent reference as written when only its task is edited", async () => {
    const agentOf = (key: string) =>
      (world.handlers.routine_list({}) as { key: string; def: { agent: string } }[]).find(
        (r) => r.key === key,
      )?.def.agent;
    const editTask = async (name: string, task: string, revision: string) => {
      fireEvent.click(await screen.findByRole("button", { name: `Open ${name}` }));
      await screen.findByRole("heading", { name });
      await waitFor(() =>
        expect(screen.getByRole("combobox", { name: "Agent" })).not.toHaveTextContent(
          "Loading agents",
        ),
      );
      type("Task", task);
      fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
      await screen.findByText(revision);
    };
    // A full team key stays a key, a bare archived id stays bare.
    world.agentEdit("acme/backend/standup", "triage-bot", { agent: "acme/backend/scribe" });
    renderTab();
    await editTask("Standup", "Notes, keyed.", "Revision 3");
    expect(agentOf("acme/backend/standup")).toBe("acme/backend/scribe");
    await editTask("Release notes", "Draft them.", "Revision 2");
    expect(agentOf("acme/backend/release-notes")).toBe("writer");
  });

  it("keeps an agent nobody can see editable: shown as not found, kept on save until re-picked", async () => {
    world.agentEdit("acme/backend/standup", "triage-bot", { agent: "ghost" });
    const agentOf = () =>
      (world.handlers.routine_list({}) as { key: string; def: { agent: string } }[]).find(
        (r) => r.key === "acme/backend/standup",
      )?.def.agent;
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    const slot = screen.getByRole("combobox", { name: "Agent" });
    await waitFor(() => expect(slot).toHaveTextContent("ghost (not found)"));
    type("Task", "Notes, again.");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Revision 3");
    expect(agentOf()).toBe("ghost");
    expect(screen.queryByTestId("routine-save-error")).toBeNull();
    await pickAgent("Scribe");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Revision 4");
    expect(agentOf()).toBe("scribe");
  });

  it("edits and restores a teammate's routine, crediting each change to whoever made it", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    expect(screen.queryByText(/read-only/i)).toBeNull();
    expect(screen.getByLabelText("When")).toHaveValue("weekday 08:00");
    type("Task", "Notes, with blockers.");
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Revision 2");

    fireEvent.click(screen.getByRole("tab", { name: "History" }));
    await waitFor(() => expect(revisions()).toHaveLength(2));
    expect(revisions().map((r) => [r.revision, r.author?.split(" · ")[0]])).toEqual([
      ["2", "by octocat"],
      ["1", "by sam"],
    ]);
    const first = screen.getAllByTestId("routine-revision")[1];
    fireEvent.click(within(first).getByRole("button", { name: /Restore/ }));
    await waitFor(() => expect(revisions()).toHaveLength(3));
    fireEvent.click(screen.getByRole("tab", { name: "Overview" }));
    await waitFor(() => expect(screen.getByLabelText("Task")).toHaveValue("Notes."));
    // Still Sam's routine: the edits changed its body, not who made it.
    const row = screen
      .getAllByTestId("routine-row")
      .find((r) => r.dataset.routineKey === "acme/backend/standup")!;
    expect(within(row).getByTestId("routine-created-by")).toHaveTextContent("Created by Sam");
  });

  it("opens an agent primed with /routine from the New routine form", async () => {
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    fireEvent.click(screen.getByRole("button", { name: "Describe it to an agent" }));
    await waitFor(() => expect(calls).toContain("routine_open_authoring_agent"));
    expect(screen.queryByTestId("routines-notice")).toBeNull();
  });

  it("keeps a routine's several schedules: the slot is read-only and says why", () => {
    const [standup] = teammates();
    const row = {
      ...standup.row,
      agent_display: { name: "Scribe", archived: false, resolvable: true },
      agent_key: null,
      unassigned: false,
      duplicate: false,
      effective_mode: "fresh" as const,
      mine: true,
      last_run: null,
      needs_you: false,
      needs_you_run: null,
      def: {
        ...standup.row.def,
        triggers: [
          { schedule: { cron: "0 8 * * 1-5", tz: "UTC" } },
          { schedule: { cron: "0 17 * * 5", tz: "UTC" } },
        ],
      },
    };
    render(<RoutineOverview row={row} />);
    expect(screen.getByLabelText("When")).toBeDisabled();
    expect(screen.getByTestId("routine-when-locked")).toHaveTextContent("2 schedules");
    // The rest of the sentence stays editable.
    expect(screen.getByLabelText("Task")).toBeEnabled();
  });

  it("says which events also run a routine, which the sentence does not edit", () => {
    const [standup] = teammates();
    const row = {
      ...standup.row,
      schedule_in_words: "when an agent sits idle with unread mail",
      agent_display: { name: "Scribe", archived: false, resolvable: true },
      agent_key: null,
      unassigned: false,
      duplicate: false,
      effective_mode: "fresh" as const,
      mine: true,
      last_run: null,
      needs_you: false,
      needs_you_run: null,
      def: { ...standup.row.def, triggers: [{ event: { kind: "inbox_idle" } }] },
    };
    render(<RoutineOverview row={row} />);
    expect(screen.getByTestId("routine-event-triggers")).toHaveTextContent(
      "when an agent sits idle with unread mail · ask an agent to change this",
    );
    // No schedule: the When slot is empty but still editable, and a save keeps the event.
    expect(screen.getByLabelText("When")).toBeEnabled();
    expect(screen.getByLabelText("When")).toHaveValue("");
  });

  it("shows no event note for a routine that only has a schedule", () => {
    const [standup] = teammates();
    const row = {
      ...standup.row,
      agent_display: { name: "Scribe", archived: false, resolvable: true },
      agent_key: null,
      unassigned: false,
      duplicate: false,
      effective_mode: "fresh" as const,
      mine: true,
      last_run: null,
      needs_you: false,
      needs_you_run: null,
    };
    render(<RoutineOverview row={row} />);
    expect(screen.queryByTestId("routine-event-triggers")).toBeNull();
  });

  it("opens nothing and says why when the authoring agent cannot open", async () => {
    const hint = "No team is active.";
    overrides.routine_open_authoring_agent = () => ({ opened: false, message: hint });
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
    fireEvent.click(screen.getByRole("button", { name: "Describe it to an agent" }));
    expect(await screen.findByTestId("routines-notice")).toHaveTextContent(hint);
    // The form stays open with whatever was typed.
    expect(screen.getByTestId("routine-create")).toBeInTheDocument();
  });

  it("does not reorder the list when a routine is selected or created", async () => {
    renderTab();
    await createEcho();
    const order = () => screen.getAllByTestId("routine-row").map((r) => r.dataset.routineKey);
    const before = order();
    fireEvent.click(screen.getByRole("button", { name: "Open Standup" }));
    fireEvent.click(screen.getByRole("button", { name: "Open Echo" }));
    expect(order()).toEqual(before);
  });
});

it("creates, inspects, edits and removes a trigger-only metadata routine", async () => {
  renderTab();
  fireEvent.click(await screen.findByRole("button", { name: "New routine" }));
  type("Name", "Start stories");
  type("Task", "Read routine_context and start the story.");
  await pickAgent("Scribe");
  fireEvent.click(screen.getByRole("button", { name: "Add a metadata trigger" }));
  expect(screen.getByLabelText("Field")).toHaveFocus();
  type("Field", "status");
  type("To value", "in-progress");
  type("From value", "planning");
  expect(screen.getByText(/No schedule: runs only on its triggers/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Create routine" }));
  await waitFor(() => expect(!!world.defOf("Start stories")).toBe(true));
  expect(world.defOf("Start stories")!.triggers).toEqual([
    {
      event: {
        kind: "metadata_change",
        metadata: {
          selection: { where: { kinds: ["story"] } },
          target: { key: "status", op: "eq", value: "in-progress" },
          previous: { key: "status", op: "eq", value: "planning" },
        },
      },
    },
  ]);
  await screen.findByTestId("routine-overview");
  expect(screen.getByLabelText("To value")).toHaveValue("in-progress");
  type("To value", "review");
  fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
  await waitFor(() => expect(world.defOf("Start stories")!.revision).toBe(2));
  fireEvent.click(screen.getByRole("button", { name: "Remove trigger 1" }));
  expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();
  type("When", "5m");
  fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
  await waitFor(() =>
    expect(world.defOf("Start stories")!.triggers).toEqual([
      { schedule: { cron: "*/5 * * * *", tz: "America/Denver" } },
    ]),
  );
});

it("uses scoped query suggestions and counts, and blocks an impossible text transition", async () => {
  const condition = {
    selection: { where: { kinds: ["story" as const] } },
    target: { key: "status", op: "eq" as const, value: "in-progress" },
  };
  const queries: Record<string, unknown>[] = [];
  overrides.query_context = (args) => {
    const query = args.query as Record<string, unknown>;
    queries.push(query);
    return {
      schemaVersion: 1,
      total: 2,
      indexing: false,
      generation: 1,
      rows: [
        { relPath: "stories/release/a.md", fields: { status: "planning", qa_signoff: false } },
      ],
      groups: [
        { value: "planning", count: 2 },
        { value: "in-progress", count: 2 },
      ],
    };
  };
  const row = overviewRow();
  row.def.triggers!.push({ event: { kind: "metadata_change", metadata: condition } });
  row.def.triggers!.push({ event: { kind: "inbox_idle" } });
  render(<RoutineOverview row={row} />);
  expect(await screen.findByText(/2 stories already have status in-progress/)).toBeInTheDocument();
  expect(queries[0].where).toMatchObject({ orgs: ["acme"], teams: ["backend"], layers: ["team"] });
  const fields = screen.getByLabelText("Field");
  expect(
    document
      .getElementById(fields.getAttribute("list")!)
      ?.querySelector('option[value="qa_signoff"]'),
  ).not.toBeNull();
  type("From value", "in-progress");
  expect(screen.getByRole("alert")).toHaveTextContent("so this never fires");
  expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();
  expect(screen.getByTestId("routine-event-triggers")).toHaveTextContent(
    "ask an agent to change this",
  );
  fireEvent.click(screen.getByText("Options"));
  const eachMac = within(screen.getByLabelText("Runs")).getByRole("option", {
    name: /each teammate/,
  });
  expect(eachMac).toBeDisabled();
});

it("detects a list from selected query values, words gains and disables From", async () => {
  overrides.query_context = (args) => {
    const query = args.query as { groupBy?: string };
    return {
      schemaVersion: 1,
      total: 1,
      indexing: false,
      generation: 1,
      rows: [{ relPath: "stories/a.md", fields: { tags: ["blocked"] } }],
      groups: query.groupBy ? [{ value: "blocked", count: 1 }] : undefined,
    };
  };
  const row = overviewRow();
  row.def.triggers = [
    {
      event: {
        kind: "metadata_change",
        metadata: {
          selection: { where: { kinds: ["story"] } },
          target: { key: "tags", op: "eq", value: "blocked" },
        },
      },
    },
  ];
  render(<RoutineOverview row={row} />);
  await waitFor(() => expect(screen.getByLabelText("From value")).toBeDisabled());
  expect(screen.getByText("When a story gains blocked in tags.")).toBeInTheDocument();
});

it("disables adding metadata to a machine-armed routine", () => {
  const row = overviewRow();
  row.def.armed_machine = "Studio Mac";
  render(<RoutineOverview row={row} />);
  expect(screen.getByRole("button", { name: "Add a metadata trigger" })).toBeDisabled();
  expect(
    screen.getByText("Routines armed on Studio Mac run on schedules only."),
  ).toBeInTheDocument();
});

it("shows the field change carried by a coalesced history row", () => {
  const row = overviewRow();
  useRoutinesStore.setState({
    runs: {
      [row.key]: [
        {
          run_id: "metadata-merge",
          routine_key: row.key,
          revision: 1,
          source: "event",
          occurrence_key: "metadata",
          phase: "coalesced",
          reason: null,
          linked_run_id: "active",
          session_id: null,
          created_at: Date.now(),
          started_at: null,
          ended_at: null,
          result_text: null,
          agent: null,
          trigger_payload: {
            kind: "metadata_change",
            story_id: "SV-1",
            field: "status",
            previous_present: true,
            previous_value: "planning",
            new_value: "in-progress",
          },
        },
      ],
    },
  });
  render(<RoutineRunsView row={row} now={Date.now()} labels={new Map()} />);
  expect(screen.getByText(/field change/)).toBeInTheDocument();
  expect(screen.getByTestId("routine-run-metadata")).toHaveTextContent(
    "SV-1 · status planning → in-progress",
  );
  expect(screen.getByTestId("routine-run-reason")).toHaveTextContent(
    "Merged into the run already waiting",
  );
});

it("warns about duplicate rows and a folder with no current documents without blocking save", async () => {
  const row = overviewRow();
  const metadata = {
    selection: { where: { kinds: ["story" as const] } },
    target: { key: "status", op: "eq" as const, value: "review" },
  };
  row.def.triggers = [
    { event: { kind: "metadata_change", metadata } },
    { event: { kind: "metadata_change", metadata } },
  ];
  render(<RoutineOverview row={row} />);
  expect(screen.getByText("Same as trigger 1.")).toBeInTheDocument();
  const second = screen.getAllByRole("group")[1];
  fireEvent.change(within(second).getByLabelText("Folder"), {
    target: { value: "stories/future/" },
  });
  expect(
    await screen.findByText(
      "Nothing under stories/future/ yet. Check the folder, or keep it for files to come.",
    ),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled();
});

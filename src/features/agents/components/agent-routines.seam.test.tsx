// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

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
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { MachineSummary } from "@/tauri/commands";

import { resetAgentsStoreForTests } from "../stores/agents-store";
import { AgentsTab } from "./agents-tab";

/**
 * SW-AGR-5: an agent's routines live in its detail (design §4.4–§4.7). Driven through the
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
const REVIEWER = "acme/backend/reviewer";

let MACHINES: MachineSummary[] = [];
const PERSONAL_BOX: MachineSummary = {
  machineId: "devbox-01",
  kind: "ssh",
  label: "devbox-01",
  scope: { kind: "personal" },
  declaration: null,
  binding: null,
  canPublish: true,
  status: { state: "connected", workspaces: 1 },
  attention: 0,
  capabilities: { agentCallbacks: true },
};

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

function boot(
  seed: (now: number) => MockRoutine[] = demo,
  extra: Record<string, (args: Record<string, unknown>) => unknown> = {},
) {
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
    ...extra,
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
  MACHINES = [...ALL_MACHINES, PERSONAL_BOX];
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

const rowOf = (key: string) =>
  screen.getAllByTestId("agent-row").find((r) => r.dataset.agentKey === key)!;

async function openAgent(key: string, name: string) {
  render(
    <TooltipProvider>
      <AgentsTab filePath="virtual://agents" teamContentPath="" />
    </TooltipProvider>,
  );
  await waitFor(() => expect(rowOf(key)).toBeTruthy());
  fireEvent.click(within(rowOf(key)).getByRole("button", { name: `Open ${name}` }));
  return screen.findByTestId("agent-routines");
}

/** The sentence form as it reads: its words, each input's value, each select's chosen
 *  option, and the fixed agent's name (its runtime badge is a label beside it). */
function readSentence(form: HTMLElement): string {
  const words: string[] = [];
  for (const el of Array.from(form.children) as HTMLElement[]) {
    if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) words.push(el.value);
    else if (el instanceof HTMLSelectElement) words.push(el.selectedOptions[0]?.text ?? "");
    else if (el.dataset.testid === "routine-agent-fixed") {
      const badge = el.querySelector("[data-testid=agent-runtime]")?.textContent ?? "";
      words.push((el.textContent ?? "").replace(badge, "").trim());
    } else words.push((el.textContent ?? "").trim());
  }
  return words.filter(Boolean).join(" ");
}

describe("an agent's Routines section (AC1)", () => {
  it("lists each routine as a sentence with its next run and switch; Add routine fixes the agent", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    // Enabled first, by next fire, then the routine that is off.
    expect(rows.map((r) => within(r).getByTestId("agent-routine-name").textContent)).toEqual([
      "Inbox sweep",
      "Morning triage",
      "Weekly retro",
    ]);
    const morning = within(rows[1]);
    expect(morning.getByTestId("agent-routine-sentence").textContent).toBe(
      "Weekdays at 9:00 · wake · “Triage new issues and post what needs an owner.” · on codesense-test-1",
    );
    expect(morning.getByRole("switch", { name: "Morning triage on" })).toBeTruthy();
    expect(within(rows[0]).getByTestId("agent-routine-next").textContent).toMatch(
      /^next .+ · in 1[78]m$/,
    );
    expect(within(rows[2]).getByTestId("agent-routine-next").textContent).toBe("Off");
    expect(within(section).getByRole("heading", { name: /Routines/ }).textContent).toBe(
      "Routines3",
    );

    // The switch is this Mac's switch for the routine.
    fireEvent.click(morning.getByRole("switch", { name: "Morning triage on" }));
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "routine_set_enabled")?.args).toEqual({
        key: "_personal/1/personal/morning-triage",
        enabled: false,
      }),
    );

    // + Add routine opens New routine with Triage fixed, and saves it under Triage.
    fireEvent.click(within(section).getByTestId("agent-routine-add"));
    const page = await screen.findByTestId("routine-page");
    expect(within(page).getByRole("navigation", { name: "Breadcrumb" }).textContent).toContain(
      "Triage/New routine",
    );
    const form = within(page).getByTestId("routine-sentence-form");
    expect(within(form).getByTestId("routine-agent-fixed").textContent).toContain("Triage");
    expect(within(form).queryByRole("combobox", { name: "Agent" })).toBeNull();
    fireEvent.change(within(page).getByLabelText("Name"), { target: { value: "Late check" } });
    fireEvent.change(within(page).getByLabelText("When"), { target: { value: "day 17:00" } });
    fireEvent.change(within(page).getByLabelText("Task"), { target: { value: "check the build" } });
    fireEvent.click(within(page).getByRole("button", { name: "Save routine" }));
    await waitFor(() => {
      const save = calls.find((c) => c.cmd === "routine_save");
      expect(save?.args.form).toMatchObject({ agent: TRIAGE, name: "Late check" });
      expect(save?.args.team).toEqual({ org: "acme", team: "backend" });
    });
  });

  it("past 5 routines it collapses, with Show all N", async () => {
    boot((now) =>
      Array.from({ length: 6 }, (_, i) => {
        const base = demo(now)[5];
        return {
          row: {
            ...base.row,
            key: `acme/backend/r${i}`,
            def: { ...base.row.def, id: `r${i}`, name: `Routine ${i}` },
          },
          runs: [],
        };
      }),
    );
    const section = await openAgent(REVIEWER, "Reviewer");
    await waitFor(() =>
      expect(within(section).getAllByTestId("agent-routine-row")).toHaveLength(5),
    );
    fireEvent.click(within(section).getByRole("button", { name: "Show all 6" }));
    expect(within(section).getAllByTestId("agent-routine-row")).toHaveLength(6);
  });

  it("an agent with none says so and offers one Add routine", async () => {
    boot();
    const section = await openAgent("acme/backend/scribe", "Scribe");
    await waitFor(() =>
      expect(within(section).getAllByTestId("agent-routine-row")).toHaveLength(2),
    );
    cleanup();
    resetAgentsStoreForTests();
    resetRoutinesStoreForTests();
    boot(() => []);
    const empty = await openAgent(TRIAGE, "Triage");
    await within(empty).findByText("No routines yet.");
    expect(within(empty).getAllByRole("button", { name: /Add routine/ })).toHaveLength(1);
  });
});

describe("the sentence form (AC2)", () => {
  it("reads: Every weekday 9:00 wake Triage to … in … on codesense-test-1", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    fireEvent.click(within(rows[1]).getByRole("button", { name: /Morning triage/ }));
    const page = await screen.findByTestId("routine-page");
    const form = await within(page).findByTestId("routine-sentence-form");
    await waitFor(() =>
      expect(readSentence(form)).toBe(
        "Every weekday 09:00 wake Triage to Triage new issues and post what needs an owner. in the team workspace on codesense-test-1",
      ),
    );
    expect(within(page).getByTestId("routine-mode-hint").textContent).toContain(
      "If Triage is running on codesense-test-1, this goes into that session when it's idle.",
    );
    // The fresh word is "start a new", and the machine slot offers any connected machine
    // only for wake.
    const mode = within(form).getByLabelText("Mode") as HTMLSelectElement;
    expect(Array.from(mode.options).map((o) => o.text)).toEqual(["wake", "start a new"]);
    const machine = within(form).getByLabelText("Machine") as HTMLSelectElement;
    expect(Array.from(machine.options).map((o) => o.text)).toEqual([
      "This Mac",
      "codesense-test-1",
      "any connected machine",
    ]);
    fireEvent.change(mode, { target: { value: "fresh" } });
    expect(
      Array.from((within(form).getByLabelText("Machine") as HTMLSelectElement).options).map(
        (o) => o.text,
      ),
    ).not.toContain("any connected machine");

    // Back to Triage: the breadcrumb names the agent, never "Schedule".
    fireEvent.change(mode, { target: { value: "wake" } });
    fireEvent.click(within(page).getByTestId("routine-breadcrumb-back"));
    await screen.findByTestId("agent-routines");
  });

  it("a migrated routine opens on start a new, with the note saying why", async () => {
    boot();
    const section = await openAgent("acme/backend/scribe", "Scribe");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    fireEvent.click(within(rows[0]).getByRole("button", { name: /Standup notes/ }));
    const page = await screen.findByTestId("routine-page");
    expect((within(page).getByLabelText("Mode") as HTMLSelectElement).value).toBe("fresh");
    expect(within(page).getByTestId("routine-mode-hint").textContent).toContain(
      "Set to start a new Scribe each run when this routine moved under Scribe",
    );
  });
});

describe("the next-run chip (AC3)", () => {
  it("Triage's row reads due while its wake is queued on a busy session", async () => {
    boot();
    await openAgent(TRIAGE, "Triage");
    const chip = await screen.findByTestId("agent-next-run");
    expect(chip.dataset.state).toBe("due");
    expect(chip.textContent).toBe("due");
    expect(chip.getAttribute("aria-label")).toMatch(
      /^Morning triage is waiting for Triage to be idle \(until .+\)$/,
    );
    // An agent with only an event routine reads after its trigger; none, no chip.
    expect(screen.getByTestId("agent-routines-link").textContent).toContain("3 routines");
    fireEvent.click(within(rowOf(REVIEWER)).getByRole("button", { name: "Open Reviewer" }));
    await waitFor(() =>
      expect(screen.getByTestId("agent-next-run").textContent).toBe("after Triage"),
    );
  });
});

describe("an unknown run (AC4)", () => {
  it("shows the binding copy and Details offers only Show, never Retry or Run now", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    const sweep = within(rows[0]);
    expect(sweep.getByTestId("run-status-dot").dataset.status).toBe("unknown");
    expect(sweep.getByText(UNKNOWN_COPY)).toBeTruthy();
    fireEvent.click(sweep.getByRole("button", { name: "Details" }));
    const details = await screen.findByTestId("run-details");
    expect(details.textContent).toContain(UNKNOWN_COPY);
    expect(details.textContent).toContain("The paste wasn't confirmed.");
    const buttons = within(details)
      .getAllByRole("button")
      .map((b) => b.textContent || b.getAttribute("aria-label"));
    expect(buttons).toContain("Show Triage's tab");
    expect(buttons.some((b) => /retry|run now/i.test(b ?? ""))).toBe(false);
    fireEvent.click(within(details).getByRole("button", { name: "Show Triage's tab" }));
    expect(results).toEqual([{ run_id: "run_inbo4", sessionId: "s-triage" }]);
  });

  it("Run now after an unknown wake asks to check the tab first", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    fireEvent.click(within(rows[0]).getByRole("button", { name: /Inbox sweep/ }));
    const page = await screen.findByTestId("routine-page");
    fireEvent.click(within(page).getByRole("button", { name: "Run now" }));
    const guard = await screen.findByTestId("routine-run-guard");
    expect(guard.textContent).toContain("Check Triage's tab first");
    expect(guard.textContent).toContain("could paste it twice");
    expect(calls.some((c) => c.cmd === "routine_run_now")).toBe(false);
    fireEvent.click(within(guard).getByRole("button", { name: "Run anyway" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "routine_run_now")).toBe(true));
  });

  it("an undelivered run names its reason and offers Details", async () => {
    boot();
    const section = await openAgent(REVIEWER, "Reviewer");
    const row = within((await within(section).findAllByTestId("agent-routine-row"))[0]);
    expect(row.getByTestId("run-status-dot").dataset.status).toBe("undelivered");
    expect(row.getByTestId("agent-routine-last").textContent).toContain(
      "The workspace on codesense-test-1 isn't prepared yet",
    );
    expect(row.getByRole("button", { name: "Details" })).toBeTruthy();
  });
});

describe("leaving a routine page with an unsaved edit", () => {
  it("asks first, then goes back to the agent", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    fireEvent.click(within(section).getByTestId("agent-routine-add"));
    const page = await screen.findByTestId("routine-page");
    fireEvent.change(within(page).getByLabelText("Name"), { target: { value: "Half done" } });
    await waitFor(() => expect(useRoutinesStore.getState().formDirty).toBe(true));
    fireEvent.click(within(page).getByTestId("routine-breadcrumb-back"));
    const confirm = await screen.findByTestId("routine-discard-confirm");
    act(() => fireEvent.click(within(confirm).getByRole("button", { name: "Discard" })));
    await screen.findByTestId("agent-routines");
  });
});

describe("review r1", () => {
  async function openRoutinePage(agentKey: string, agentName: string, routine: RegExp) {
    const section = await openAgent(agentKey, agentName);
    const rows = await within(section).findAllByTestId("agent-routine-row");
    const row = rows.find((r) => routine.test(r.textContent ?? ""))!;
    fireEvent.click(within(row).getByRole("button", { name: routine }));
    return screen.findByTestId("routine-page");
  }

  it("Cancel wake withdraws a queued wake: the backend's stop is called and due goes away", async () => {
    boot();
    const page = await openRoutinePage(TRIAGE, "Triage", /Morning triage/);
    fireEvent.click(within(page).getByRole("tab", { name: "Runs" }));
    const queued = (await within(page).findAllByTestId("routine-run")).find(
      (r) => r.dataset.phase === "queued",
    )!;
    expect(within(queued).getByTestId("routine-run-reason").textContent).toMatch(
      /^Waiting for Triage to be idle · until .+$/,
    );
    fireEvent.click(within(queued).getByRole("button", { name: "Cancel wake" }));
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "routine_stop_run")?.args).toEqual({
        runId: "run_morn3",
      }),
    );
    fireEvent.click(within(page).getByTestId("routine-breadcrumb-back"));
    await waitFor(() => expect(screen.getByTestId("agent-next-run").dataset.state).toBe("next"));
  });

  it("each run offers its §4.7 action: Run now where a retry can land, never for unknown", async () => {
    boot();
    const page = await openRoutinePage(TRIAGE, "Triage", /Inbox sweep/);
    fireEvent.click(within(page).getByRole("tab", { name: "Runs" }));
    const runs = await within(page).findAllByTestId("routine-run");
    const action = (phase: string) =>
      within(runs.find((r) => r.dataset.phase === phase)!).queryByTestId("routine-run-action")
        ?.textContent;
    expect(action("unknown")).toBe("Show");
    // An older undelivered run offers no Run now: only the latest run may, behind the
    // routine page's guard (here the latest is unknown).
    expect(action("undelivered")).toBeUndefined();
    expect(action("running")).toBe("Show"); // delivered into the session
    expect(action("coalesced")).toBeUndefined();
    const merged = runs.find((r) => r.dataset.phase === "coalesced")!;
    expect(within(merged).getByTestId("routine-run-reason").textContent).toBe(
      "Merged into the wake already waiting",
    );
  });

  it("a wake that needs its workspace prepared offers Prepare…", async () => {
    boot();
    const page = await openRoutinePage(REVIEWER, "Reviewer", /Review after triage/);
    fireEvent.click(within(page).getByRole("tab", { name: "Runs" }));
    const [run] = await within(page).findAllByTestId("routine-run");
    expect(within(run).getByTestId("routine-run-action").textContent).toBe("Prepare…");
  });

  it("Details names the tab the wake went into", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    fireEvent.click(within(rows[0]).getByRole("button", { name: "Details" }));
    const into = await screen.findByTestId("run-details-into");
    expect(into.textContent).toContain("Triage · This Mac");
  });

  it("a routine that went away says so instead of a blank form", async () => {
    boot();
    await openAgent(TRIAGE, "Triage");
    act(() => {
      useRoutinesStore.setState({ routineOwner: TRIAGE, selectedKey: "acme/backend/gone" });
    });
    expect((await screen.findByTestId("routine-page-missing")).textContent).toBe(
      "This routine was archived or moved.",
    );
    expect(screen.queryByTestId("routine-sentence-form")).toBeNull();
  });

  it("a machine problem blocks Save until it is fixed", async () => {
    // This team doesn't share codesense-test-1 any more.
    MACHINES = [ALL_MACHINES[0]];
    boot();
    const page = await openRoutinePage(TRIAGE, "Triage", /Morning triage/);
    await waitFor(() =>
      expect(within(page).getByRole("alert").textContent).toBe(
        "Pick a machine. codesense-test-1 isn't shared with this team any more.",
      ),
    );
    fireEvent.change(within(page).getByLabelText("Task"), { target: { value: "changed" } });
    expect(within(page).getByRole("button", { name: "Save changes" })).toBeDisabled();
    fireEvent.change(within(page).getByLabelText("Machine"), { target: { value: "local" } });
    expect(within(page).queryByRole("alert")).toBeNull();
    expect(within(page).getByRole("button", { name: "Save changes" })).toBeEnabled();
  });

  it("each teammate's Mac sets the machine to This Mac", async () => {
    boot();
    const page = await openRoutinePage(TRIAGE, "Triage", /Inbox sweep/);
    fireEvent.change(within(page).getByLabelText("Machine"), {
      target: { value: "codesense-test-1" },
    });
    fireEvent.click(within(page).getByText("Options"));
    fireEvent.change(within(page).getByLabelText("Runs"), { target: { value: "each_mac" } });
    expect(within(page).getByTestId("routine-machine").textContent).toBe("each teammate's Mac");
    expect(within(page).queryByRole("alert")).toBeNull();
  });

  it("the max wait reads as fixed here, and the migrated note says start a new", async () => {
    boot();
    const page = await openRoutinePage(TRIAGE, "Triage", /Morning triage/);
    expect(within(page).getByTestId("routine-max-wait").textContent).toBe(
      "30m · can't be changed here yet",
    );
    cleanup();
    resetAgentsStoreForTests();
    resetRoutinesStoreForTests();
    boot();
    const standup = await openRoutinePage("acme/backend/scribe", "Scribe", /Standup notes/);
    expect(within(standup).getByTestId("routine-sentence").textContent).toContain("start a new");
    expect(within(standup).getByTestId("routine-mode-hint").textContent).toContain(
      "Set to start a new Scribe each run when this routine moved under Scribe",
    );
  });

  it("Escape returns to the agent and focus to the row that opened the page", async () => {
    boot();
    const page = await openRoutinePage(TRIAGE, "Triage", /Inbox sweep/);
    fireEvent.keyDown(within(page).getByRole("heading", { name: "Inbox sweep" }), {
      key: "Escape",
    });
    await screen.findByTestId("agent-routines");
    await waitFor(() =>
      expect(document.activeElement?.getAttribute("data-routine-open")).toBe(
        "acme/backend/inbox-sweep",
      ),
    );
  });
});

describe("review r2", () => {
  async function page(agentKey: string, agentName: string, routine: RegExp) {
    const section = await openAgent(agentKey, agentName);
    const rows = await within(section).findAllByTestId("agent-routine-row");
    const row = rows.find((r) => routine.test(r.textContent ?? ""))!;
    fireEvent.click(within(row).getByRole("button", { name: routine }));
    return screen.findByTestId("routine-page");
  }

  it("the latest undelivered run offers Run now", async () => {
    boot((now) =>
      demo(now).map((r) =>
        r.row.def.id === "morning-triage"
          ? {
              row: { ...r.row, queued: null },
              runs: r.runs.filter((x) => x.phase === "undelivered"),
            }
          : r,
      ),
    );
    const p = await page(TRIAGE, "Triage", /Morning triage/);
    fireEvent.click(within(p).getByRole("tab", { name: "Runs" }));
    const [run] = await within(p).findAllByTestId("routine-run");
    expect(within(run).getByTestId("routine-run-action").textContent).toBe("Run now");
  });

  it("a personal agent's routine may name the user's own machines; a team agent's may not", async () => {
    boot();
    const dev = await page("_personal/1/personal/dev", "Dev", /PR sweeper/);
    fireEvent.change(within(dev).getByLabelText("Mode"), { target: { value: "wake" } });
    expect(
      Array.from((within(dev).getByLabelText("Machine") as HTMLSelectElement).options).map(
        (o) => o.text,
      ),
    ).toContain("devbox-01");
    cleanup();
    resetAgentsStoreForTests();
    resetRoutinesStoreForTests();
    boot();
    const triage = await page(TRIAGE, "Triage", /Morning triage/);
    expect(
      Array.from((within(triage).getByLabelText("Machine") as HTMLSelectElement).options).map(
        (o) => o.text,
      ),
    ).not.toContain("devbox-01");
  });

  it("a queued wake says until once", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    const morning = rows.find((r) => r.textContent?.includes("Morning triage"))!;
    expect(morning.textContent?.match(/until/g)).toHaveLength(1);
  });
});

describe("review r3", () => {
  it("a remote fresh run names the machine it started on, in the row and in Runs", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    const retro = rows.find((r) => r.textContent?.includes("Weekly retro"))!;
    expect(within(retro).getByTestId("agent-routine-launch-machine").textContent).toBe(
      "on codesense-test-1",
    );
    fireEvent.click(within(retro).getByRole("button", { name: /Weekly retro/ }));
    const page = await screen.findByTestId("routine-page");
    fireEvent.click(within(page).getByRole("tab", { name: "Runs" }));
    const [run] = await within(page).findAllByTestId("routine-run");
    expect(within(run).getByTestId("routine-run-reason").textContent).toBe(
      "Started a new Triage session on codesense-test-1",
    );
  });

  it("start a new may run on another machine now (SW-AGR-2b)", async () => {
    boot();
    const section = await openAgent(TRIAGE, "Triage");
    const rows = await within(section).findAllByTestId("agent-routine-row");
    const retro = rows.find((r) => r.textContent?.includes("Weekly retro"))!;
    fireEvent.click(within(retro).getByRole("button", { name: /Weekly retro/ }));
    const page = await screen.findByTestId("routine-page");
    const machine = within(page).getByLabelText("Machine") as HTMLSelectElement;
    expect(Array.from(machine.options).every((o) => !o.disabled)).toBe(true);
    fireEvent.change(machine, { target: { value: "codesense-test-1" } });
    expect(within(page).queryByRole("alert")).toBeNull();
    fireEvent.change(within(page).getByLabelText("Task"), { target: { value: "draft it" } });
    expect(within(page).getByRole("button", { name: "Save changes" })).toBeEnabled();
  });
});

describe("a machine-armed routine out of date on its machine (SW-XS-25)", () => {
  const staleSeed =
    (why: string) =>
    (now: number): MockRoutine[] =>
      demo(now).map((r) =>
        r.row.def.name === "Morning triage"
          ? {
              ...r,
              row: {
                ...r.row,
                def: { ...r.row.def, armed_machine: "codesense-test-1" },
                content_stale: { machine: "codesense-test-1", since_ms: now - 60_000, why },
              },
            }
          : r,
      );
  const morningOf = async (section: HTMLElement) =>
    (await within(section).findAllByTestId("agent-routine-row")).find(
      (r) => within(r).getByTestId("agent-routine-name").textContent === "Morning triage",
    )!;

  it("shows why with Refresh, which prepares it there and clears the badge", async () => {
    boot(staleSeed("changed since it was prepared on the machine"));
    const section = await openAgent(TRIAGE, "Triage");
    const morning = await morningOf(section);
    expect(within(section).getAllByTestId("routine-stale")).toHaveLength(1);
    const stale = within(morning).getByTestId("routine-stale");
    expect(within(stale).getByText("Out of date on codesense-test-1")).toBeTruthy();
    expect(within(stale).getByTestId("routine-stale-why").textContent).toBe(
      "Changed since it was prepared there; codesense-test-1 runs the earlier version.",
    );

    fireEvent.click(
      within(stale).getByRole("button", { name: "Refresh Morning triage on codesense-test-1" }),
    );
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "routine_refresh_on_machine")?.args).toEqual({
        key: morning.dataset.routineKey,
      }),
    );
    await waitFor(() => expect(within(section).queryByTestId("routine-stale")).toBeNull());
  });

  it("never prepared there: says so, without claiming an earlier version runs", async () => {
    boot(staleSeed("codesense-test-1 is not connected"));
    const section = await openAgent(TRIAGE, "Triage");
    const stale = within(await morningOf(section)).getByTestId("routine-stale");
    expect(within(stale).getByTestId("routine-stale-why").textContent).toBe(
      "Codesense-test-1 is not connected.",
    );
    expect(stale.textContent).not.toContain("earlier version");
  });

  it("a refresh that fails says why, and the badge stays", async () => {
    boot(staleSeed("changed since it was prepared on the machine"), {
      routine_refresh_on_machine: () => {
        throw new Error("codesense-test-1 is not connected");
      },
    });
    const section = await openAgent(TRIAGE, "Triage");
    const stale = within(await morningOf(section)).getByTestId("routine-stale");
    fireEvent.click(within(stale).getByRole("button", { name: /^Refresh Morning triage/ }));
    expect(await screen.findByText(/codesense-test-1 is not connected/)).toBeTruthy();
    expect(useRoutinesStore.getState().actionError).toContain("not connected");
    expect(within(section).getAllByTestId("routine-stale")).toHaveLength(1);
  });
});

it("opens a saved agent directly in its editor from another detail view, including archived definitions", async () => {
  boot();
  await openAgent(REVIEWER, "Reviewer");
  const { useAgentsStore } = await import("../stores/agents-store");
  const { openAgentEditor } = await import("../agents-virtual-tab");
  act(() => useAgentsStore.getState().setView("running"));
  act(() => openAgentEditor(REVIEWER, true));
  expect(useAgentsStore.getState().includeArchived).toBe(true);
  expect(useAgentsStore.getState().view).toBe("edit");
  expect(await screen.findByTestId("agent-editor")).toBeVisible();
});

// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";
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
import type { RoutineRun } from "@/tauri/commands";

import { resetRoutinesStoreForTests, useRoutinesStore } from "../stores/routines-store";
import { IdentityMenu } from "@/features/shell/components/identity-menu";
import { RoutinesTab } from "./routines-tab";

/**
 * The Routines tab driven through the same door the app uses
 * (`window.__TAURI_INTERNALS__`), over the RF-29 mock routines backend: every
 * read is a real command round-trip and every update arrives as a
 * `routines:changed` event.
 */

const KEY = "acme/backend/digest";

function digest(): MockRoutine[] {
  return [
    {
      row: {
        key: KEY,
        scope: "team",
        armed_here: true,
        owner_display: "Alice",
        enabled: true,
        created_by: { kind: "agent", name: "triage-bot" },
        schedule_in_words: "every 5 minutes",
        next_run_at: Date.now() + 3 * 3_600_000 + 1_000,
        def: {
          id: "digest",
          name: "Digest",
          owner: "1",
          agent: "dev",
          task: "Summarize what changed.",
          where: { kind: "team_workspace" },
          policy: { overlap: "coalesce", missed: "skip", max_runtime: "30m", visible: false },
          revision: 1,
          armed_host: "this-mac",
        },
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: true,
      },
      runs: [],
    },
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
          policy: { overlap: "coalesce", missed: "skip", visible: false },
          revision: 1,
          armed_host: "sams-mac",
        },
        agent_key: null,
        unassigned: false,
        duplicate: false,
        effective_mode: "fresh",
        mine: false,
      },
      runs: [],
    },
  ];
}

let world: MockRoutines;

beforeEach(() => {
  resetRoutinesStoreForTests();
  const events = new MockEventBus();
  world = createMockRoutines(events, { seed: digest, missed: 2, runMs: null });
  const internals = buildTauriInternals(
    createInvokeRouter({ ...events.handlers(), ...world.handlers }),
  );
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
});

afterEach(() => {
  cleanup();
  resetRoutinesStoreForTests();
});

function renderTab() {
  return render(
    <TooltipProvider>
      <IdentityMenu />
      <RoutinesTab filePath="virtual://routines" teamContentPath="" />
    </TooltipProvider>,
  );
}

const digestRow = () =>
  screen.getAllByTestId("routine-row").find((r) => r.dataset.routineKey === KEY)!;

const runRows = () => screen.queryAllByTestId("routine-run");

/** Open the Digest routine on its Runs view. */
function openDigestRuns(open: HTMLElement) {
  fireEvent.click(open);
  fireEvent.click(screen.getByRole("tab", { name: "Runs" }));
}

function onlyRun(): HTMLElement {
  const rows = runRows();
  expect(rows).toHaveLength(1);
  return rows[0];
}

describe("Routines tab over the mock backend", () => {
  it("shows only one agent's routines under its removable filter chip", async () => {
    useRoutinesStore
      .getState()
      .setAgentFilter({ agentKey: "acme/backend/dev", name: "Dev", routineKeys: [KEY] });
    renderTab();
    await waitFor(() => expect(screen.getAllByTestId("routine-row")).toHaveLength(1));
    expect(screen.getByTestId("routine-row").dataset.routineKey).toBe(KEY);
    const chip = screen.getByTestId("routine-agent-filter");
    expect(chip.textContent).toContain("Using Dev");
    fireEvent.click(within(chip).getByRole("button"));
    await waitFor(() => expect(screen.getAllByTestId("routine-row")).toHaveLength(2));
    expect(screen.queryByTestId("routine-agent-filter")).toBeNull();
  });

  it("lists a routine whose agent is gone under Unassigned, first, with how to fix it", async () => {
    resetRoutinesStoreForTests();
    const events = new MockEventBus();
    const seeded = (): MockRoutine[] => {
      const [first, second] = digest();
      second.row = { ...second.row, unassigned: true, next_run_at: null };
      return [first, second];
    };
    world = createMockRoutines(events, { seed: seeded, missed: 0, runMs: null });
    const internals = buildTauriInternals(
      createInvokeRouter({ ...events.handlers(), ...world.handlers }),
    );
    events.bind(internals.runCallback);
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
    renderTab();
    const unassigned = await screen.findByRole("region", { name: "Unassigned" });
    expect(within(unassigned).getByText("Standup")).toBeInTheDocument();
    expect(within(unassigned).getByTestId("routine-group-note")).toHaveTextContent("pick an agent");
    const regions = screen.getAllByRole("region").map((r) => r.getAttribute("aria-label"));
    expect(regions.indexOf("Unassigned")).toBeLessThan(regions.indexOf("Mine"));
    expect(screen.queryByRole("region", { name: "Team" })).toBeNull();
  });

  it("shows what a run does, a migration rename note and a duplicate id in the detail", async () => {
    resetRoutinesStoreForTests();
    const events = new MockEventBus();
    const seeded = (): MockRoutine[] => {
      const [first, second] = digest();
      first.row.def = {
        ...first.row.def,
        migrated: { from: "routines/digest", note: "Renamed from digest when it moved" },
      };
      second.row = { ...second.row, duplicate: true, next_run_at: null };
      return [first, second];
    };
    world = createMockRoutines(events, { seed: seeded, missed: 0, runMs: null });
    const internals = buildTauriInternals(
      createInvokeRouter({ ...events.handlers(), ...world.handlers }),
    );
    events.bind(internals.runCallback);
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
    renderTab();
    fireEvent.click(await screen.findByRole("button", { name: "Open Digest" }));
    await screen.findByRole("heading", { name: "Digest" });
    expect(screen.getByTestId("routine-mode-line")).toHaveTextContent(
      "Each run starts a fresh session of the agent.",
    );
    expect(screen.getByTestId("routine-migration-note")).toHaveTextContent(
      "Renamed from digest when it moved",
    );
    expect(screen.queryByTestId("routine-duplicate")).toBeNull();
    const held = screen.getByRole("region", { name: "Unassigned" });
    fireEvent.click(within(held).getByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    expect(screen.getByTestId("routine-duplicate")).toHaveTextContent(
      "Another routine already has the id standup",
    );
  });

  it("marks an archived agent and warns, with the raw id, about one that does not resolve", async () => {
    resetRoutinesStoreForTests();
    const events = new MockEventBus();
    const seeded = (): MockRoutine[] => {
      const [first, second] = digest();
      first.row.def = { ...first.row.def, agent: "writer" };
      second.row.def = { ...second.row.def, agent: "acme/backend/gone" };
      return [first, second];
    };
    world = createMockRoutines(events, { seed: seeded, missed: 0, runMs: null });
    const internals = buildTauriInternals(
      createInvokeRouter({ ...events.handlers(), ...world.handlers }),
    );
    events.bind(internals.runCallback);
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
    renderTab();
    const mine = await screen.findByRole("region", { name: "Mine" });
    const team = screen.getByRole("region", { name: "Team" });
    expect(within(mine).getByText(/· Writer \(archived\) ·/)).toBeInTheDocument();
    expect(within(mine).queryByTestId("routine-agent-unavailable")).toBeNull();
    expect(within(team).getByTestId("routine-agent-unavailable")).toHaveTextContent(
      "Agent unavailable: acme/backend/gone",
    );
    fireEvent.click(within(team).getByRole("button", { name: "Open Standup" }));
    await screen.findByRole("heading", { name: "Standup" });
    expect(screen.getAllByTestId("routine-agent-unavailable")).toHaveLength(2);
  });

  it("groups the user's and a teammate's routines, with who made each and where it runs", async () => {
    renderTab();
    const mine = await screen.findByRole("region", { name: "Mine" });
    const team = screen.getByRole("region", { name: "Team" });
    expect(screen.queryByText(/read-only/i)).toBeNull();
    expect(within(mine).getByText("Digest")).toBeInTheDocument();
    expect(within(mine).getByTestId("routine-created-by")).toHaveTextContent(
      "Created by triage-bot",
    );
    expect(within(mine).getByTestId("routine-next-run")).toHaveTextContent("Next in 3h");
    // The schedule opens its line in sentence case, in the list and the detail alike.
    // The agent reads by its name, resolved by the backend.
    expect(within(mine).getByText(/^Every 5 minutes · Dev · Team workspace$/)).toBeInTheDocument();
    expect(within(team).getByText(/^Weekdays at 8:00 · Scribe/)).toBeInTheDocument();
    fireEvent.click(within(mine).getByRole("button", { name: "Open Digest" }));
    expect(await screen.findByTestId("routine-sentence")).toHaveTextContent(
      "Every 5 minutes, Dev runs in Team workspace.",
    );
    expect(within(mine).getByRole("switch", { name: "Digest enabled" })).toBeChecked();
    const teammate = () =>
      within(screen.getByRole("region", { name: "Team" })).getByTestId("routine-row");
    expect(within(teammate()).getByTestId("routine-created-by")).toHaveTextContent(
      "Created by Sam",
    );
    expect(within(teammate()).getByTestId("routine-machine")).toHaveTextContent(
      "Runs once for the team · on another Mac",
    );
    // It fires on another Mac, so it is paused there; anyone can move it here.
    expect(within(teammate()).queryByRole("switch")).toBeNull();
    // A run by hand runs on this Mac wherever it is armed.
    expect(within(teammate()).getByRole("button", { name: /Run now/ })).toBeEnabled();
    expect(within(teammate()).getByRole("button", { name: "Copy to my routines" })).toBeEnabled();
    fireEvent.click(within(teammate()).getByRole("button", { name: "Run on this Mac" }));
    await waitFor(() =>
      expect(within(teammate()).getByRole("switch", { name: "Standup enabled" })).toBeChecked(),
    );
    expect(within(teammate()).getByTestId("routine-machine")).toHaveTextContent(
      "Runs once for the team · on this Mac",
    );
    expect(within(teammate()).getByRole("button", { name: /Run now/ })).toBeEnabled();
    // Still Sam's: moving it changes where it runs, not who made it.
    expect(within(teammate()).getByTestId("routine-created-by")).toHaveTextContent(
      "Created by Sam",
    );
  });

  it("shows a routine that names no Mac as running nowhere until someone picks one", async () => {
    resetRoutinesStoreForTests();
    const events = new MockEventBus();
    world = createMockRoutines(events, {
      seed: () => {
        const [, standup] = digest();
        standup.row.def = { ...standup.row.def, armed_host: "" };
        return [standup];
      },
      missed: 0,
      runMs: null,
    });
    const internals = buildTauriInternals(
      createInvokeRouter({ ...events.handlers(), ...world.handlers }),
    );
    events.bind(internals.runCallback);
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
    renderTab();
    const row = async () =>
      within(await screen.findByRole("region", { name: "Team" })).getByTestId("routine-row");
    expect(within(await row()).getByTestId("routine-machine")).toHaveTextContent(
      "Runs once for the team · not running anywhere",
    );
    expect(within(await row()).queryByTestId("routine-next-run")).toBeNull();
    expect(within(await row()).queryByRole("switch")).toBeNull();
    fireEvent.click(within(await row()).getByRole("button", { name: "Run on this Mac" }));
    await waitFor(async () =>
      expect(within(await row()).getByRole("switch", { name: "Standup enabled" })).toBeChecked(),
    );
    expect(within(await row()).getByTestId("routine-machine")).toHaveTextContent(
      "Runs once for the team · on this Mac",
    );
  });

  it("turns a recipe each teammate's Mac runs on here, and switches a routine's mode by editing it", async () => {
    resetRoutinesStoreForTests();
    const events = new MockEventBus();
    world = createMockRoutines(events, {
      seed: () => {
        const [, standup] = digest();
        const inbox = digest()[1];
        inbox.row = {
          ...inbox.row,
          key: "acme/backend/inbox",
          armed_here: true,
          enabled: false,
          def: {
            ...inbox.row.def,
            id: "inbox",
            name: "Inbox",
            armed_host: "",
            runs_on: "each_mac",
          },
        };
        return [standup, inbox];
      },
      missed: 0,
      runMs: null,
    });
    const internals = buildTauriInternals(
      createInvokeRouter({ ...events.handlers(), ...world.handlers }),
    );
    events.bind(internals.runCallback);
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
    renderTab();
    const rowOf = async (key: string) =>
      (await screen.findAllByTestId("routine-row")).find((r) => r.dataset.routineKey === key)!;
    const inbox = () => rowOf("acme/backend/inbox");

    // Off here until this Mac turns it on; it is never moved, only switched.
    expect(within(await inbox()).getByTestId("routine-machine")).toHaveTextContent(
      "Each teammate's Mac · off here",
    );
    expect(within(await inbox()).getByRole("switch", { name: "Run on my Mac" })).not.toBeChecked();
    expect(within(await inbox()).getByText("Run on my Mac")).toBeInTheDocument();
    expect(within(await inbox()).queryByRole("button", { name: "Run on this Mac" })).toBeNull();
    expect(
      within(await inbox()).getByRole("button", { name: "Copy to my routines" }),
    ).toBeEnabled();
    // A run by hand while it is off here says how to make it run here.
    const runNowButton = within(await inbox()).getByRole("button", { name: /Run now/ });
    expect(runNowButton).toHaveAttribute("title", "Turn on for this Mac to run here");
    fireEvent.click(runNowButton);
    await waitFor(() =>
      expect(screen.getByTestId("routines-notice")).toHaveTextContent(
        "Inbox didn't run: Skipped — off on this Mac. Turn on for this Mac to run here.",
      ),
    );
    fireEvent.click(within(await inbox()).getByRole("switch", { name: "Run on my Mac" }));
    await waitFor(async () =>
      expect(within(await inbox()).getByTestId("routine-machine")).toHaveTextContent(
        "Each teammate's Mac · on here",
      ),
    );
    expect(within(await inbox()).getByRole("switch", { name: "Run on my Mac" })).toBeChecked();
    expect(within(await inbox()).getByRole("button", { name: /Run now/ })).not.toHaveAttribute(
      "title",
    );

    // Sam's shared job becomes a recipe by an ordinary edit: it then starts
    // off here, since it ran on another Mac.
    const standup = () => rowOf("acme/backend/standup");
    fireEvent.click(within(await standup()).getByRole("button", { name: "Open Standup" }));
    fireEvent.change(await screen.findByRole("combobox", { name: "Runs" }), {
      target: { value: "each_mac" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(async () =>
      expect(within(await standup()).getByTestId("routine-machine")).toHaveTextContent(
        "Each teammate's Mac · off here",
      ),
    );
    // And back to a shared job: it runs on this Mac, the one that changed it.
    fireEvent.change(screen.getByRole("combobox", { name: "Runs" }), {
      target: { value: "one_mac" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(async () =>
      expect(within(await standup()).getByTestId("routine-machine")).toHaveTextContent(
        "Runs once for the team · on this Mac",
      ),
    );
    expect(within(await standup()).getByRole("switch", { name: "Standup enabled" })).toBeChecked();

    // Paused here, it stays off when it becomes a recipe again.
    fireEvent.click(within(await standup()).getByRole("switch", { name: "Standup enabled" }));
    await waitFor(async () =>
      expect(
        within(await standup()).getByRole("switch", { name: "Standup enabled" }),
      ).not.toBeChecked(),
    );
    fireEvent.change(screen.getByRole("combobox", { name: "Runs" }), {
      target: { value: "each_mac" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(async () =>
      expect(within(await standup()).getByTestId("routine-machine")).toHaveTextContent(
        "Each teammate's Mac · off here",
      ),
    );
  });

  it("shows the always-on strip with the missed count until it is dismissed", async () => {
    renderTab();
    const strip = screen.getByTestId("routines-status-strip");
    expect(strip).toHaveTextContent("Routines on This Mac run while CodeSense is open");
    await waitFor(() =>
      expect(strip).toHaveTextContent("· 2 missed while closed or asleep (skipped)"),
    );
    fireEvent.click(screen.getByTestId("routines-missed-dismiss"));
    await waitFor(() => expect(screen.queryByTestId("routines-missed")).toBeNull());
    expect(strip).toHaveTextContent("Routines on This Mac run while CodeSense is open");
  });

  it("runs, stops, skips and needs you, live, without reordering the list", async () => {
    renderTab();
    await screen.findByText("Digest");
    const order = () => screen.getAllByTestId("routine-row").map((r) => r.dataset.routineKey);
    const initialOrder = order();
    openDigestRuns(within(digestRow()).getByRole("button", { name: "Open Digest" }));
    await screen.findByText("No runs yet.");

    // Run now: a running row with Stop.
    fireEvent.click(within(digestRow()).getByRole("button", { name: /Run now/ }));
    await waitFor(() => expect(onlyRun().dataset.phase).toBe("running"));
    fireEvent.click(within(onlyRun()).getByRole("button", { name: /Stop/ }));
    await waitFor(() => expect(onlyRun().dataset.phase).toBe("cancelled"));
    expect(within(onlyRun()).queryByRole("button", { name: /Stop/ })).toBeNull();

    // Paused, a manual fire is recorded as a skip with its reason in words.
    fireEvent.click(within(digestRow()).getByRole("switch", { name: "Digest enabled" }));
    await waitFor(() =>
      expect(within(digestRow()).getByRole("switch", { name: "Digest enabled" })).not.toBeChecked(),
    );
    fireEvent.click(within(digestRow()).getByRole("button", { name: /Run now/ }));
    await waitFor(() => expect(runRows()).toHaveLength(2));
    expect(runRows()[0].dataset.phase).toBe("skipped");
    expect(within(runRows()[0]).getByTestId("routine-run-reason")).toHaveTextContent(
      "Skipped — paused",
    );
    expect(screen.getByTestId("routines-notice")).toHaveTextContent(
      "Digest didn't run: Skipped — paused.",
    );
    fireEvent.click(within(digestRow()).getByRole("switch", { name: "Digest enabled" }));

    // A run that ends needing you: the row says so and the identity menu's dot counts it.
    expect(screen.queryByTestId("identity-menu-attention")).toBeNull();
    await waitFor(() =>
      expect(within(digestRow()).getByRole("switch", { name: "Digest enabled" })).toBeChecked(),
    );
    fireEvent.click(within(digestRow()).getByRole("button", { name: /Run now/ }));
    await waitFor(() => expect(runRows()[0]?.dataset.phase).toBe("running"));
    const asking = runRows()[0].dataset.runId as string;
    act(() => world.finishRun(asking, "needs_you", "Which repo should I sweep?"));
    await waitFor(() => expect(screen.getByTestId("identity-menu-attention")).toBeInTheDocument());
    expect(within(digestRow()).getByTestId("routine-needs-you")).toHaveTextContent("Needs you");
    expect(within(digestRow()).getByTestId("routine-needs-you-text")).toHaveTextContent(
      "Which repo should I sweep?",
    );
    expect(within(runRows()[0]).getByTestId("routine-run-result")).toHaveTextContent(
      "Which repo should I sweep?",
    );
    expect(screen.getByTestId("identity-menu-trigger")).toHaveAccessibleName(/— 1 needs you$/);

    // Clicking the result opens the run detail.
    fireEvent.click(within(runRows()[0]).getByTestId("routine-run-result"));
    const opened = await screen.findByTestId("routine-open-run");
    expect(opened).toHaveTextContent("Digest · Needs you");

    // A run whose agent simply exits reads "Ended" with no result, and the
    // question still stands.
    fireEvent.click(within(digestRow()).getByRole("button", { name: /Run now/ }));
    await waitFor(() => expect(runRows()[0]?.dataset.phase).toBe("running"));
    act(() => world.finishRun(runRows()[0].dataset.runId as string, "ended", null));
    await waitFor(() => expect(runRows()[0]?.dataset.phase).toBe("ended"));
    expect(runRows()[0]).toHaveTextContent("Ended");
    expect(within(runRows()[0]).queryByTestId("routine-run-result")).toBeNull();
    expect(within(runRows()[0]).queryByTestId("routine-run-reason")).toBeNull();
    expect(screen.getByTestId("identity-menu-trigger")).toHaveAccessibleName(/— 1 needs you$/);
    expect(within(digestRow()).getByTestId("routine-needs-you")).toHaveTextContent("Needs you");

    // The next run that ends any other way clears both.
    fireEvent.click(within(digestRow()).getByRole("button", { name: /Run now/ }));
    await waitFor(() => expect(runRows()[0]?.dataset.phase).toBe("running"));
    act(() => world.finishRun(runRows()[0].dataset.runId as string, "succeeded", "All clear."));
    await waitFor(() => expect(screen.queryByTestId("identity-menu-attention")).toBeNull());
    expect(within(digestRow()).queryByTestId("routine-needs-you")).toBeNull();

    expect(order()).toEqual(initialOrder);
    const phases = runRows().map((r) => r.dataset.phase as RoutineRun["phase"]);
    expect(phases).toEqual(["succeeded", "ended", "needs_you", "skipped", "cancelled"]);
  });

  it("keeps older runs the user paged in when a change re-reads the runs", async () => {
    resetRoutinesStoreForTests();
    const events = new MockEventBus();
    const many = (): MockRoutine[] => {
      const [first] = digest();
      first.runs = Array.from({ length: 70 }, (_, i) => ({
        run_id: `old-${String(70 - i).padStart(2, "0")}`,
        routine_key: KEY,
        revision: 1,
        source: "schedule",
        occurrence_key: null,
        phase: "succeeded" as const,
        reason: null,
        linked_run_id: null,
        session_id: null,
        created_at: 1_000 + (70 - i),
        started_at: 1_000 + (70 - i),
        ended_at: 1_000 + (70 - i),
        result_text: null,
        agent: null,
      }));
      return [first];
    };
    world = createMockRoutines(events, { seed: many, missed: 0, runMs: null });
    const internals = buildTauriInternals(
      createInvokeRouter({ ...events.handlers(), ...world.handlers }),
    );
    events.bind(internals.runCallback);
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;

    renderTab();
    openDigestRuns(await screen.findByRole("button", { name: "Open Digest" }));
    await waitFor(() => expect(runRows()).toHaveLength(50));
    fireEvent.click(screen.getByRole("button", { name: "Show older runs" }));
    await waitFor(() => expect(runRows()).toHaveLength(70));
    // A new run arrives; the re-read keeps as many runs as were shown, newest
    // first, instead of folding back to the first page.
    fireEvent.click(within(digestRow()).getByRole("button", { name: /Run now/ }));
    await waitFor(() => expect(runRows()[0]?.dataset.phase).toBe("running"));
    await new Promise((r) => setTimeout(r, 400));
    expect(runRows()).toHaveLength(70);
    expect(runRows()[69].dataset.runId).toBe("old-02");
    fireEvent.click(screen.getByRole("button", { name: "Show older runs" }));
    await waitFor(() => expect(runRows()).toHaveLength(71));
  });

  it("drops a second Run now while the first is in flight", async () => {
    renderTab();
    await screen.findByText("Digest");
    const run = within(digestRow()).getByRole("button", { name: /Run now/ });
    fireEvent.click(run);
    fireEvent.click(run);
    openDigestRuns(within(digestRow()).getByRole("button", { name: "Open Digest" }));
    await waitFor(() => expect(runRows()).toHaveLength(1));
  });
});

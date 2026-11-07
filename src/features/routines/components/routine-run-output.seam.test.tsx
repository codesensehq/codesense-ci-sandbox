// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockRoutines, type MockRoutine } from "@/app/mock/handlers/routines";
import { AgentRunOutputHost } from "@/features/viewer/components/agent-run-output-host";
import { RunsView, useRunHistory } from "@/features/viewer/components/runs-view";
import { useOrgStore } from "@/shared/stores/org-store";
import { useAgentResultStore } from "@/shared/stores/use-agent-result-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { useRoutinesStore, resetRoutinesStoreForTests } from "../stores/routines-store";
import { RunsView as RoutineRunsView } from "./routine-detail";
import type { RoutineRow } from "@/tauri/commands";
import { RoutinesTab } from "./routines-tab";

/**
 * Opening a routine run's output, driven through the same door the app uses
 * (`window.__TAURI_INTERNALS__`) over the mock backend: the Routines run detail and the bottom
 * agent area share one opener, and the output lands in the agent area — never the right pane.
 */

const TEAM = "acme:backend";

/** The Agents pane's Run history view, fetched while it shows. */
function RunHistory({ teamKey }: { teamKey: string }) {
  return <RunsView teamKey={teamKey} runs={useRunHistory(true)} />;
}
const KEY = "acme/backend/digest";

function world(): MockRoutine[] {
  const minute = 60_000;
  const now = Date.now();
  return [
    {
      row: {
        key: KEY,
        scope: "team",
        armed_here: true,
        owner_display: "Alice",
        enabled: true,
        created_by: { kind: "user", name: "alice" },
        schedule_in_words: "every 5 minutes",
        next_run_at: null,
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
      runs: [
        {
          run_id: "digest-run-1",
          routine_key: KEY,
          revision: 1,
          source: "schedule",
          occurrence_key: null,
          phase: "succeeded",
          reason: null,
          linked_run_id: null,
          session_id: "sess-digest-1",
          created_at: now - 5 * minute,
          started_at: now - 4 * minute,
          ended_at: now - 2 * minute,
          result_text: "Three PRs merged overnight.",
          agent: "dev@1",
        },
      ],
    },
  ];
}

beforeEach(() => {
  resetRoutinesStoreForTests();
  useAgentResultStore.getState().close();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useViewerStore.setState({
    layoutMode: "viewer",
    terminalVisible: false,
  });
  const events = new MockEventBus();
  const routines = createMockRoutines(events, { seed: world, missed: 0, runMs: null });
  const internals = buildTauriInternals(
    createInvokeRouter({ ...events.handlers(), ...routines.handlers }),
  );
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
});

afterEach(() => {
  cleanup();
  resetRoutinesStoreForTests();
  useAgentResultStore.getState().close();
});

function renderApp() {
  return render(
    <TooltipProvider>
      <RoutinesTab filePath="virtual://routines" teamContentPath="" />
      <div data-testid="agent-area" className="relative">
        <RunHistory teamKey={TEAM} />
        <div data-testid="cli-tile">live terminal</div>
        <AgentRunOutputHost teamKey={TEAM} repoPath="/repo" />
      </div>
    </TooltipProvider>,
  );
}

describe("Routine run output in the agent area", () => {
  it("shows command evidence inline without offering a transcript or agent session", () => {
    const seeded = world()[0];
    const command = {
      ...seeded.runs[0],
      action: "command" as const,
      argv: ["printf", "a | $HOME"],
      exit_code: 1,
      duration_ms: 400,
      stderr_tail: "problem",
      cwd: "/team/content",
      phase: "failed" as const,
      reason: "exit:1",
      agent: null,
    };
    const row: RoutineRow = {
      ...seeded.row,
      def: {
        ...seeded.row.def,
        action: "command",
        command: ["printf", "a | $HOME"],
        agent: undefined,
        task: undefined,
      },
      agent_display: { name: "", resolvable: false, archived: false },
      last_run: null,
      needs_you: false,
      needs_you_run: null,
    };
    useRoutinesStore.setState({
      runs: { [KEY]: [command] },
      openRun: { ...command, routine_name: "Digest" },
    });
    render(<RoutineRunsView row={row} now={Date.now()} labels={new Map()} />);
    const detail = screen.getByTestId("routine-open-run");
    expect(detail).toHaveTextContent("printf");
    expect(detail).toHaveTextContent("a | $HOME");
    expect(detail).toHaveTextContent("Exit 1 · 0.4s");
    expect(detail).toHaveTextContent("problem");
    expect(screen.getByTestId("routine-run-result")).toHaveTextContent("problem");
    expect(within(detail).getByText("Errors (stderr)").closest("details")).toHaveAttribute("open");
    expect(
      screen.queryByRole("button", { name: /Open transcript|Open live session|Show/ }),
    ).not.toBeInTheDocument();
  });

  it.each([
    ["failed", "unresolved:itemId"],
    ["timed_out", "Stopped at the 1m max runtime"],
  ] as const)(
    "shows the recorded %s reason in both the list and command detail",
    (phase, expected) => {
      const seeded = world()[0];
      const command = {
        ...seeded.runs[0],
        action: "command" as const,
        phase,
        started_at: phase === "failed" ? null : seeded.runs[0].started_at,
        reason: phase === "failed" ? "launch_failed" : "max_runtime",
        result_text: phase === "failed" ? null : "partial",
        detail: phase === "failed" ? "unresolved:itemId" : null,
        max_runtime_ms: 60000,
        agent: null,
      };
      const row: RoutineRow = {
        ...seeded.row,
        agent_display: { name: "", resolvable: false, archived: false },
        last_run: null,
        needs_you: false,
        needs_you_run: null,
        def: {
          ...seeded.row.def,
          action: "command",
          command: ["printf", "{{itemId}}"],
          policy: { ...seeded.row.def.policy, max_runtime: "30m" },
        },
      };
      useRoutinesStore.setState({
        runs: { [KEY]: [command] },
        openRun: { ...command, routine_name: "Digest" },
      });
      render(<RoutineRunsView row={row} now={Date.now()} labels={new Map()} />);
      expect(screen.getAllByText(expected)).toHaveLength(2);
      if (phase === "failed") {
        const detail = screen.getByTestId("routine-open-run");
        expect(within(detail).getByText("Reason")).toBeInTheDocument();
        expect(within(detail).getByText("No output.")).toBeInTheDocument();
      }
    },
  );

  it.each(["hermes-agent@1", "claude-agent@1"])(
    "shows why an agent run %s never started from its detail, whatever its CLI",
    (agent) => {
      const seeded = world()[0];
      const refused = {
        ...seeded.runs[0],
        phase: "failed" as const,
        reason: "launch_failed",
        session_id: null,
        started_at: null,
        result_text: null,
        detail: "Hermes does not support safe-mode in CodeSense",
        agent,
      };
      const row: RoutineRow = {
        ...seeded.row,
        agent_display: { name: "Dev", resolvable: true, archived: false },
        last_run: null,
        needs_you: false,
        needs_you_run: null,
      };
      useRoutinesStore.setState({
        runs: { [KEY]: [refused] },
        openRun: { ...refused, routine_name: "Digest" },
      });
      render(<RoutineRunsView row={row} now={Date.now()} labels={new Map()} />);
      const detail = screen.getByTestId("routine-open-run");
      expect(detail).toHaveTextContent("Failed — the agent did not launch");
      expect(detail).toHaveTextContent("Hermes does not support safe-mode in CodeSense");
      expect(screen.getByTestId("routine-run-result")).toHaveTextContent(
        "Hermes does not support safe-mode in CodeSense",
      );
      // It never ran: there is no transcript to open.
      expect(
        within(detail).queryByRole("button", { name: /Open transcript/ }),
      ).not.toBeInTheDocument();
    },
  );

  it.each([
    [
      "an undelivered wake",
      { phase: "undelivered" as const, reason: "session_gone", wake_machine: "local" },
    ],
    [
      "a run mirrored from a machine",
      { phase: "skipped" as const, reason: "machine", launch_machine: "codesense-test-1" },
    ],
  ])("still shows %s's own text where it always has", (_, shape) => {
    const seeded = world()[0];
    const mirrored = {
      ...seeded.runs[0],
      ...shape,
      started_at: null,
      result_text: "paused on the machine",
      detail: null,
    };
    const row: RoutineRow = {
      ...seeded.row,
      agent_display: { name: "Dev", resolvable: true, archived: false },
      last_run: null,
      needs_you: false,
      needs_you_run: null,
    };
    useRoutinesStore.setState({ runs: { [KEY]: [mirrored] } });
    render(<RoutineRunsView row={row} now={Date.now()} labels={new Map()} />);
    expect(screen.getByTestId("routine-run-result")).toHaveTextContent("paused on the machine");
  });

  it("keeps an admission skip's text off a command run's output, as before", () => {
    const seeded = world()[0];
    const skipped = {
      ...seeded.runs[0],
      action: "command" as const,
      phase: "skipped" as const,
      reason: "loop_guard",
      session_id: null,
      started_at: null,
      ended_at: null,
      result_text: "cycle (digest → digest)",
      detail: null,
      agent: null,
    };
    const row: RoutineRow = {
      ...seeded.row,
      def: { ...seeded.row.def, action: "command", command: ["printf", "ok"] },
      agent_display: { name: "", resolvable: false, archived: false },
      last_run: null,
      needs_you: false,
      needs_you_run: null,
    };
    useRoutinesStore.setState({
      runs: { [KEY]: [skipped] },
      openRun: { ...skipped, routine_name: "Digest" },
    });
    render(<RoutineRunsView row={row} now={Date.now()} labels={new Map()} />);
    expect(screen.queryByTestId("routine-run-result")).not.toBeInTheDocument();
    const detail = screen.getByTestId("routine-open-run");
    expect(within(detail).getByText("No output.")).toBeInTheDocument();
  });

  it("opens from the run detail in the bottom area and leaves the right pane alone", async () => {
    const before = useViewerStore.getState();
    const rightPane = { layout: before.rightPaneLayout, open: before.rightPaneOpen };
    renderApp();

    fireEvent.click(await screen.findByRole("button", { name: "Open Digest" }));
    fireEvent.click(screen.getByRole("tab", { name: "Runs" }));
    fireEvent.click(await screen.findByRole("button", { name: "Open run digest-run-1" }));
    const detail = await screen.findByTestId("routine-open-run");
    fireEvent.click(within(detail).getByRole("button", { name: "Open transcript" }));

    const area = screen.getByTestId("agent-area");
    const output = await within(area).findByRole("region", { name: "Run output" });
    expect(within(output).getByTestId("agent-run-output-name")).toHaveTextContent("digest");
    expect(
      await within(output).findByText(/Three PRs merged overnight\./, { selector: "pre" }),
    ).toBeInTheDocument();
    // The terminal column is revealed and its tile is covered, not removed.
    const viewer = useViewerStore.getState();
    expect(viewer.terminalVisible).toBe(true);
    expect(viewer.layoutMode).toBe("shared");
    expect(within(area).getByTestId("cli-tile")).toBeInTheDocument();
    // The right pane is untouched.
    expect(viewer.rightPaneLayout).toBe(rightPane.layout);
    expect(viewer.rightPaneOpen).toBe(rightPane.open);

    fireEvent.click(within(output).getByRole("button", { name: "Close run output" }));
    expect(within(area).queryByRole("region", { name: "Run output" })).toBeNull();
    expect(within(area).getByTestId("cli-tile")).toBeInTheDocument();
    // The column was hidden before the output opened, so closing hides it again.
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    expect(useViewerStore.getState().terminalVisible).toBe(false);
  });

  it("leaves an already-visible terminal column visible on close", async () => {
    useViewerStore.setState({
      layoutMode: "shared",
      terminalVisible: true,
    });
    renderApp();
    const recent = await screen.findByRole("region", { name: "Run history" });
    fireEvent.click(await within(recent).findByRole("button", { name: /digest/ }));
    const output = await screen.findByRole("region", { name: "Run output" });
    fireEvent.keyDown(output, { key: "Escape" });

    expect(screen.queryByRole("region", { name: "Run output" })).toBeNull();
    expect(useViewerStore.getState().layoutMode).toBe("shared");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
  });

  it("reopens a finished run from the Run history", async () => {
    renderApp();
    const recent = await screen.findByRole("region", { name: "Run history" });
    fireEvent.click(await within(recent).findByRole("button", { name: /digest/ }));

    const output = await screen.findByRole("region", { name: "Run output" });
    await waitFor(() => expect(output).toHaveTextContent("digest-run-1"));
    expect(
      await within(output).findByText(/Three PRs merged overnight\./, { selector: "pre" }),
    ).toBeInTheDocument();
  });
});

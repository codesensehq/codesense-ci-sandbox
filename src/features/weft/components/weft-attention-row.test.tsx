// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@/test-utils";

import type { AttentionKind, AttentionRowDto } from "@/tauri/commands";

const revealPanelById = vi.fn((_id: string) => {});
// SW-WA-E5 (AC-6): the row reads the live tab map from the store and calls the exported
// `findTabBySessionId` to guard the one-click terminal affordance. Stub the store's tab
// maps + the resolver so the guard can be steered to resolve / not resolve.
const findTabBySessionId = vi.fn();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(() => undefined, {
    getState: () => ({
      revealPanelById,
      softClosedWorkspaces: {},
    }),
  }),
  selectRunningTerminalTabIndex: () => ({}),
  findTabLocationBySessionId: (...args: unknown[]) => findTabBySessionId(...args),
}));

// SW-AV-9: the row mounts the real composer on Answer/Steer; stub only the durable
// send transport so the composer renders without pulling the terminal/tauri chain.
const sendTextToSession = vi.fn();
vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToSession: (...args: unknown[]) => sendTextToSession(...args),
}));

// SW-AV-8: the run-detail / last-output opener a dead-target action routes to. Stubbed so
// the row doesn't pull the heavy agent-result-panel chain.
const openAgentResult = vi.fn();
vi.mock("@/features/viewer/components/agent-result-panel", () => ({
  openAgentResult: (...args: unknown[]) => openAgentResult(...args),
}));

// SW-AV-8: the two reachability signals a bare tab lookup misses — a live-headless agent's
// pending `ask_user`, and the union-roster/managed liveness of the handle. Steer per-test.
// The REAL headless answer surface (ParkedQuestionExpansion → ManagedQuestionPanel) reads
// `useOldestPendingAsk` too, so this one mock drives both the row's routing AND the card.
const useOldestPendingAsk = vi.fn((_sid: string): { ask: unknown; pendingCount: number } => ({
  ask: null,
  pendingCount: 0,
}));
vi.mock("@/shared/hooks/use-oldest-pending-ask", () => ({
  useOldestPendingAsk: (sid: string) => useOldestPendingAsk(sid),
}));

const useSessionLiveness = vi.fn((_sid: string | null) => ({
  loaded: false,
  alive: false,
  runId: null as string | null,
  storyId: null as string | null,
}));
vi.mock("../hooks/use-session-liveness", () => ({
  useSessionLiveness: (sid: string | null) => useSessionLiveness(sid),
}));

// SW-AV-8: the shipped headless answer transport ManagedQuestionPanel delivers through —
// stubbed so a delivery assertion doesn't hit the tauri oneshot bridge.
const answerAskUser = vi.fn();
vi.mock("@/shared/hooks/use-managed-session-bridge", () => ({
  answerAskUser: (...args: unknown[]) => answerAskUser(...args),
}));

import { WeftAttentionRow } from "./weft-attention-row";

function row(overrides: Partial<AttentionRowDto> = {}): AttentionRowDto {
  return {
    id: 1,
    summary: "You owe: review the plan.",
    source: null,
    ref: null,
    initiator: "human",
    ...overrides,
  };
}

describe("SW-WV-F1 · WeftAttentionRow (AC-3)", () => {
  beforeEach(() => revealPanelById.mockClear());

  it("renders the summary and a single primary verb", () => {
    render(<WeftAttentionRow row={row()} status="dropped" />);
    expect(screen.getByText("You owe: review the plan.")).toBeInTheDocument();
    // With no wired review/terminal action the primary verb routes to where it resolves.
    expect(screen.getByTestId("weft-row-primary-verb")).toBeInTheDocument();
  });

  it('source="panel" routes to the named panel via the primary verb', () => {
    render(
      <WeftAttentionRow row={row({ source: "panel", ref: "automations" })} status="dropped" />,
    );
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    expect(revealPanelById).toHaveBeenCalledWith("automations");
  });

  it("null source is a no-op (no throw, no route)", () => {
    render(<WeftAttentionRow row={row({ source: null })} status="dropped" />);
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    expect(revealPanelById).not.toHaveBeenCalled();
  });

  it("unknown source is a no-op (no throw, no route)", () => {
    render(
      <WeftAttentionRow row={row({ source: "totally-unknown", ref: "x" })} status="dropped" />,
    );
    expect(() => fireEvent.click(screen.getByTestId("weft-row-primary-verb"))).not.toThrow();
    expect(revealPanelById).not.toHaveBeenCalled();
  });

  // SW-WA-E5 (AC-3) + SW-AV-3: the 🤖 provenance marker (initiator `"agent"`).
  it("AC-3: an agent row renders the 🤖 provenance marker; a human row does not", () => {
    const { rerender } = render(
      <WeftAttentionRow row={row({ initiator: "agent" })} status="running" />,
    );
    expect(screen.getByTestId("weft-row-agent-marker")).toBeInTheDocument();
    expect(screen.getByLabelText("Started by the agent")).toBeInTheDocument();

    rerender(<WeftAttentionRow row={row({ initiator: "human" })} status="running" />);
    expect(screen.queryByTestId("weft-row-agent-marker")).not.toBeInTheDocument();
  });

  // SW-WA-E5 (AC-1 TS fixture): a row carrying the new fields compiles + renders.
  it("AC-1: a row with the provenance axis + live handle renders its summary", () => {
    render(
      <WeftAttentionRow
        row={row({ initiator: "agent", liveSessionId: "sess-1" })}
        status="running"
      />,
    );
    expect(screen.getByText("You owe: review the plan.")).toBeInTheDocument();
  });

  // SW-WA-E5 (AC-6): the one-click live-terminal affordance is now the "Steer" primary verb,
  // gated by the same FE resolvability guard.
  describe("AC-6 · Steer (open-live-terminal) primary verb", () => {
    beforeEach(() => findTabBySessionId.mockReset());

    it("promotes Steer (weft-row-open-terminal) and invokes onOpenTerminal(row) when the handle resolves", () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      const onOpenTerminal = vi.fn();
      const r = row({ initiator: "agent", liveSessionId: "sess-1", kind: "running" });
      render(<WeftAttentionRow row={r} status="running" onOpenTerminal={onOpenTerminal} />);
      const btn = screen.getByTestId("weft-row-open-terminal");
      expect(btn).toHaveTextContent("Steer");
      fireEvent.click(btn);
      expect(onOpenTerminal).toHaveBeenCalledWith(r);
    });

    it("does NOT promote Steer when the handle does not resolve (a bare-name / closed-tab handle)", () => {
      findTabBySessionId.mockReturnValue(undefined);
      render(
        <WeftAttentionRow
          row={row({ initiator: "agent", liveSessionId: "worker-name", kind: "running" })}
          status="running"
          onOpenTerminal={vi.fn()}
        />,
      );
      expect(screen.queryByTestId("weft-row-open-terminal")).not.toBeInTheDocument();
    });

    it("does NOT promote Steer when the row has no liveSessionId", () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      render(
        <WeftAttentionRow
          row={row({ initiator: "agent", liveSessionId: null, kind: "running" })}
          status="running"
          onOpenTerminal={vi.fn()}
        />,
      );
      expect(screen.queryByTestId("weft-row-open-terminal")).not.toBeInTheDocument();
      // With no handle, the resolver is never consulted.
      expect(findTabBySessionId).not.toHaveBeenCalled();
    });
  });

  // SW-WA-E12 (AC-2/AC-7): the demoted dispatch overflow carries a real tooltip.
  it("SW-WA-E12 AC-7: the demoted dispatch overflow has a tooltip that explains it dispatches a worker", async () => {
    render(<WeftAttentionRow row={row()} status="awaiting-answer" onDispatch={vi.fn()} />);
    const btn = screen.getByTestId("weft-row-dispatch");
    btn.focus();
    expect(await screen.findByRole("tooltip")).toHaveTextContent(
      /Dispatch a worker to handle this/i,
    );
  });

  it("SW-WA-E9: an old-shaped (no-kind) PARKED row falls back to the 'Parked' chip, not 'Done'", () => {
    // The slate "done" tone stays, but the label reads its real state.
    render(<WeftAttentionRow row={row()} status="done" chipLabel="Parked" />);
    const chip = screen.getByRole("img");
    expect(chip).toHaveTextContent("Parked");
    expect(chip).not.toHaveTextContent("Done");
  });

  // ── SW-AV-4: calm resting redesign — chip-from-kind, one primary verb, hover secondaries ──
  describe("SW-AV-4 · calm resting row", () => {
    beforeEach(() => findTabBySessionId.mockReset());

    it("derives the chip from the row's kind, NOT the lane (same lane tone → distinct chips)", () => {
      // Every row is given the SAME lane `status` (dropped) — a kind-derived chip must still
      // read differently per kind, proving it comes from `kind`, not the section.
      const { rerender } = render(
        <WeftAttentionRow row={row({ kind: "running" })} status="dropped" />,
      );
      expect(screen.getByRole("img")).toHaveTextContent("Working");

      rerender(<WeftAttentionRow row={row({ kind: "parked" })} status="dropped" />);
      expect(screen.getByRole("img")).toHaveTextContent("Parked");

      rerender(<WeftAttentionRow row={row({ kind: "blocked" })} status="dropped" />);
      const blocked = screen.getByRole("img");
      expect(blocked).toHaveTextContent("Blocked");
      // …and never the section's own "Needs attention" (dropped) label.
      expect(blocked).not.toHaveTextContent(/needs attention/i);
    });

    it("two YOU-OWE rows of different kinds show different chips", () => {
      const { rerender } = render(
        <WeftAttentionRow row={row({ kind: "assertion" })} status="dropped" />,
      );
      expect(screen.getByRole("img")).toHaveTextContent("Assertion");
      rerender(<WeftAttentionRow row={row({ kind: "approval" })} status="dropped" />);
      expect(screen.getByRole("img")).toHaveTextContent("Approval");
    });

    it("shows EXACTLY ONE primary verb at rest, with the demoted dispatch in the hover/focus secondaries", () => {
      const { container } = render(
        <WeftAttentionRow row={row({ kind: "assertion" })} status="dropped" onDispatch={vi.fn()} />,
      );
      // Exactly one at-rest primary control.
      expect(container.querySelectorAll('[data-role="primary"]')).toHaveLength(1);
      expect(screen.getByTestId("weft-row-primary-verb")).toHaveTextContent("Ratify");
      // Dispatch lives inside the hover/focus-revealed secondaries, never at rest as a peer.
      const secondaries = screen.getByTestId("weft-row-secondaries");
      expect(secondaries).toContainElement(screen.getByTestId("weft-row-dispatch"));
    });

    it.each<[AttentionKind, string]>([
      ["assertion", "Ratify"],
      ["approval", "Approve"],
      ["question", "Answer"],
      ["blocked", "Answer"],
      // SW-AV-7: parked's reach-it verb is the ONE canonical "Go" (was "Open").
      ["parked", "Go"],
    ])("the primary verb matches kind %s → %s", (kind, verb) => {
      render(<WeftAttentionRow row={row({ kind })} status="dropped" />);
      expect(screen.getByTestId("weft-row-primary-verb")).toHaveTextContent(verb);
    });

    it("running → Steer is the primary verb when the live handle resolves", () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      render(
        <WeftAttentionRow
          row={row({ kind: "running", liveSessionId: "sess-1" })}
          status="running"
          onOpenTerminal={vi.fn()}
        />,
      );
      expect(screen.getByTestId("weft-row-open-terminal")).toHaveTextContent("Steer");
    });

    it("hides the secondaries at rest and reveals them on hover/focus (keyboard-reachable)", () => {
      render(
        <WeftAttentionRow row={row({ kind: "assertion" })} status="dropped" onDispatch={vi.fn()} />,
      );
      const secondaries = screen.getByTestId("weft-row-secondaries");
      // Calm at rest: opacity-0. Revealed on hover AND on keyboard focus-within — never
      // `hidden`/`display:none`, so the controls stay in tab order.
      expect(secondaries.className).toContain("opacity-0");
      expect(secondaries.className).toContain("group-hover:opacity-100");
      expect(secondaries.className).toContain("group-focus-within:opacity-100");
      // Reduced motion drops only the fade, not the reveal.
      expect(secondaries.className).toContain("motion-reduce:transition-none");
      // The secondary is genuinely keyboard-reachable (a real, focusable button).
      const dispatch = screen.getByTestId("weft-row-dispatch");
      expect(dispatch).not.toHaveAttribute("tabindex", "-1");
      dispatch.focus();
      expect(document.activeElement).toBe(dispatch);
    });
  });

  // ── SW-AV-5: every row drills to its agent (whole-row click + → go), never the old lite focus ──
  describe("SW-AV-5 · row → agent drill", () => {
    beforeEach(() => {
      revealPanelById.mockClear();
      findTabBySessionId.mockReset();
    });

    it("a whole-row click drills to the row's live agent when onDrill + liveSessionId are present", () => {
      const onDrill = vi.fn();
      const r = row({ kind: "running", liveSessionId: "sess-1" });
      render(<WeftAttentionRow row={r} status="running" onDrill={onDrill} />);
      // The row body is a drillable click target (keyboard users reach the drill via → go).
      const body = screen.getByTestId("weft-attention-row");
      expect(body).toHaveAttribute("data-drillable", "true");
      fireEvent.click(body);
      expect(onDrill).toHaveBeenCalledWith(r);
    });

    it("the → go chevron drills to the agent (not routeAttentionRow) when the row is drillable", () => {
      const onDrill = vi.fn();
      // A YOU-OWE-shaped row (source panel) whose primary verb IS its route — the chevron is
      // still surfaced BECAUSE the row is drillable, and it drills rather than routing.
      const r = row({
        kind: "question",
        source: "panel",
        ref: "automations",
        liveSessionId: "sess-1",
      });
      render(<WeftAttentionRow row={r} status="dropped" onDrill={onDrill} />);
      const go = screen.getByTestId("weft-row-go");
      fireEvent.click(go);
      expect(onDrill).toHaveBeenCalledWith(r);
      // It drilled — it did NOT also route to the panel.
      expect(revealPanelById).not.toHaveBeenCalled();
    });

    it("clicking the primary verb does NOT also drill (stopPropagation) — the verb owns its click", () => {
      const onDrill = vi.fn();
      const r = row({ source: "panel", ref: "automations", liveSessionId: "sess-1" });
      render(<WeftAttentionRow row={r} status="dropped" onDrill={onDrill} />);
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      // The primary verb routed to its panel; the whole-row drill did NOT fire.
      expect(revealPanelById).toHaveBeenCalledWith("automations");
      expect(onDrill).not.toHaveBeenCalled();
    });

    it("a row with no live agent is not drillable — a whole-row click is a benign no-op", () => {
      const onDrill = vi.fn();
      render(
        <WeftAttentionRow
          row={row({ source: "panel", ref: "automations", liveSessionId: null })}
          status="dropped"
          onDrill={onDrill}
        />,
      );
      const body = screen.getByTestId("weft-attention-row");
      expect(body).not.toHaveAttribute("data-drillable");
      fireEvent.click(body);
      expect(onDrill).not.toHaveBeenCalled();
    });
  });

  // ── SW-AV-9: Answer / Steer opens the inline composer + lands focus on the input ──
  describe("SW-AV-9 · Answer / Steer composer", () => {
    beforeEach(() => {
      findTabBySessionId.mockReset();
      sendTextToSession.mockReset();
      sendTextToSession.mockResolvedValue(undefined);
    });

    it("Answer on a question row opens the composer and lands focus on the input", () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      render(
        <WeftAttentionRow
          row={row({ kind: "question", liveSessionId: "sess-1" })}
          status="awaiting-answer"
        />,
      );
      // Not open at rest.
      expect(screen.queryByTestId("weft-answer-composer")).not.toBeInTheDocument();
      const answer = screen.getByTestId("weft-row-primary-verb");
      expect(answer).toHaveTextContent("Answer");
      fireEvent.click(answer);
      const input = screen.getByTestId("weft-answer-composer-input");
      expect(input).toBeInTheDocument();
      expect(document.activeElement).toBe(input);
    });

    it("Answer on a blocked row opens the composer too", () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      render(
        <WeftAttentionRow
          row={row({ kind: "blocked", liveSessionId: "sess-1" })}
          status="dropped"
        />,
      );
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      expect(screen.getByTestId("weft-answer-composer")).toBeInTheDocument();
    });

    it("Steer on a running row drills to the terminal AND opens the composer", () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      const onOpenTerminal = vi.fn();
      const r = row({ kind: "running", liveSessionId: "sess-1" });
      render(<WeftAttentionRow row={r} status="running" onOpenTerminal={onOpenTerminal} />);
      fireEvent.click(screen.getByTestId("weft-row-open-terminal"));
      expect(onOpenTerminal).toHaveBeenCalledWith(r);
      expect(screen.getByTestId("weft-answer-composer")).toBeInTheDocument();
    });

    it("the composer sends to the row's liveSessionId through the durable path", async () => {
      findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
      render(
        <WeftAttentionRow
          row={row({ kind: "question", liveSessionId: "sess-1" })}
          status="awaiting-answer"
        />,
      );
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      fireEvent.change(screen.getByTestId("weft-answer-composer-input"), {
        target: { value: "yes, proceed" },
      });
      fireEvent.click(screen.getByTestId("weft-answer-composer-send"));
      expect(sendTextToSession).toHaveBeenCalledWith(
        "sess-1",
        "yes, proceed",
        expect.objectContaining({ submit: true }),
      );
    });

    it("a gone target (unresolvable handle) opens the composer DISABLED with an explanation", () => {
      // A bare-name / since-closed handle does not resolve to a tab, and — with no settled
      // liveness read yet (default `loaded:false`) — it isn't classified dead either, so it
      // degrades to the honest disabled composer rather than the recovery state.
      findTabBySessionId.mockReturnValue(undefined);
      render(
        <WeftAttentionRow
          row={row({ kind: "question", liveSessionId: "worker-name" })}
          status="awaiting-answer"
        />,
      );
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      expect(screen.getByTestId("weft-answer-composer-send")).toBeDisabled();
      expect(screen.getByTestId("weft-answer-composer-disabled")).toBeInTheDocument();
    });
  });
});

// ── SW-AV-8: dead/exited agent handling + live-headless answer routing (no dead-end actions) ──
describe("SW-AV-8 · dead-agent recovery + headless-answer routing", () => {
  beforeEach(() => {
    revealPanelById.mockClear();
    findTabBySessionId.mockReset();
    findTabBySessionId.mockReturnValue(undefined); // default: no tab
    openAgentResult.mockReset();
    answerAskUser.mockReset();
    sendTextToSession.mockReset();
    sendTextToSession.mockResolvedValue(undefined);
    useOldestPendingAsk.mockReset();
    useOldestPendingAsk.mockReturnValue({ ask: null, pendingCount: 0 });
    useSessionLiveness.mockReset();
    // Default to a SETTLED "still running" read so only the tests that opt in read dead.
    useSessionLiveness.mockReturnValue({ loaded: true, alive: true, runId: null, storyId: null });
  });

  // The core seam: an action on a not-alive liveSessionId routes to last output AND surfaces
  // the "agent no longer running" recovery state — never a swallowed no-op.
  it("routes a dead-target action to the last output (openAgentResult) and shows the recovery state", () => {
    useSessionLiveness.mockReturnValue({
      loaded: true,
      alive: false,
      runId: "run-x",
      storyId: "SW-A",
    });
    const r = row({ kind: "assertion", liveSessionId: "sess-dead" });
    render(<WeftAttentionRow row={r} status="dropped" onDrill={vi.fn()} onRecover={vi.fn()} />);
    const primary = screen.getByTestId("weft-row-primary-verb");
    // The verb LABEL is unchanged (a Ratify still reads Ratify) — only its routing changes.
    expect(primary).toHaveTextContent("Ratify");
    fireEvent.click(primary);
    expect(openAgentResult).toHaveBeenCalledWith(
      expect.objectContaining({ run_id: "run-x", sessionId: "sess-dead", story_id: "SW-A" }),
    );
    // The "agent no longer running" state with relaunch / reassign / dismiss (not a no-op).
    expect(screen.getByTestId("weft-dead-agent-recovery")).toBeInTheDocument();
    expect(screen.getByTestId("weft-dead-recover-relaunch")).toBeInTheDocument();
    expect(screen.getByTestId("weft-dead-recover-reassign")).toBeInTheDocument();
    expect(screen.getByTestId("weft-dead-recover-dismiss")).toBeInTheDocument();
  });

  it("a dead-target row is NOT drilled — the whole-row click opens recovery instead of the drill's dead-end", () => {
    useSessionLiveness.mockReturnValue({
      loaded: true,
      alive: false,
      runId: "run-y",
      storyId: null,
    });
    const onDrill = vi.fn();
    const r = row({ kind: "assertion", liveSessionId: "sess-dead" });
    render(<WeftAttentionRow row={r} status="dropped" onDrill={onDrill} onRecover={vi.fn()} />);
    const body = screen.getByTestId("weft-attention-row");
    expect(body).not.toHaveAttribute("data-drillable");
    expect(body).toHaveAttribute("data-dead", "true");
    fireEvent.click(body);
    expect(onDrill).not.toHaveBeenCalled();
    expect(openAgentResult).toHaveBeenCalledWith(expect.objectContaining({ run_id: "run-y" }));
    expect(screen.getByTestId("weft-dead-agent-recovery")).toBeInTheDocument();
  });

  it("Relaunch / Reassign fire onRecover with the intent; Dismiss collapses the state", () => {
    useSessionLiveness.mockReturnValue({
      loaded: true,
      alive: false,
      runId: "run-z",
      storyId: null,
    });
    const onRecover = vi.fn();
    const r = row({ kind: "assertion", liveSessionId: "sess-dead" });
    render(<WeftAttentionRow row={r} status="dropped" onRecover={onRecover} />);
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    fireEvent.click(screen.getByTestId("weft-dead-recover-relaunch"));
    expect(onRecover).toHaveBeenCalledWith(r, "relaunch");
    fireEvent.click(screen.getByTestId("weft-dead-recover-reassign"));
    expect(onRecover).toHaveBeenCalledWith(r, "reassign");
    fireEvent.click(screen.getByTestId("weft-dead-recover-dismiss"));
    expect(screen.queryByTestId("weft-dead-agent-recovery")).not.toBeInTheDocument();
  });

  it("a dead target with no resolvable run id still ROUTES (never a swallowed no-op) and shows recovery", () => {
    useSessionLiveness.mockReturnValue({ loaded: true, alive: false, runId: null, storyId: null });
    const r = row({
      kind: "assertion",
      source: "panel",
      ref: "automations",
      liveSessionId: "sess-dead",
    });
    render(<WeftAttentionRow row={r} status="dropped" onRecover={vi.fn()} />);
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    // No run id → fall back to the shared route seam rather than a no-op; still recovers.
    expect(revealPanelById).toHaveBeenCalledWith("automations");
    expect(openAgentResult).not.toHaveBeenCalled();
    expect(screen.getByTestId("weft-dead-agent-recovery")).toBeInTheDocument();
  });

  it("recovery buttons hide when no onRecover is wired, but Dismiss stays (never a dead-end)", () => {
    useSessionLiveness.mockReturnValue({ loaded: true, alive: false, runId: "r", storyId: null });
    render(
      <WeftAttentionRow
        row={row({ kind: "assertion", liveSessionId: "sess-dead" })}
        status="dropped"
      />,
    );
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    expect(screen.queryByTestId("weft-dead-recover-relaunch")).not.toBeInTheDocument();
    expect(screen.queryByTestId("weft-dead-recover-reassign")).not.toBeInTheDocument();
    expect(screen.getByTestId("weft-dead-recover-dismiss")).toBeInTheDocument();
  });

  // The folded gap: a LIVE HEADLESS agent (no tab) asking a question is answerable inline
  // through the shipped headless path, NOT a disabled submit_pty_line composer.
  it("a live-headless parked row's Answer opens the headless answer surface (not the disabled composer) and delivers", () => {
    useSessionLiveness.mockReturnValue({ loaded: true, alive: true, runId: null, storyId: null });
    useOldestPendingAsk.mockImplementation((sid: string) =>
      sid === "headless-1"
        ? {
            ask: {
              correlationId: "ask-user-1",
              sessionId: "headless-1",
              question: "Ship it?",
              options: ["Yes", "No"],
            },
            pendingCount: 1,
          }
        : { ask: null, pendingCount: 0 },
    );
    render(
      <WeftAttentionRow
        row={row({ kind: "question", liveSessionId: "headless-1" })}
        status="awaiting-answer"
      />,
    );
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    // The headless answer surface, NOT the disabled submit_pty_line composer.
    expect(screen.getByTestId("parked-question-expansion")).toBeInTheDocument();
    expect(screen.getByTestId("managed-question-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("weft-answer-composer")).not.toBeInTheDocument();
    // And it delivers through the shipped headless transport.
    fireEvent.click(screen.getByText("Yes"));
    expect(answerAskUser).toHaveBeenCalledWith(
      "ask-user-1",
      expect.objectContaining({ action: "accept", content: "Yes" }),
    );
  });

  it("a live TAB-backed row is unchanged — Answer still opens the AV-9 submit_pty_line composer", () => {
    // Tab resolves → the AV-9 path, regardless of the SW-AV-8 signals.
    findTabBySessionId.mockReturnValue({ id: "tab-1", sessionId: "sess-1" });
    render(
      <WeftAttentionRow
        row={row({ kind: "question", liveSessionId: "sess-1" })}
        status="awaiting-answer"
      />,
    );
    fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
    expect(screen.getByTestId("weft-answer-composer")).toBeInTheDocument();
    expect(screen.queryByTestId("parked-question-expansion")).not.toBeInTheDocument();
    expect(screen.queryByTestId("weft-dead-agent-recovery")).not.toBeInTheDocument();
  });

  // SW-AV-21: a human decision (Approve/Ratify) is driven THROUGH the agent's runtime;
  // when the runtime DECLINES it, the row must SHOW the rejection — an outcome CodeSense
  // displays, never a gate it enforces.
  describe("SW-AV-21 · permission-denied outcome on a decision row", () => {
    it("a DECLINED Approve flips the row to the rejection signal with the runtime's reason", async () => {
      const onDecision = vi.fn().mockResolvedValue({
        ok: false,
        reason: "permission denied by agent runtime",
      });
      render(
        <WeftAttentionRow
          row={row({ kind: "approval", initiator: "agent" })}
          status="paused"
          onDecision={onDecision}
        />,
      );
      const btn = screen.getByTestId("weft-row-primary-verb");
      expect(btn).toHaveTextContent("Approve");
      // No rejection at rest.
      expect(screen.queryByTestId("weft-decision-rejection")).not.toBeInTheDocument();

      fireEvent.click(btn);
      expect(onDecision).toHaveBeenCalledTimes(1);

      // The declined outcome renders as an explicit rejection, carrying the reason.
      const rejection = await screen.findByTestId("weft-decision-rejection");
      expect(rejection).toHaveTextContent("Declined by agent");
      expect(screen.getByTestId("weft-decision-rejection-reason")).toHaveTextContent(
        "permission denied by agent runtime",
      );
      // It is DISPLAY-ONLY: no CodeSense gate/route ran on the decline.
      expect(revealPanelById).not.toHaveBeenCalled();

      // Dismiss collapses the signal (the durable ledger owns the row).
      fireEvent.click(screen.getByTestId("weft-decision-rejection-dismiss"));
      expect(screen.queryByTestId("weft-decision-rejection")).not.toBeInTheDocument();
    });

    it("a declined Ratify with no reason shows the generic rejection copy", async () => {
      const onDecision = vi.fn().mockResolvedValue({ ok: false });
      render(
        <WeftAttentionRow
          row={row({ kind: "assertion", initiator: "agent" })}
          status="paused"
          onDecision={onDecision}
        />,
      );
      expect(screen.getByTestId("weft-row-primary-verb")).toHaveTextContent("Ratify");
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      const rejection = await screen.findByTestId("weft-decision-rejection");
      expect(rejection).toHaveTextContent("The agent's runtime rejected this decision.");
    });

    it("a rejected drive (thrown decline) is surfaced, never swallowed into a phantom success", async () => {
      const onDecision = vi.fn().mockRejectedValue(new Error("runtime rejected: forbidden"));
      render(
        <WeftAttentionRow
          row={row({ kind: "approval", initiator: "agent" })}
          status="paused"
          onDecision={onDecision}
        />,
      );
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      const rejection = await screen.findByTestId("weft-decision-rejection");
      expect(rejection).toHaveTextContent("runtime rejected: forbidden");
    });

    it("an ACCEPTED decision leaves the row unchanged — no rejection signal", async () => {
      const onDecision = vi.fn().mockResolvedValue({ ok: true });
      render(
        <WeftAttentionRow
          row={row({ kind: "approval", initiator: "agent" })}
          status="paused"
          onDecision={onDecision}
        />,
      );
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      await waitFor(() => expect(onDecision).toHaveBeenCalledTimes(1));
      expect(screen.queryByTestId("weft-decision-rejection")).not.toBeInTheDocument();
      expect(revealPanelById).not.toHaveBeenCalled();
    });

    it("without an onDecision driver an approval row is unchanged — the verb routes, no rejection", () => {
      render(
        <WeftAttentionRow
          row={row({ kind: "approval", source: "panel", ref: "automations" })}
          status="paused"
        />,
      );
      fireEvent.click(screen.getByTestId("weft-row-primary-verb"));
      // Legacy behavior preserved: the primary verb routes to where it resolves.
      expect(revealPanelById).toHaveBeenCalledWith("automations");
      expect(screen.queryByTestId("weft-decision-rejection")).not.toBeInTheDocument();
    });
  });
});

// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CANCELLED_NOTE_MS, ParkedQuestionExpansion } from "./parked-question-expansion";
import {
  useManagedSessionStore,
  resetManagedSessionStoreForTests,
} from "@/shared/stores/use-managed-session-store";
import type { AskUserRequest } from "@/shared/lib/managed-session";

vi.mock("@/tauri/commands", () => ({
  resolveAskUser: vi.fn().mockResolvedValue(undefined),
}));

const ask = (overrides: Partial<AskUserRequest> = {}): AskUserRequest => ({
  correlationId: "ask-user-0",
  sessionId: "s1",
  question: "Deploy to prod or staging?",
  ...overrides,
});

describe("ParkedQuestionExpansion (SW-BA-7)", () => {
  beforeEach(() => {
    useAgentListProjectionStore.setState({ sessions: [] });
    resetManagedSessionStoreForTests();
    useAgentStateStore.setState({ exitBySession: {} });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("AC-2: mounts the REUSED ManagedQuestionPanel for the parked session", () => {
    useManagedSessionStore.getState().addPendingAsk(ask());
    render(<ParkedQuestionExpansion sessionId="s1" onCollapse={vi.fn()} />);
    // The shared card — not a fork.
    expect(screen.getByTestId("managed-question-panel")).toBeInTheDocument();
    expect(screen.getByText("Deploy to prod or staging?")).toBeInTheDocument();
  });

  it("AC-3c: focuses the first control inside the card on expand (no window raise)", () => {
    useManagedSessionStore.getState().addPendingAsk(ask({ options: ["Prod", "Staging"] }));
    render(<ParkedQuestionExpansion sessionId="s1" onCollapse={vi.fn()} />);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Prod" }));
  });

  it("AC-3a: Escape collapses the row; keydowns never reach the list nav", async () => {
    const user = userEvent.setup();
    const onCollapse = vi.fn();
    const parentKeyDown = vi.fn();
    useManagedSessionStore.getState().addPendingAsk(ask());
    render(
      // The roving-nav lives on an ancestor of the row — assert our keys don't bubble.
      <div onKeyDown={parentKeyDown}>
        <ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />
      </div>,
    );
    const textarea = screen.getByLabelText("Your answer");
    textarea.focus();
    await user.keyboard("{ArrowDown}");
    expect(parentKeyDown).not.toHaveBeenCalled(); // stopPropagation kept it in the card
    await user.keyboard("{Escape}");
    expect(onCollapse).toHaveBeenCalledTimes(1);
  });

  it("AC-4: a run that dies while parked resolves to a cancelled note, then auto-collapses", () => {
    vi.useFakeTimers();
    const onCollapse = vi.fn();
    useManagedSessionStore.getState().addPendingAsk(ask());
    const { rerender } = render(<ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />);
    // The death sweep sheds the ask (applyDoneStatus/handleSessionDeath) and the chip
    // flips to a terminal state — the card must NOT silently vanish.
    act(() => {
      projectSessionStatus("s1", "dropped", { managedState: "needs-attention" });
      useManagedSessionStore.getState().handleSessionDeath("s1");
    });
    rerender(<ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />);

    expect(screen.getByText(/This run ended/)).toBeInTheDocument();
    expect(screen.queryByTestId("managed-question-panel")).not.toBeInTheDocument();
    expect(onCollapse).not.toHaveBeenCalled(); // lingers first
    act(() => {
      vi.advanceTimersByTime(CANCELLED_NOTE_MS);
    });
    expect(onCollapse).toHaveBeenCalledTimes(1);
  });

  it("AC-4: an answered run (ask cleared, back to running) collapses at once with no note", () => {
    const onCollapse = vi.fn();
    useManagedSessionStore.getState().addPendingAsk(ask());
    const { rerender } = render(<ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />);
    act(() => {
      useManagedSessionStore.getState().resolvePendingAsk("ask-user-0"); // answered → running
    });
    rerender(<ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />);

    expect(screen.queryByText(/This run ended/)).not.toBeInTheDocument();
    expect(onCollapse).toHaveBeenCalledTimes(1);
  });

  it("AC-2: renders the stakes/impact hint when the ask carries one", () => {
    useManagedSessionStore
      .getState()
      .addPendingAsk(ask({ stakes: "Irreversible — deletes prod data" }));
    render(<ParkedQuestionExpansion sessionId="s1" onCollapse={vi.fn()} />);
    expect(screen.getByText("Irreversible — deletes prod data")).toBeInTheDocument();
  });

  it("AC-4: with a second ask queued, answering the oldest keeps the card open on the next", () => {
    const onCollapse = vi.fn();
    useManagedSessionStore
      .getState()
      .addPendingAsk(ask({ correlationId: "ask-user-0", question: "First?" }));
    useManagedSessionStore
      .getState()
      .addPendingAsk(ask({ correlationId: "ask-user-1", question: "Second?" }));
    render(<ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />);
    expect(screen.getByText("First?")).toBeInTheDocument();

    act(() => {
      useManagedSessionStore.getState().resolvePendingAsk("ask-user-0");
    });
    expect(screen.getByText("Second?")).toBeInTheDocument();
    expect(onCollapse).not.toHaveBeenCalled(); // still awaiting an answer — must not collapse
  });

  it("AC-3: a run dying while the user is elsewhere does not steal focus to the note", () => {
    useManagedSessionStore.getState().addPendingAsk(ask());
    render(
      <div>
        <button data-testid="elsewhere">elsewhere</button>
        <ParkedQuestionExpansion sessionId="s1" onCollapse={vi.fn()} />
      </div>,
    );
    // The user moved focus out of the card (opened it, then tabbed away).
    const elsewhere = screen.getByTestId("elsewhere");
    elsewhere.focus();
    expect(document.activeElement).toBe(elsewhere);

    act(() => {
      projectSessionStatus("s1", "dropped", { managedState: "needs-attention" });
      useManagedSessionStore.getState().handleSessionDeath("s1");
    });
    // The note is present (announced via role=status), but focus is NOT yanked back.
    expect(screen.getByText(/This run ended/)).toBeInTheDocument();
    expect(document.activeElement).toBe(elsewhere);
  });

  it("AC-3c: Escape returns focus to the row's roving chip, not <body>", async () => {
    const user = userEvent.setup();
    const onCollapse = vi.fn();
    useManagedSessionStore.getState().addPendingAsk(ask({ options: ["Prod"] }));
    render(
      // Mimic the row: a [data-index] container carrying the roving-nav chip ([data-rov]).
      <div data-index="0">
        <button data-rov>chip</button>
        <ParkedQuestionExpansion sessionId="s1" onCollapse={onCollapse} />
      </div>,
    );
    // Focus starts inside the card (AC-3c).
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Prod" }));
    await user.keyboard("{Escape}");
    expect(onCollapse).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "chip" }));
  });
});

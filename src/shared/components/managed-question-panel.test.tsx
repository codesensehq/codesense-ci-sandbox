// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ManagedQuestionPanel } from "./managed-question-panel";
import {
  useManagedSessionStore,
  selectManagedState,
  resetManagedSessionStoreForTests,
} from "@/shared/stores/use-managed-session-store";
import type { AskUserRequest } from "@/shared/lib/managed-session";
import { resolveAskUser } from "@/tauri/commands";

vi.mock("@/tauri/commands", () => ({
  resolveAskUser: vi.fn().mockResolvedValue(undefined),
}));

const stateOf = (sessionId: string) =>
  selectManagedState(sessionId)(useManagedSessionStore.getState());
const ask = (overrides: Partial<AskUserRequest> = {}): AskUserRequest => ({
  correlationId: "ask-user-0",
  sessionId: "s1",
  question: "Which database should I use?",
  ...overrides,
});

describe("ManagedQuestionPanel (SW-AW-4 answer-from-the-UI)", () => {
  beforeEach(() => {
    resetManagedSessionStoreForTests();
    vi.mocked(resolveAskUser).mockClear();
  });

  it("renders nothing when there is no pending ask", () => {
    const { container } = render(<ManagedQuestionPanel sessionId="s1" />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders the question and sends a free-text answer (accept)", async () => {
    const user = userEvent.setup();
    useManagedSessionStore.getState().addPendingAsk(ask());
    render(<ManagedQuestionPanel sessionId="s1" />);

    expect(screen.getByText("Which database should I use?")).toBeInTheDocument();
    await user.type(screen.getByLabelText("Your answer"), "Postgres");
    await user.click(screen.getByRole("button", { name: "Send answer" }));

    expect(resolveAskUser).toHaveBeenCalledWith("ask-user-0", "accept", "Postgres");
    expect(useManagedSessionStore.getState().pendingAsks["ask-user-0"]).toBeUndefined();
    expect(stateOf("s1")).toBe("running");
  });

  it("renders discrete options and answers with the chosen one", async () => {
    const user = userEvent.setup();
    useManagedSessionStore.getState().addPendingAsk(ask({ options: ["Postgres", "SQLite"] }));
    render(<ManagedQuestionPanel sessionId="s1" />);

    await user.click(screen.getByRole("button", { name: "SQLite" }));
    expect(resolveAskUser).toHaveBeenCalledWith("ask-user-0", "accept", "SQLite");
  });

  it("Handled it tells the agent the human settled it themselves", async () => {
    const user = userEvent.setup();
    useManagedSessionStore.getState().addPendingAsk(ask());
    render(<ManagedQuestionPanel sessionId="s1" />);

    await user.click(screen.getByRole("button", { name: "I handled this myself" }));
    expect(resolveAskUser).toHaveBeenCalledWith("ask-user-0", "handled", null);
    expect(useManagedSessionStore.getState().pendingAsks["ask-user-0"]).toBeUndefined();
  });

  it("a question with options can also be marked handled or dismissed", async () => {
    const user = userEvent.setup();
    useManagedSessionStore.getState().addPendingAsk(ask({ options: ["Push", "Don't push"] }));
    render(<ManagedQuestionPanel sessionId="s1" />);

    expect(screen.getByRole("button", { name: /Dismiss/ })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "I handled this myself" }));
    expect(resolveAskUser).toHaveBeenCalledWith("ask-user-0", "handled", null);
  });

  it("dismiss sends a cancel with null content (no wedge)", async () => {
    const user = userEvent.setup();
    useManagedSessionStore.getState().addPendingAsk(ask());
    render(<ManagedQuestionPanel sessionId="s1" />);

    await user.click(screen.getByRole("button", { name: /Dismiss/ }));
    expect(resolveAskUser).toHaveBeenCalledWith("ask-user-0", "cancel", null);
  });

  it("shows only this session's oldest question + a pending count", () => {
    useManagedSessionStore.getState().addPendingAsk(ask({ correlationId: "ask-user-0" }));
    useManagedSessionStore
      .getState()
      .addPendingAsk(ask({ correlationId: "ask-user-1", question: "second?" }));
    useManagedSessionStore
      .getState()
      .addPendingAsk(ask({ correlationId: "ask-user-2", sessionId: "other" }));
    render(<ManagedQuestionPanel sessionId="s1" />);

    expect(screen.getByText("Which database should I use?")).toBeInTheDocument();
    expect(screen.queryByText("second?")).not.toBeInTheDocument();
    expect(screen.getByText("+1 more")).toBeInTheDocument();
  });
});

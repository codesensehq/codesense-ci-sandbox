// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const sendTextToSession = vi.fn<(...args: unknown[]) => Promise<void>>();
vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToSession: (...args: unknown[]) => sendTextToSession(...args),
}));

import { AgentChatSurface } from "./agent-chat-surface";
import {
  useAgentChatStore,
  resetAgentChatStoreForTests,
} from "@/shared/stores/use-agent-chat-store";

describe("AgentChatSurface (AC-7) @P1", () => {
  beforeEach(() => {
    resetAgentChatStoreForTests();
    sendTextToSession.mockReset();
    sendTextToSession.mockResolvedValue(undefined);
  });

  it("renders the transcript with both roles", () => {
    useAgentChatStore.getState().appendMessage("s1", "agent", "from the agent");
    useAgentChatStore.getState().appendMessage("s1", "user", "from me");
    render(<AgentChatSurface sessionId="s1" canSend />);
    const transcript = screen.getByTestId("agent-chat-transcript");
    expect(transcript.querySelector('[data-role="agent"]')?.textContent).toBe("from the agent");
    expect(transcript.querySelector('[data-role="user"]')?.textContent).toBe("from me");
  });

  it("submits into the BOUND session ({ submit }, raw message) and appends the user message only after it resolves", async () => {
    const user = userEvent.setup();
    render(<AgentChatSurface sessionId="s1" canSend />);
    await user.type(screen.getByLabelText("Message the agent"), "publish please");
    await user.click(screen.getByLabelText("Send message"));

    // SW-GS-23: RAW message (no hand-appended \r — submit_pty_line owns the Enter),
    // via the reliable-submit path, targeting the bound session (not the active tab).
    expect(sendTextToSession).toHaveBeenCalledWith("s1", "publish please", {
      submit: true,
      focusVisibleTerminal: false,
    });
    // Mirrored into the transcript + box cleared, only after the send resolves.
    await waitFor(() => {
      const msgs = useAgentChatStore.getState().transcripts.s1;
      expect(msgs.at(-1)).toMatchObject({ role: "user", text: "publish please" });
    });
    expect(screen.getByLabelText("Message the agent")).toHaveValue("");
  });

  it("SW-GS-23: a failed submit preserves the text and surfaces an error — no phantom user message", async () => {
    sendTextToSession.mockRejectedValue(new Error("write_pty: session not found: s1"));
    const user = userEvent.setup();
    render(<AgentChatSurface sessionId="s1" canSend />);
    await user.type(screen.getByLabelText("Message the agent"), "publish please");
    await user.click(screen.getByLabelText("Send message"));

    // Error surfaced; the composed text is preserved (box NOT cleared).
    await screen.findByRole("alert");
    expect(screen.getByLabelText("Message the agent")).toHaveValue("publish please");
    // No phantom "sent" entry recorded.
    expect(useAgentChatStore.getState().transcripts.s1).toBeUndefined();
  });

  it("SW-GS-23: does not double-send while a send is in flight", async () => {
    let resolve!: () => void;
    sendTextToSession.mockReturnValue(new Promise<void>((r) => (resolve = () => r())));
    const user = userEvent.setup();
    render(<AgentChatSurface sessionId="s1" canSend />);
    await user.type(screen.getByLabelText("Message the agent"), "hi");
    await user.click(screen.getByLabelText("Send message"));
    // A second click while the first send is pending is a no-op.
    await user.click(screen.getByLabelText("Send message"));
    expect(sendTextToSession).toHaveBeenCalledTimes(1);
    resolve();
  });

  it("SW-GS-23: does not double-send on two synchronous Enter keydowns (sendingRef guard)", () => {
    let resolve!: () => void;
    sendTextToSession.mockReturnValue(new Promise<void>((r) => (resolve = () => r())));
    render(<AgentChatSurface sessionId="s1" canSend />);
    const box = screen.getByLabelText("Message the agent");
    fireEvent.change(box, { target: { value: "hi" } });
    // Two Enter keydowns in ONE tick — the `sending` state flag can't guard this
    // (both closures read the pre-update value); the synchronous `sendingRef` does.
    fireEvent.keyDown(box, { key: "Enter" });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(sendTextToSession).toHaveBeenCalledTimes(1);
    resolve();
  });

  it("SW-GS-23: does not wipe a fresh draft typed during an in-flight send", async () => {
    let resolve!: () => void;
    sendTextToSession.mockReturnValue(new Promise<void>((r) => (resolve = () => r())));
    render(<AgentChatSurface sessionId="s1" canSend />);
    const box = screen.getByLabelText("Message the agent");
    fireEvent.change(box, { target: { value: "first" } });
    fireEvent.keyDown(box, { key: "Enter" }); // send "first" (now in flight)
    // The composer stays editable — the user starts a new draft mid-submit.
    fireEvent.change(box, { target: { value: "second draft" } });
    resolve();
    // "first" is recorded; the interim draft is preserved (not cleared).
    await waitFor(() => {
      expect(useAgentChatStore.getState().transcripts.s1?.at(-1)).toMatchObject({ text: "first" });
    });
    expect(box).toHaveValue("second draft");
  });

  // SW-WV-F6 — the render-tripwire footnote suffix on the bubble.
  it("renders a [doc] suffix for a drill_doc-targeted message", () => {
    useAgentChatStore.getState().appendMessage("s1", "agent", "companion", "drill_doc");
    render(<AgentChatSurface sessionId="s1" canSend />);
    const bubble = screen.getByTestId("agent-chat-transcript").querySelector('[data-role="agent"]');
    expect(bubble?.textContent).toContain("[doc]");
    expect(bubble?.textContent).not.toContain("[artifact]");
  });

  it("renders an [artifact] suffix for an artifact-targeted message", () => {
    useAgentChatStore.getState().appendMessage("s1", "agent", "deliverable", "artifact");
    render(<AgentChatSurface sessionId="s1" canSend />);
    const bubble = screen.getByTestId("agent-chat-transcript").querySelector('[data-role="agent"]');
    expect(bubble?.textContent).toContain("[artifact]");
    expect(bubble?.textContent).not.toContain("[doc]");
  });

  it("renders no suffix for a terminal-target or untagged message", () => {
    useAgentChatStore.getState().appendMessage("s1", "agent", "plain terminal");
    useAgentChatStore.getState().appendMessage("s1", "agent", "explicit terminal", "terminal");
    render(<AgentChatSurface sessionId="s1" canSend />);
    const transcript = screen.getByTestId("agent-chat-transcript");
    expect(transcript.textContent).not.toContain("[doc]");
    expect(transcript.textContent).not.toContain("[artifact]");
  });

  it("is gated with a reason when the caller says it can't send", () => {
    render(<AgentChatSurface sessionId={null} canSend={false} disabledReason="No live session." />);
    expect(screen.getByLabelText("Message the agent")).toBeDisabled();
    expect(screen.getByText("No live session.")).toBeInTheDocument();
  });

  // SW-DX-10 (AC-6): the "working…" liveness lamp.
  it("shows the working…/idle lamp only when a `working` flag is provided", () => {
    const { rerender } = render(<AgentChatSurface sessionId="s1" canSend />);
    // No flag → no lamp (git-sync and other callers are unaffected).
    expect(screen.queryByTestId("agent-liveness-lamp")).toBeNull();

    rerender(<AgentChatSurface sessionId="s1" canSend working={true} />);
    const lamp = screen.getByTestId("agent-liveness-lamp");
    expect(lamp).toHaveAttribute("data-working", "true");
    expect(lamp.textContent).toContain("working…");

    rerender(<AgentChatSurface sessionId="s1" canSend working={false} />);
    const idle = screen.getByTestId("agent-liveness-lamp");
    expect(idle).toHaveAttribute("data-working", "false");
    expect(idle.textContent).toContain("idle");
  });
});

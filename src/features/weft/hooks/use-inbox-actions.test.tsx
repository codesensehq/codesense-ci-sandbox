// SPDX-License-Identifier: MIT
import { render, screen, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AttentionRowDto } from "@/tauri/commands";

import type { InboxItem } from "../lib/inbox-model";
import { useInboxActions } from "./use-inbox-actions";

// Spy the transports so we can prove NONE of them fire on the exited path.
const runAutomationNow = vi.fn(() => Promise.resolve({}));
const wvSettleApproval = vi.fn(() => Promise.resolve(true));
vi.mock("@/tauri/commands", () => ({
  runAutomationNow: (...a: unknown[]) => runAutomationNow(...(a as [])),
  wvSettleApproval: (...a: unknown[]) => wvSettleApproval(...(a as [])),
}));
const sendTextToSession = vi.fn(() => Promise.resolve());
vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToSession: (...a: unknown[]) => sendTextToSession(...(a as [])),
}));

afterEach(() => {
  runAutomationNow.mockClear();
  wvSettleApproval.mockClear();
  sendTextToSession.mockClear();
});

/** An EXITED, automation-sourced question row: no live session, `source="automation"`. */
function exitedAutomationQuestion(): InboxItem {
  const row: AttentionRowDto = {
    id: 9,
    summary: "Which region should I deploy to?",
    source: "automation",
    ref: "auto-1",
    initiator: "agent",
    liveSessionId: null,
    need: "awaiting-answer",
  };
  return { key: "needs-you:9", row, need: "awaiting-answer", lane: "needs-you" };
}

/** Mounts the real hook and renders its reply surface for `item`. */
function ReplyHarness({ item }: { item: InboxItem }): React.JSX.Element {
  const handlers = useInboxActions("org:team", vi.fn());
  return <>{handlers.renderReply(item, () => {})}</>;
}

describe("useInboxActions — exited reply is HONEST (no phantom, no reply-less run)", () => {
  it("the exited automation-sourced Reply fires NO reply-less run and claims NO delivery", async () => {
    render(<ReplyHarness item={exitedAutomationQuestion()} />);
    await userEvent.type(screen.getByLabelText("Reply"), "us-east-1");
    await userEvent.click(screen.getByRole("button", { name: "Send reply" }));

    // It degrades honestly — the error explains the run has exited, and points at the CLI
    // (which is present on this agent-bearing row).
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(/exited/i));

    // The load-bearing guarantees:
    // 1) NO reply-less continuation run is fired (which could re-trigger real side effects).
    expect(runAutomationNow).not.toHaveBeenCalled();
    // 2) NOTHING is typed into a (non-existent) live session.
    expect(sendTextToSession).not.toHaveBeenCalled();
    // 3) NO "Sent" confirmation is shown — the button never claims a delivery that didn't happen.
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});

// SPDX-License-Identifier: MIT
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AttentionRowDto } from "@/tauri/commands";

import type { InboxItem } from "../lib/inbox-model";
import { InboxAgentReplyComposer as InboxReplyComposer } from "./inbox-agent-reply-composer";

const sendTextToSession = vi.fn(() => Promise.resolve());
vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToSession: (...a: unknown[]) => sendTextToSession(...(a as [])),
}));

afterEach(() => {
  sendTextToSession.mockReset().mockResolvedValue(undefined);
});

function makeItem(over: Partial<AttentionRowDto> = {}): InboxItem {
  const row: AttentionRowDto = {
    id: 7,
    summary: "Which region should I deploy to?",
    source: "run",
    ref: "run-42",
    initiator: "agent",
    liveSessionId: "sess-1",
    ...over,
  };
  return { key: `needs-you:7`, row, need: "awaiting-answer", lane: "needs-you" };
}

describe("InboxReplyComposer — durable reply + liveness contract (§2.3)", () => {
  it("labels the surface 'Reply' regardless of liveness — no runtime leak (§8.5)", () => {
    const { rerender } = render(
      <InboxReplyComposer
        item={makeItem()}
        live
        liveSessionId="sess-1"
        onRedispatch={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("Reply")).toBeInTheDocument();
    rerender(
      <InboxReplyComposer
        item={makeItem()}
        live={false}
        liveSessionId={null}
        onRedispatch={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    // Same "Reply" label after degrading to the exited path.
    expect(screen.getByLabelText("Reply")).toBeInTheDocument();
  });

  it("LIVE: types straight into the waiting agent and acknowledges it landed", async () => {
    render(
      <InboxReplyComposer
        item={makeItem()}
        live
        liveSessionId="sess-1"
        onRedispatch={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    await userEvent.type(screen.getByLabelText("Reply"), "us-east-1");
    await userEvent.click(screen.getByRole("button", { name: "Send reply" }));
    await waitFor(() =>
      expect(sendTextToSession).toHaveBeenCalledWith(
        "sess-1",
        "us-east-1",
        expect.objectContaining({ submit: true }),
      ),
    );
    expect(await screen.findByRole("status")).toHaveTextContent(/sent/i);
  });

  it("EXITED: the SAME Reply re-dispatches a continuation and acknowledges (never looks dead)", async () => {
    const onRedispatch = vi.fn(() => Promise.resolve());
    render(
      <InboxReplyComposer
        item={makeItem({ liveSessionId: null })}
        live={false}
        liveSessionId={null}
        onRedispatch={onRedispatch}
        onClose={vi.fn()}
      />,
    );
    await userEvent.type(screen.getByLabelText("Reply"), "us-east-1");
    await userEvent.click(screen.getByRole("button", { name: "Send reply" }));
    await waitFor(() => expect(onRedispatch).toHaveBeenCalledWith(expect.any(Object), "us-east-1"));
    // The exited path never routes into a live send.
    expect(sendTextToSession).not.toHaveBeenCalled();
    // §8.14 — a silent re-dispatch must not look like the button did nothing.
    expect(await screen.findByRole("status")).toHaveTextContent(/pick this up|sent/i);
  });

  it("EXITED: a failed re-dispatch surfaces the error and withholds the confirmation", async () => {
    const onRedispatch = vi.fn(() => Promise.reject(new Error("no continuation")));
    render(
      <InboxReplyComposer
        item={makeItem({ liveSessionId: null })}
        live={false}
        liveSessionId={null}
        onRedispatch={onRedispatch}
        onClose={vi.fn()}
      />,
    );
    await userEvent.type(screen.getByLabelText("Reply"), "hello");
    await userEvent.click(screen.getByRole("button", { name: "Send reply" }));
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("does not send an empty reply", async () => {
    const onRedispatch = vi.fn(() => Promise.resolve());
    render(
      <InboxReplyComposer
        item={makeItem()}
        live
        liveSessionId="sess-1"
        onRedispatch={onRedispatch}
        onClose={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Send reply" }));
    expect(sendTextToSession).not.toHaveBeenCalled();
    expect(onRedispatch).not.toHaveBeenCalled();
  });
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@/test-utils";

// SW-AV-9: the composer sends through the DURABLE message path — `sendTextToSession`
// with `{ submit: true }` (→ `submit_pty_line`). Mock it so the seam test asserts the
// message reaches that path for the row's session id, and can drive success / failure.
const sendTextToSession = vi.fn();
vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToSession: (...args: unknown[]) => sendTextToSession(...args),
}));

import { WeftAnswerComposer } from "./weft-answer-composer";

describe("SW-AV-9 · WeftAnswerComposer", () => {
  beforeEach(() => {
    sendTextToSession.mockReset();
    sendTextToSession.mockResolvedValue(undefined);
  });

  it("lands focus on the input when it opens (the Answer lands-on-input contract)", () => {
    render(<WeftAnswerComposer sessionId="sess-1" canSend verb="Answer" onClose={vi.fn()} />);
    expect(document.activeElement).toBe(screen.getByTestId("weft-answer-composer-input"));
  });

  it("send routes the message through the durable path for the row's session id and confirms", async () => {
    render(<WeftAnswerComposer sessionId="sess-1" canSend verb="Answer" onClose={vi.fn()} />);
    const input = screen.getByTestId("weft-answer-composer-input");
    fireEvent.change(input, { target: { value: "please rebase onto main" } });
    fireEvent.click(screen.getByTestId("weft-answer-composer-send"));

    await waitFor(() =>
      expect(sendTextToSession).toHaveBeenCalledWith(
        "sess-1",
        "please rebase onto main",
        expect.objectContaining({ submit: true }),
      ),
    );
    // The confirmation renders only after a confirmed landing.
    expect(await screen.findByTestId("weft-answer-composer-sent")).toBeInTheDocument();
  });

  it("Enter submits (Shift+Enter does not)", async () => {
    render(<WeftAnswerComposer sessionId="sess-1" canSend verb="Steer" onClose={vi.fn()} />);
    const input = screen.getByTestId("weft-answer-composer-input");
    fireEvent.change(input, { target: { value: "hold on" } });

    fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
    expect(sendTextToSession).not.toHaveBeenCalled();

    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(sendTextToSession).toHaveBeenCalledTimes(1));
  });

  it("a gone target disables send with an explanation — never a silent send", () => {
    render(
      <WeftAnswerComposer
        sessionId={null}
        canSend={false}
        disabledReason="This agent's session has ended — reopen it to send a message."
        verb="Answer"
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("weft-answer-composer-input")).toBeDisabled();
    expect(screen.getByTestId("weft-answer-composer-send")).toBeDisabled();
    expect(screen.getByTestId("weft-answer-composer-disabled")).toHaveTextContent(
      /session has ended/i,
    );
    // Even if a click is forced, the durable path is never invoked.
    fireEvent.click(screen.getByTestId("weft-answer-composer-send"));
    expect(sendTextToSession).not.toHaveBeenCalled();
  });

  it("a runtime send failure surfaces the error and withholds the confirmation (no phantom sent)", async () => {
    sendTextToSession.mockRejectedValue(new Error("session not found"));
    render(<WeftAnswerComposer sessionId="sess-1" canSend verb="Steer" onClose={vi.fn()} />);
    const input = screen.getByTestId("weft-answer-composer-input");
    fireEvent.change(input, { target: { value: "steer left" } });
    fireEvent.click(screen.getByTestId("weft-answer-composer-send"));

    expect(await screen.findByRole("alert")).toHaveTextContent(/session not found/i);
    expect(screen.queryByTestId("weft-answer-composer-sent")).not.toBeInTheDocument();
    // The composed text is preserved for a retry.
    expect(screen.getByTestId("weft-answer-composer-input")).toHaveValue("steer left");
  });

  it("does not send an empty / whitespace-only message", () => {
    render(<WeftAnswerComposer sessionId="sess-1" canSend verb="Answer" onClose={vi.fn()} />);
    fireEvent.change(screen.getByTestId("weft-answer-composer-input"), {
      target: { value: "   " },
    });
    expect(screen.getByTestId("weft-answer-composer-send")).toBeDisabled();
  });

  it("Escape closes the composer", () => {
    const onClose = vi.fn();
    render(<WeftAnswerComposer sessionId="sess-1" canSend verb="Answer" onClose={onClose} />);
    fireEvent.keyDown(screen.getByTestId("weft-answer-composer-input"), { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});

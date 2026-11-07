// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { InboxReplyComposer } from "./inbox-reply-composer";
import { useInboxComposeDraftStore } from "../stores/inbox-compose-draft-store";

beforeEach(() => {
  useInboxComposeDraftStore.setState({ drafts: {} });
});

describe("InboxReplyComposer (SW-IB1-C2 human-inbox wrapper)", () => {
  it("mounts the shared InboxCompose primitive at the pinned footer", () => {
    render(<InboxReplyComposer conversationId={42} onSend={async () => {}} />);
    const wrap = screen.getByTestId("inbox-reply-composer");
    expect(wrap).toBeInTheDocument();
    // The primitive carries its own canonical testids — the wrapper must
    // delegate rather than render a bespoke textarea.
    expect(screen.getByTestId("inbox-compose-textarea")).toBeInTheDocument();
    expect(screen.getByTestId("inbox-compose-send")).toBeInTheDocument();
  });

  it("uses the canonical 'Write a message...' placeholder by default (EXPERIENCE § Compose)", () => {
    render(<InboxReplyComposer conversationId={1} onSend={async () => {}} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textarea.placeholder).toBe("Write a message...");
  });

  it("passes custom placeholder through for the observe-only path", () => {
    render(
      <InboxReplyComposer
        conversationId={1}
        onSend={async () => {}}
        placeholder="This thread is addressed to an agent — Reply is read-only here."
        disabled
      />,
    );
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textarea.placeholder).toContain("addressed to an agent");
    expect(textarea).toBeDisabled();
  });

  it("keys the compose-draft slice by conversationId", () => {
    render(<InboxReplyComposer conversationId={99} onSend={async () => {}} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "a reply" } });
    fireEvent.blur(textarea);
    // Blur-persist writes to the compose draft store under `conv:${id}`.
    expect(useInboxComposeDraftStore.getState().getDraft("conv:99")?.text).toBe("a reply");
  });

  it("delegates send to the mount site and clears the body on success", async () => {
    const onSend = vi.fn(async () => {});
    render(<InboxReplyComposer conversationId={1} onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "ship it" } });
    fireEvent.click(screen.getByTestId("inbox-compose-send"));
    await waitFor(() => expect(onSend).toHaveBeenCalledWith("ship it"));
    // After the primitive sees `sending → idle`, its own effect clears body + draft.
    await waitFor(() => {
      expect((screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement).value).toBe("");
    });
  });

  it("surfaces failure through the primitive's destructive chip", async () => {
    const onSend = vi.fn(async () => {
      throw new Error("not sent");
    });
    render(<InboxReplyComposer conversationId={1} onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "attempt" } });
    fireEvent.click(screen.getByTestId("inbox-compose-send"));
    await waitFor(() => expect(onSend).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByTestId("inbox-compose-error")).toBeInTheDocument());
    // The body sticks around so the user can retry.
    expect((screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement).value).toBe(
      "attempt",
    );
  });
});

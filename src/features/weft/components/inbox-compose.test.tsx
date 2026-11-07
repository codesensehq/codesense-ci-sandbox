// SPDX-License-Identifier: MIT
import { useState } from "react";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { act, fireEvent, render, screen } from "@/test-utils";

import { useInboxComposeDraftStore } from "../stores/inbox-compose-draft-store";

import {
  INBOX_COMPOSE_COPY,
  InboxCompose,
  type InboxComposeProps,
  type InboxComposeSendState,
} from "./inbox-compose";

/**
 * SW-IB1-A5 — unit tests for the shared compose primitive.
 *
 * The primitive is pure render + text management; the mount site drives
 * `sendState` from its own Promise lifecycle. Tests simulate that mount
 * site with a `Harness` component so the sending → idle / sending → error
 * transitions happen for real and the primitive's post-send cleanup runs
 * in the same way production does.
 */

interface HarnessProps extends Partial<InboxComposeProps> {
  initialSendState?: InboxComposeSendState;
  onSend?: (body: string) => Promise<void>;
}

/**
 * The mount-site contract in miniature. `onSend` returning fulfilled flips
 * sendState sending → idle; rejected flips sending → error. The primitive
 * uses that transition to clear its body + refocus; the harness mirrors
 * what the push pane / human inbox will do once wired.
 */
function Harness({
  initialSendState = "idle",
  onSend = () => Promise.resolve(),
  draftKey = "test-key",
  placeholder = "Write a message...",
  ...rest
}: HarnessProps) {
  const [sendState, setSendState] = useState<InboxComposeSendState>(initialSendState);
  const handleSend = async (body: string) => {
    setSendState("sending");
    try {
      await onSend(body);
      setSendState("idle");
    } catch {
      setSendState("error");
    }
  };
  return (
    <InboxCompose
      {...rest}
      placeholder={placeholder}
      draftKey={draftKey}
      sendState={sendState}
      onSend={handleSend}
    />
  );
}

beforeEach(() => {
  useInboxComposeDraftStore.getState()._reset();
});

afterEach(() => {
  useInboxComposeDraftStore.getState()._reset();
});

describe("InboxCompose — autosize bounds", () => {
  it("respects the min-height floor (72px) and max-height ceiling (240px) via inline style", () => {
    render(<Harness />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textarea.style.minHeight).toBe("72px");
    expect(textarea.style.maxHeight).toBe("240px");
  });

  it("enables internal scroll inside the textarea when content exceeds the ceiling", () => {
    render(<Harness />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    // Stub scrollHeight to simulate a draft taller than the ceiling. jsdom
    // doesn't compute real layout, so we override the getter.
    Object.defineProperty(textarea, "scrollHeight", {
      configurable: true,
      get: () => 400,
    });
    fireEvent.change(textarea, { target: { value: "a very long multiline draft" } });
    // The JS fallback flips overflow-y to auto when scrollHeight > MAX; the
    // native field-sizing path never hits this branch and keeps overflow
    // hidden until the browser's scrollbar kicks in. We assert the fallback
    // ran because jsdom doesn't ship CSS.supports('field-sizing: content').
    expect(textarea.style.overflowY).toBe("auto");
    expect(textarea.style.height).toBe("240px");
  });

  it("keeps the textarea at the floor when content is short", () => {
    render(<Harness />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    Object.defineProperty(textarea, "scrollHeight", {
      configurable: true,
      get: () => 30,
    });
    fireEvent.change(textarea, { target: { value: "hi" } });
    expect(textarea.style.height).toBe("72px");
    expect(textarea.style.overflowY).toBe("hidden");
  });
});

describe("InboxCompose — draft persistence", () => {
  it("writes to the compose-draft store on blur, keyed by draftKey", () => {
    render(<Harness draftKey="conv-42" />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "pending thought" } });
    fireEvent.blur(textarea);
    const stored = useInboxComposeDraftStore.getState().getDraft("conv-42");
    expect(stored?.text).toBe("pending thought");
  });

  it("hydrates from the store on mount for the given draftKey", () => {
    useInboxComposeDraftStore.getState().setDraft("conv-99", "unfinished reply");
    render(<Harness draftKey="conv-99" />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textarea.value).toBe("unfinished reply");
  });

  it("preserves per-target drafts across unmount / remount — tab A → B → A restores A", () => {
    const { unmount } = render(<Harness draftKey="target-A" />);
    const textareaA = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textareaA, { target: { value: "draft for A" } });
    fireEvent.blur(textareaA);
    unmount();

    const r2 = render(<Harness draftKey="target-B" />);
    const textareaB = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textareaB.value).toBe("");
    fireEvent.change(textareaB, { target: { value: "draft for B" } });
    fireEvent.blur(textareaB);
    r2.unmount();

    render(<Harness draftKey="target-A" />);
    const textareaAgain = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textareaAgain.value).toBe("draft for A");
  });

  it("clears the stored draft after a successful send", async () => {
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(<Harness draftKey="post-send" onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "ship it" } });

    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    });

    expect(onSend).toHaveBeenCalledWith("ship it");
    expect(textarea.value).toBe("");
    expect(useInboxComposeDraftStore.getState().getDraft("post-send")).toBeNull();
  });
});

describe("InboxCompose — keyboard contract", () => {
  it("sends on ⌘↩ (meta+Enter)", async () => {
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "hi" } });
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    });
    expect(onSend).toHaveBeenCalledWith("hi");
  });

  it("sends on Ctrl+Enter too (cross-platform muscle memory)", async () => {
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "hi" } });
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter", ctrlKey: true });
    });
    expect(onSend).toHaveBeenCalledWith("hi");
  });

  it("does not send on bare Enter — Enter inserts a newline", () => {
    const onSend = vi.fn();
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "hi" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not send on Shift+Enter — also inserts a newline", () => {
    const onSend = vi.fn();
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "hi" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not send ⌘↩ from an empty body (empty-send guard)", () => {
    const onSend = vi.fn();
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not send ⌘↩ from a whitespace-only body — trim-to-empty is empty", () => {
    const onSend = vi.fn();
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "   \n  " } });
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("sends the trimmed body (leading/trailing whitespace stripped)", async () => {
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "  payload  " } });
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    });
    expect(onSend).toHaveBeenCalledWith("payload");
  });

  it("ignores ⌘↩ while an IME composition is active (CJK input safety)", () => {
    const onSend = vi.fn();
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "こ" } });
    // isComposing on the native event — React forwards it through
    // nativeEvent. A ⌘↩ press during composition must commit the IME, not
    // the send.
    fireEvent.keyDown(textarea, {
      key: "Enter",
      metaKey: true,
      // React synthetic events proxy to a native KeyboardEvent whose
      // isComposing getter we patch via keyCode 229 fallback.
      keyCode: 229,
    });
    expect(onSend).not.toHaveBeenCalled();
  });
});

describe("InboxCompose — Esc behavior", () => {
  it("Esc with dirty body shows the discard prompt chip (not silent)", () => {
    render(<Harness />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "half-written" } });
    fireEvent.keyDown(textarea, { key: "Escape" });
    const prompt = screen.getByTestId("inbox-compose-discard-prompt");
    expect(prompt).toHaveTextContent(INBOX_COMPOSE_COPY.discardPrompt);
  });

  it("Esc on an empty body is silent — no discard prompt renders", () => {
    render(<Harness />);
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(screen.queryByTestId("inbox-compose-discard-prompt")).not.toBeInTheDocument();
  });

  it("Esc from an open discard prompt dismisses the prompt (keeps the draft)", () => {
    render(<Harness />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "keep this" } });
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(screen.getByTestId("inbox-compose-discard-prompt")).toBeInTheDocument();
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(screen.queryByTestId("inbox-compose-discard-prompt")).not.toBeInTheDocument();
    expect(textarea.value).toBe("keep this");
  });

  it("⌘⇧D from the discard prompt clears textarea, fires onDiscard, and removes the store record", () => {
    const onDiscard = vi.fn();
    render(<Harness draftKey="discard-k" onDiscard={onDiscard} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "nevermind" } });
    fireEvent.blur(textarea);
    expect(useInboxComposeDraftStore.getState().getDraft("discard-k")?.text).toBe("nevermind");

    fireEvent.focus(textarea);
    fireEvent.keyDown(textarea, { key: "Escape" });
    fireEvent.keyDown(textarea, { key: "D", metaKey: true, shiftKey: true });

    expect(textarea.value).toBe("");
    expect(onDiscard).toHaveBeenCalledTimes(1);
    expect(useInboxComposeDraftStore.getState().getDraft("discard-k")).toBeNull();
  });
});

describe("InboxCompose — error chip", () => {
  it("renders the error chip with the exact canonical copy when sendState=error", () => {
    render(<Harness initialSendState="error" />);
    const chip = screen.getByTestId("inbox-compose-error");
    expect(chip).toHaveTextContent(INBOX_COMPOSE_COPY.sendError);
    // `role="alert"` is self-live (implicit assertive) — we do NOT also set
    // `aria-live` to avoid cross-signaling. See H4 in the review triage.
    expect(chip).toHaveAttribute("role", "alert");
  });

  it("auto-dismisses the error chip on the next keypress", () => {
    render(<Harness initialSendState="error" />);
    expect(screen.getByTestId("inbox-compose-error")).toBeInTheDocument();
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.keyDown(textarea, { key: "a" });
    expect(screen.queryByTestId("inbox-compose-error")).not.toBeInTheDocument();
  });

  it("hides the error chip when sendState=idle", () => {
    render(<Harness />);
    expect(screen.queryByTestId("inbox-compose-error")).not.toBeInTheDocument();
  });

  it("renders the error chip with a destructive-tinted surface + inline chip radius (DESIGN § compose error)", () => {
    render(<Harness initialSendState="error" />);
    const chip = screen.getByTestId("inbox-compose-error");
    expect(chip.className).toContain("bg-destructive/10");
    expect(chip.className).toContain("text-destructive");
    // happy-dom drops unknown `var(...)` tokens from parsed style attrs, so
    // we assert on the padding classes that make the element read as a
    // chip (surface + inline padding) and trust the DESIGN-token binding
    // for the radius via visual review.
    expect(chip.className).toMatch(/px-\d/);
    expect(chip.className).toMatch(/py-\d/);
  });

  it("surfaces the error chip after an onSend rejection flips sendState sending → error", async () => {
    const onSend = vi.fn().mockRejectedValue(new Error("boom"));
    render(<Harness onSend={onSend} />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "will fail" } });
    await act(async () => {
      fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    });
    expect(screen.getByTestId("inbox-compose-error")).toBeInTheDocument();
    // The body stays — the user still has their text to retry with.
    expect(textarea.value).toBe("will fail");
  });
});

describe("InboxCompose — send button state", () => {
  it("renders the send button with accessible name 'Send message'", () => {
    render(<Harness />);
    const btn = screen.getByTestId("inbox-compose-send");
    expect(btn).toHaveAttribute("aria-label", "Send message");
  });

  it("disables the send button when the body is empty (opacity 0.5, pointer-events none)", () => {
    render(<Harness />);
    const btn = screen.getByTestId("inbox-compose-send") as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(btn.className).toContain("opacity-50");
    expect(btn.className).toContain("pointer-events-none");
  });

  it("enables the send button when the trimmed body is non-empty", () => {
    render(<Harness />);
    const btn = screen.getByTestId("inbox-compose-send") as HTMLButtonElement;
    const textarea = screen.getByTestId("inbox-compose-textarea");
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(btn.disabled).toBe(false);
  });

  it("swaps the label to 'Sending...' and marks the textarea readOnly during sendState=sending", () => {
    render(<Harness initialSendState="sending" />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    expect(textarea.readOnly).toBe(true);
    const btn = screen.getByTestId("inbox-compose-send");
    expect(btn).toHaveTextContent(INBOX_COMPOSE_COPY.sendSending);
  });

  it("respects the `disabled` prop — the send button never enables, textarea is disabled", () => {
    render(<Harness disabled />);
    const textarea = screen.getByTestId("inbox-compose-textarea") as HTMLTextAreaElement;
    const btn = screen.getByTestId("inbox-compose-send") as HTMLButtonElement;
    expect(textarea.disabled).toBe(true);
    fireEvent.change(textarea, { target: { value: "attempted" } });
    expect(btn.disabled).toBe(true);
  });
});

describe("InboxCompose — a11y", () => {
  it("gives the textarea an accessible label matching the placeholder", () => {
    render(<Harness placeholder="Message Winston..." />);
    const textarea = screen.getByLabelText("Message Winston...");
    expect(textarea).toBeInTheDocument();
  });
});

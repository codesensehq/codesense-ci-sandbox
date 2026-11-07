// SPDX-License-Identifier: MIT
/**
 * SW-IB1-C2 — the human inbox's reply composer, now a thin wrapper around the
 * shared {@link InboxCompose} primitive (SW-IB1-A5).
 *
 * The file keeps its name as the mount site for the conversation view — the
 * COMPONENT-INVENTORY § 3.5 target — so downstream importers (and any future
 * machine-badge / mentions wiring) resolve against a stable path. The compose
 * UX itself (textarea autosize, blur-persist, ⌘↩ send, dirty-Esc discard
 * prompt, destructive error chip) is the primitive; this wrapper only plumbs
 * the human-inbox send contract through:
 *
 *   - `conversationId` → stable per-conversation draft key.
 *   - `onSend(body)`   → the mount site's send verb (`replyAsHuman` wired by
 *     {@link InboxConversationView}). The wrapper drives `sendState` from the
 *     returned promise so the primitive's `idle → sending → idle | error`
 *     cycle is honest without the mount site re-wiring state.
 *   - `onDiscard?`     → fired after the user confirms discard via `⌘⇧D` on
 *     the primitive; lets the view clear its own stale status chip.
 *   - `placeholder`    → defaults to "Write a message..." per
 *     EXPERIENCE § Voice and Tone § Compose placeholders.
 *   - `disabled`       → mirrors the `replyDisabled` thread (observe-only).
 *
 * No sender-based filter, no permission check: the human inbox is the user's
 * own mailbox and every router that posts here is already authorized at the
 * backend. The compose primitive makes the same promise — the only send-gate
 * is empty-body (DESIGN § compose.disabled).
 */

import { useCallback, useState } from "react";

import { InboxCompose, type InboxComposeSendState } from "./inbox-compose";

export interface InboxReplyComposerProps {
  /** Opaque conversation id — becomes the `draftKey` for the compose slice so
   *  a switched-away thread's partial reply survives the round trip. */
  conversationId: number | string;
  /** The mount site's send verb. Resolves on success; rejects on failure. The
   *  wrapper flips `sendState` from these transitions — the primitive's own
   *  post-success cleanup (clear body, refocus) runs when `sendState` returns
   *  to `idle`. */
  onSend: (body: string) => Promise<void>;
  /** Observe-only thread (e.g. a to-agents thread the human is watching).
   *  The textarea renders in the disabled state and Send is non-interactive. */
  disabled?: boolean;
  /** Textarea placeholder (and aria-label). Defaults to the human-inbox
   *  canonical copy from EXPERIENCE § Compose placeholders. */
  placeholder?: string;
  /** Fired after the user confirms discard. Lets the view clear any post-send
   *  chip ("Sent" / "Not sent: …") without reinventing its own flow. */
  onDiscard?: () => void;
  className?: string;
}

/**
 * Default placeholder — canonical from EXPERIENCE § Voice and Tone §
 * Compose. "Write a message..." (not "Reply…") because the human inbox's
 * compose is a universal "write to the thread" surface; a reply is just the
 * common case.
 */
const DEFAULT_PLACEHOLDER = "Write a message...";

export function InboxReplyComposer({
  conversationId,
  onSend,
  disabled = false,
  placeholder = DEFAULT_PLACEHOLDER,
  onDiscard,
  className,
}: InboxReplyComposerProps): React.JSX.Element {
  const [sendState, setSendState] = useState<InboxComposeSendState>("idle");

  // The primitive hands us the trimmed body; we drive the mount site's send
  // and reflect the outcome back through `sendState`. On success the
  // primitive's own effect clears the textarea + draft and refocuses — we
  // never duplicate that work here.
  const handleSend = useCallback(
    async (body: string) => {
      setSendState("sending");
      try {
        await onSend(body);
        setSendState("idle");
      } catch {
        // The primitive renders the destructive chip from the shared copy
        // (COMPONENT-INVENTORY § 4.2); the mount site may ALSO surface its
        // own status chip (replyAsHuman's structured error). Both paths are
        // honest — the primitive's chip is the compose-local signal, the
        // view's status chip the per-send message.
        setSendState("error");
      }
    },
    [onSend],
  );

  return (
    <div data-testid="inbox-reply-composer" className={className}>
      <InboxCompose
        placeholder={placeholder}
        draftKey={`conv:${conversationId}`}
        disabled={disabled}
        sendState={sendState}
        onSend={handleSend}
        onDiscard={onDiscard}
      />
    </div>
  );
}

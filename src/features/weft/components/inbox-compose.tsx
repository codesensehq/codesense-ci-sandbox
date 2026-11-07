// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { cn } from "@/lib/utils";

import { useInboxComposeDraftStore } from "../stores/inbox-compose-draft-store";

/**
 * SW-IB1-A5 — the one compose box both inboxes mount (push pane + human
 * inbox). Pure render + text management: autosizes 72→240px, blur-persists
 * to the compose-draft store, sends on `⌘↩`, discards with a prompt on
 * `Esc` when dirty, surfaces send failure as an inline destructive chip
 * (no toast, no spinner). The mount site owns the actual send call — this
 * primitive just hands it the trimmed body and reflects the outcome.
 *
 * Enable-don't-restrict: the compose accepts any text an agent or human
 * wants to send. The only send-gate is the empty-body guard (DESIGN §
 * compose.disabled); there is NO content inspection, no permission wait,
 * no role check inside this primitive.
 */

/** Pixel height floor for the textarea (DESIGN § compose min-height). */
const MIN_HEIGHT_PX = 72;
/** Pixel height ceiling before internal scroll (DESIGN § compose max-height). */
const MAX_HEIGHT_PX = 240;

/** Exact microcopy from EXPERIENCE § Voice and Tone § Compose. One canonical
 *  string per surface — alternates do not ship. */
export const INBOX_COMPOSE_COPY = {
  sendDefault: "Send ⌘↩",
  sendSending: "Sending...",
  sendError: "Couldn't send. Press ⌘↩ to try again.",
  discardPrompt: "Discard draft? · ⌘⇧D discards, Esc keeps",
} as const;

export type InboxComposeSendState = "idle" | "sending" | "error";

export interface InboxComposeProps {
  /** Textarea placeholder; also the textarea's `aria-label` (the primitive's
   *  mount site owns the per-surface copy — see EXPERIENCE § Compose
   *  placeholders). */
  placeholder: string;
  /** Opaque per-target key for the compose-draft slice. Push pane uses
   *  `${tabId}:${sessionId}`; human inbox uses `${conversationId}` or
   *  `${newDraftId}`. Changing `draftKey` across remounts hydrates the
   *  textarea with the new target's stored body. */
  draftKey: string;
  /** Null-identity push pane / unresolved target → the compose section
   *  mounts in a disabled state (EXPERIENCE § Disabled). */
  disabled?: boolean;
  /** `idle` | `sending` | `error` — the mount site drives this from its own
   *  send Promise lifecycle. */
  sendState: InboxComposeSendState;
  /** Pure send verb — hands the trimmed body to the router and resolves
   *  (success) or rejects (failure). The primitive does not retry; the
   *  caller owns retry wiring. */
  onSend: (body: string) => Promise<void>;
  /** Fired when the user confirms discard via `⌘⇧D`. The primitive always
   *  clears its own textarea + stored draft; this prop lets the mount site
   *  react (close the composer, refocus the list, etc.). */
  onDiscard?: () => void;
  /** Optional className forwarded to the outer wrapper for mount-site
   *  layout (flex children, border edges). */
  className?: string;
}

/**
 * Autosize path. We measure `scrollHeight` on every change and clamp to
 * [MIN, MAX]; the `field-sizing: content` CSS value (where the browser
 * supports it) layers on as a progressive enhancement but the JS path owns
 * the clamp so the behavior is deterministic across engines. jsdom's
 * `CSS.supports` is lenient enough that a runtime feature-check is
 * unreliable in tests; the JS clamp is cheap and runs synchronously inside
 * `useLayoutEffect` so the user never sees a one-frame overshoot.
 */

export function InboxCompose({
  placeholder,
  draftKey,
  disabled = false,
  sendState,
  onSend,
  onDiscard,
  className,
}: InboxComposeProps): React.JSX.Element {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const [body, setBody] = useState<string>(() => {
    // Hydrate from the store on initial render so the textarea mounts with
    // the saved body already present (no flicker + no post-mount effect).
    return useInboxComposeDraftStore.getState().getDraft(draftKey)?.text ?? "";
  });
  const [showDiscardPrompt, setShowDiscardPrompt] = useState(false);
  // The error chip is driven by the mount site's `sendState` AND auto-
  // dismisses on the next keypress. We track a local "dismissed" flag so a
  // user's keystroke silences the chip without the mount site having to
  // reset `sendState`.
  const [errorDismissed, setErrorDismissed] = useState(false);

  // Re-hydrate when `draftKey` changes within a single mount lifecycle (a
  // tab switch that keeps the primitive mounted but points it at a new
  // target). Also catches the case where another surface mutated the slot.
  useEffect(() => {
    const stored = useInboxComposeDraftStore.getState().getDraft(draftKey)?.text ?? "";
    setBody(stored);
    setShowDiscardPrompt(false);
    setErrorDismissed(false);
  }, [draftKey]);

  // JS autosize — measure scrollHeight, clamp, apply inline. Runs
  // synchronously via `useLayoutEffect` so the user never sees a one-frame
  // overshoot before the clamp.
  useLayoutEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    // Reset height so shrinking from a tall draft to a short one measures
    // the current scrollHeight, not the stale tall one.
    el.style.height = "auto";
    const measured = el.scrollHeight;
    const clamped = Math.min(Math.max(measured, MIN_HEIGHT_PX), MAX_HEIGHT_PX);
    el.style.height = `${clamped}px`;
    // When content exceeds the ceiling, allow internal scroll; otherwise
    // keep it hidden so the clamp doesn't look like an artificial cap.
    el.style.overflowY = measured > MAX_HEIGHT_PX ? "auto" : "hidden";
  }, [body]);

  // When the send resolves and the mount site has cleared `sendState` back
  // to idle, return focus to the textarea (AC: "focus returns to the
  // textarea"). The mount site also clears the draft on success.
  const prevSendStateRef = useRef<InboxComposeSendState>(sendState);
  useEffect(() => {
    const prev = prevSendStateRef.current;
    prevSendStateRef.current = sendState;
    if (prev === "sending" && sendState === "idle") {
      // Successful send — the mount site is expected to call `onSend` which
      // we clear-after on. Clear local body + store, refocus.
      setBody("");
      useInboxComposeDraftStore.getState().clearDraft(draftKey);
      textareaRef.current?.focus();
    }
    if (sendState === "error") {
      // Re-arm the chip's dismissed flag so a fresh error from the mount
      // site re-shows the chip even if the user's prior keystroke silenced
      // the previous error.
      setErrorDismissed(false);
    }
  }, [sendState, draftKey]);

  const trimmed = body.trim();
  const canSend = trimmed.length > 0 && !disabled && sendState !== "sending";
  const sending = sendState === "sending";
  const errorVisible = sendState === "error" && !errorDismissed;

  const commitSend = useCallback(() => {
    if (!canSend) return;
    // The primitive hands the trimmed body to the mount site's Promise and
    // forgets. The mount site drives `sendState`; this effect-side pair
    // (send-state transitions above) handles the post-success cleanup.
    void onSend(trimmed);
  }, [canSend, onSend, trimmed]);

  const handleDiscard = useCallback(() => {
    setBody("");
    useInboxComposeDraftStore.getState().clearDraft(draftKey);
    setShowDiscardPrompt(false);
    onDiscard?.();
    textareaRef.current?.focus();
  }, [draftKey, onDiscard]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Any keystroke silences a stale error chip (AC: "chip auto-dismisses on
    // the next keypress"). We check first so modifier-only presses still
    // dismiss, which matches a user's intuition that they've "moved on."
    if (errorVisible) setErrorDismissed(true);

    const isCmd = e.metaKey || e.ctrlKey;
    const isShift = e.shiftKey;
    // IME composition safety — if the user is mid-composition (CJK, dead
    // keys), Enter commits the IME, not the send. React doesn't type the
    // field so we go through the native event.
    const isComposing = (e.nativeEvent as KeyboardEvent).isComposing || e.keyCode === 229;

    // `⌘⇧D` always discards when the prompt is showing (per the dirty-Esc
    // flow). We also honor it bare-key when the body is dirty — a user who
    // knows the shortcut should not need to tap Esc first.
    if (isCmd && isShift && (e.key === "d" || e.key === "D")) {
      if (showDiscardPrompt || trimmed.length > 0) {
        e.preventDefault();
        handleDiscard();
      }
      return;
    }

    // `⌘↩` sends. Bare Enter / Shift+Enter insert newlines (default browser
    // behavior — we just don't preventDefault). IME composition eats the
    // send so a CJK commit never fires a surprise message.
    if (isCmd && e.key === "Enter" && !isComposing) {
      e.preventDefault();
      commitSend();
      return;
    }

    if (e.key === "Escape") {
      if (showDiscardPrompt) {
        // Esc from the prompt KEEPS the draft and closes the prompt.
        e.preventDefault();
        setShowDiscardPrompt(false);
        return;
      }
      if (trimmed.length > 0) {
        // Esc with dirty body shows the prompt (not a modal — a chip).
        e.preventDefault();
        setShowDiscardPrompt(true);
        return;
      }
      // Esc on an empty body is silent — the mount site may wire its own
      // handler (close the pane, blur) at a parent focus level.
      return;
    }
  };

  const handleBlur = (text: string) => {
    // Blur-persist. Empty body == a user who cleared the slot; the slice
    // treats an empty write as a clear, so the textarea doesn't rehydrate
    // onto a ghost empty-string record.
    useInboxComposeDraftStore.getState().setDraft(draftKey, text);
  };

  const sendDisabled = !canSend;

  return (
    <div
      data-testid="inbox-compose"
      className={cn("flex flex-col gap-2", className)}
      style={{ padding: "var(--ib-space-4, 12px)" }}
    >
      {errorVisible && (
        // Destructive-tinted chip (DESIGN § compose error): low-mix
        // destructive surface + full-strength destructive foreground, chip
        // radius + padding so it reads as a pill, not a bare error line.
        // `role="alert"` carries its own implicit live-region politeness —
        // we don't add `aria-live` to avoid cross-signaling (an error IS an
        // assertive announcement).
        <p
          role="alert"
          data-testid="inbox-compose-error"
          className="inline-flex items-center bg-destructive/10 px-2 py-1 text-xs text-destructive"
          style={{ borderRadius: "var(--ib-radius-sm, 4px)" }}
        >
          {INBOX_COMPOSE_COPY.sendError}
        </p>
      )}
      {showDiscardPrompt && (
        // The discard prompt is a status chip (not an alert — it describes
        // a UI state the user initiated by pressing Esc). Muted-surface so
        // it doesn't compete with the destructive error chip when both
        // conditions theoretically overlap.
        <p
          role="status"
          data-testid="inbox-compose-discard-prompt"
          className="inline-flex items-center bg-muted px-2 py-1 text-xs text-muted-foreground"
          style={{ borderRadius: "var(--ib-radius-sm, 4px)" }}
        >
          {INBOX_COMPOSE_COPY.discardPrompt}
        </p>
      )}
      <textarea
        ref={textareaRef}
        data-testid="inbox-compose-textarea"
        aria-label={placeholder}
        placeholder={placeholder}
        value={body}
        readOnly={sending}
        disabled={disabled}
        onChange={(e) => {
          setBody(e.target.value);
          if (showDiscardPrompt) setShowDiscardPrompt(false);
        }}
        onKeyDown={handleKeyDown}
        onBlur={(event) => {
          const field = event.currentTarget;
          commitTextEditOnBlur(field, () => handleBlur(field.value));
        }}
        rows={1}
        className={cn(
          "w-full resize-none border border-border bg-background px-3 py-2 text-[13px] leading-[18px] font-normal outline-none placeholder:text-muted-foreground focus-visible:border-ring",
        )}
        style={{
          borderRadius: "var(--ib-radius-md, 6px)",
          minHeight: `${MIN_HEIGHT_PX}px`,
          maxHeight: `${MAX_HEIGHT_PX}px`,
        }}
      />
      <div className="flex items-center justify-end">
        <button
          type="button"
          data-testid="inbox-compose-send"
          aria-label="Send message"
          onClick={commitSend}
          disabled={sendDisabled}
          className={cn(
            "inline-flex h-7 items-center bg-accent px-3 text-[12px] font-medium text-foreground transition-opacity",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            sendDisabled && "pointer-events-none opacity-50",
          )}
          style={{ borderRadius: "var(--ib-radius-sm, 4px)" }}
          title={sending ? INBOX_COMPOSE_COPY.sendSending : "Send message (⌘↩)"}
        >
          <span
            className="transition-opacity"
            style={{ transitionDuration: "120ms" }}
            data-sending={sending ? "true" : undefined}
          >
            {sending ? INBOX_COMPOSE_COPY.sendSending : INBOX_COMPOSE_COPY.sendDefault}
          </span>
        </button>
      </div>
    </div>
  );
}

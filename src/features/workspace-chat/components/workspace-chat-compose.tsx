// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

/** Error strings the engine (`workspace_chat_send`) carries. SW-WC-2 handoff:
 *  the Tauri path emits `workspace archived` as its one distinguished refusal;
 *  anything else is treated as generic. */
export const ARCHIVED_ERROR = "workspace archived";
export const NO_ENGINE_ERROR = "workspace chat engine not installed";

export interface WorkspaceChatComposeProps {
  placeholder?: string;
  draft?: { text: string; setText: React.Dispatch<React.SetStateAction<string>> };
  displayName: string;
  sending: boolean;
  error: string | null;
  onDismissError: () => void;
  onSend: (text: string) => Promise<void>;
}

/**
 * SW-WC-3 — the human compose box (markdown-only).
 *
 * - **Markdown-only in v1** per OQ-11.4 — no attachment picker, no onClick
 *   picker, no reply affordance. Shipped `InboxMessageContent` already
 *   handles live-dashboard HTML / sanitization, so chat benefits
 *   automatically without the human's compose having to produce it.
 * - Enter sends, Shift+Enter inserts a newline — matches both inbox and
 *   every other v1 compose box in the app.
 * - A send that returns `"workspace archived"` triggers the archive banner
 *   via the parent — this component surfaces the
 *   generic error; the pane itself swaps compose → banner when the archive
 *   flag flips.
 */
export function WorkspaceChatCompose({
  placeholder = "Say something to the crew…",
  draft,
  displayName,
  sending,
  error,
  onDismissError,
  onSend,
}: WorkspaceChatComposeProps): React.JSX.Element {
  const [localText, setLocalText] = useState("");
  const { text, setText } = draft ?? { text: localText, setText: setLocalText };
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    // Auto-grow — same cheap pattern the inbox compose uses.
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [text]);

  const trimmed = text.trim();
  const canSend = trimmed.length > 0 && !sending;

  const handleSend = useCallback(async () => {
    if (!canSend) return;
    const toSend = trimmed;
    try {
      await onSend(toSend);
      setText((current) => (current.trim() === toSend ? "" : current));
    } catch {
      // Error is already surfaced by the parent via the `error` prop; don't
      // clear the text so the user can retry/edit.
    }
  }, [canSend, onSend, trimmed, setText]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (error) onDismissError();
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void handleSend();
      }
    },
    [error, handleSend, onDismissError],
  );

  const errorMessage = error === ARCHIVED_ERROR ? null : error;

  return (
    <form
      className="wc-compose"
      data-testid="workspace-chat-compose"
      onSubmit={(e) => {
        e.preventDefault();
        void handleSend();
      }}
    >
      <div className="wc-compose__field">
        <div className="wc-compose__who">
          posting as <b>{displayName}</b>
        </div>
        <textarea
          ref={textareaRef}
          className="wc-compose__textarea"
          rows={2}
          value={text}
          placeholder={placeholder}
          aria-label="Compose message"
          disabled={sending}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
        />
        <div className="wc-compose__foot">
          <span className="wc-compose__hint">
            <span>
              <kbd>↵</kbd> send
            </span>
            <span>
              <kbd>⇧↵</kbd> newline
            </span>
          </span>
          {errorMessage && (
            <span className="wc-compose__error" role="alert">
              {errorMessage}
            </span>
          )}
          <button
            type="submit"
            className="wc-compose__send"
            disabled={!canSend}
            data-testid="workspace-chat-send"
          >
            <SendIcon />
            {sending ? "Sending…" : "Send"}
          </button>
        </div>
      </div>
    </form>
  );
}

function SendIcon(): React.JSX.Element {
  return (
    <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <path d="M3 10h12m0 0-4-4m4 4-4 4" />
    </svg>
  );
}

// SPDX-License-Identifier: MIT
import { Fragment, useLayoutEffect, useMemo, useRef } from "react";

import type { WorkspaceChatMessage } from "@/tauri/commands";

import type { ChatDensity } from "../lib/chat-density";
import { colourForHandle, initialForHandle } from "../lib/role-colour";

import { WorkspaceChatMessageRow } from "./workspace-chat-message-row";
import { WorkspaceChatSystemRow } from "./workspace-chat-system-row";

export interface ChatIdentity {
  name: string;
  color: string;
  initials: string;
  tag?: string;
  machine?: string;
}
export interface WorkspaceChatBodyProps {
  emptyState?: React.ReactNode;
  unreadAfter?: number;
  unreadExclude?: string;
  unreadLabel?: string;
  identify?: (from: string, fromName: string) => ChatIdentity;
  messages: WorkspaceChatMessage[] | null;
  loading: boolean;
  error: string | null;
  density: ChatDensity;
  /** HUMAN sentinel override — defaults to `"human"`. Tests inject a stub. */
  humanSentinel?: string;
}

/** Group runs of consecutive messages by the same `from` so the renderer
 *  can collapse the header on follow-ups. System rows always break a run. */
export function annotateMessages(
  messages: readonly WorkspaceChatMessage[],
): Array<WorkspaceChatMessage & { isLead: boolean }> {
  const out: Array<WorkspaceChatMessage & { isLead: boolean }> = [];
  let lastFrom: string | null = null;
  let lastKind: WorkspaceChatMessage["kind"] | null = null;
  for (const msg of messages) {
    const isSystem = msg.kind === "system";
    const sameSender = !isSystem && msg.from === lastFrom && lastKind === "chat";
    out.push({ ...msg, isLead: !sameSender });
    lastFrom = isSystem ? null : msg.from;
    lastKind = msg.kind;
  }
  return out;
}

/**
 * SW-WC-3 — the chat pane's scrollable body.
 *
 * Renders the message list via `<WorkspaceChatMessageRow>` (which delegates
 * body rendering to `<InboxMessageContent>` per the engine design §3.5
 * MANDATORY reuse directive). System rows render through
 * `<WorkspaceChatSystemRow>`. Follow-up detection (consecutive sender)
 * drives the density-specific collapse.
 */
export function WorkspaceChatBody({
  messages,
  loading,
  error,
  density,
  humanSentinel = "human",
  emptyState,
  unreadAfter,
  unreadExclude,
  unreadLabel,
  identify,
}: WorkspaceChatBodyProps): React.JSX.Element {
  const annotated = useMemo(() => annotateMessages(messages ?? []), [messages]);
  const bodyRef = useRef<HTMLDivElement>(null);
  // Was the FIRST paint for this message set still pending? The near-bottom
  // threshold below is right for incremental appends but wrong for the first
  // paint of a long log, where scrollTop = 0 but we must land on the newest
  // message. This flag flips false after the first non-null `messages` seen
  // (review #2 fix).
  const firstPaintRef = useRef(true);

  // Auto-scroll to the tail: unconditional on the first paint after messages
  // populate (otherwise a long log lands on the oldest row); on subsequent
  // updates only when the user is already near the bottom (don't yank them
  // mid-read).
  useLayoutEffect(() => {
    const el = bodyRef.current;
    if (!el || messages === null) return;
    if (firstPaintRef.current) {
      firstPaintRef.current = false;
      el.scrollTop = el.scrollHeight;
      return;
    }
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distanceFromBottom < 120) {
      el.scrollTop = el.scrollHeight;
    }
  }, [annotated, messages]);

  // A workspace swap (`messages` back to null) resets the first-paint latch so
  // the NEXT populate re-anchors to the tail.
  useLayoutEffect(() => {
    if (messages === null) firstPaintRef.current = true;
  }, [messages]);

  if (loading && messages === null) {
    return (
      <div className="wc-status" data-testid="workspace-chat-loading" role="status">
        Loading chat…
      </div>
    );
  }

  if (error && messages === null) {
    const isNoEngine = /workspace chat engine not installed/i.test(error);
    return (
      <div className="wc-status" data-testid="workspace-chat-no-engine" role="status">
        <strong>Chat isn't available</strong>
        {isNoEngine ? "The workspace chat engine isn't installed in this build." : error}
      </div>
    );
  }

  if ((!messages || messages.length === 0) && !emptyState) {
    return (
      <div className="wc-status" data-testid="workspace-chat-empty-conv" role="status">
        <strong>No messages yet</strong>
        Post the first message to start the thread.
      </div>
    );
  }

  return (
    <div
      className="wc-body"
      ref={bodyRef}
      tabIndex={0}
      role="log"
      aria-live="polite"
      aria-label="Workspace chat messages"
      data-testid="workspace-chat-body"
    >
      {annotated.map((msg) => {
        if (msg.kind === "system") {
          return (
            <WorkspaceChatSystemRow
              key={`sys-${msg.messageId}`}
              text={msg.text}
              createdAtMs={msg.createdAtMs}
              density={density}
            />
          );
        }
        const isHuman = msg.from === humanSentinel;
        const role = colourForHandle(msg.from);
        const identity = identify?.(msg.from, msg.fromName);
        const firstUnread =
          unreadAfter !== undefined &&
          msg.from !== unreadExclude &&
          msg.messageId > unreadAfter &&
          !annotated.some(
            (m) =>
              m.kind === "chat" &&
              m.from !== unreadExclude &&
              m.messageId > unreadAfter &&
              m.messageId < msg.messageId,
          );
        return (
          <Fragment key={msg.messageId}>
            {firstUnread && unreadLabel && (
              <div className="ibx-new" role="separator">
                {unreadLabel}
              </div>
            )}
            <WorkspaceChatMessageRow
              identity={identity}
              message={msg}
              isLead={msg.isLead || firstUnread}
              isHuman={isHuman}
              avatarGlyph={initialForHandle(msg.from, msg.fromName)}
              roleColour={role}
            />
          </Fragment>
        );
      })}
      {!annotated.some((m) => m.kind === "chat") && emptyState}
    </div>
  );
}

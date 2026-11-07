// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { SquareTerminal, WifiOff } from "lucide-react";

import { goToSession } from "@/features/inbox/actions";
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import type { ConversationMessage } from "@/features/inbox/api";
import { teamOfKey } from "@/features/inbox/components/inbox-rail";
import { InboxViewProvider, type InboxView } from "@/features/inbox/components/inbox-view-context";
import { InboxLoading, ReadFailure, StaleNotice } from "@/features/inbox/components/primitives";
import { Conversation } from "@/features/inbox/components/threads-lens";
import { fixtureSessions } from "@/features/inbox/fixtures";
import { useDocumentVisible } from "@/features/inbox/hooks/use-document-visible";
import { useInboxSessions } from "@/features/inbox/hooks/use-inbox-sessions";
import { useSessionInbox } from "@/features/inbox/hooks/use-session-inbox";
import {
  HUMAN,
  buildDirectory,
  msgKey,
  normalizeMessage,
  participant,
  roleSummaries,
  sameTeam,
  sendCheck,
} from "@/features/inbox/model";
import { UNKNOWN_MACHINE_LABEL } from "@/features/machines/lib/machine-offline";
import { PopupHead } from "@/features/viewer/components/agents-popup-head";
import { useRowPopover } from "@/features/viewer/components/agents-popover";
import type { InboxTeam } from "@/tauri/commands";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { markInboxMessageRead } from "@/tauri/commands";

export interface InboxChatProps {
  /** The agent's name, for the header. */
  name: string;
  /** The status word the row shows. */
  statusWord: string;
  /** The workspace the agent works in, when it has one. */
  workspace: string | null;
  /** The question the agent is waiting on, shown above the header. */
  ask?: string | null;
  /** The agent's inbox address. */
  handle: string;
  /** The team a first message goes to when no earlier message names one. */
  teamKey: string;
  /** The machine it runs on, when that machine is offline: mail waits for it. */
  offlineMachine: string | null;
  /** The row's button, which the popup is anchored to and returns focus to. */
  anchor: HTMLElement;
  /** Close the popup. `restoreFocus` is true for Escape, ⌘W and the close button. */
  onClose: (restoreFocus: boolean) => void;
  /** Show the agent's CLI tab with its inbox open; null when it has no tab to show. */
  onOpenInCli: (() => void) | null;
}

/** The conversation's size before the user drags the corner: room for a few exchanges. */
const DEFAULT_SIZE = { width: 460, height: 440 };
const MIN_SIZE = { width: 340, height: 260 };
/** The popup's height past its conversation (the header, gaps and padding) until it is measured:
 *  an ask line or the offline note, wrapped or not, adds to it. */
const CHROME_PX = 80;
/** Where the dragged size is remembered, on this Mac (apart from the quick view's). */
const SIZE_KEY = "codesense.agents.inboxChat.size";

/** Escape in a composer holding a draft leaves the box (the draft stays); a second one closes. */
const draftFocused = (dialog: HTMLElement) => {
  const active = document.activeElement;
  return (
    active instanceof HTMLTextAreaElement &&
    dialog.contains(active) &&
    active.value.trim().length > 0
  );
};

/** A message between the human and `handle`, either way (role mail and other agents' are not). */
const between = (m: ConversationMessage, handle: string) =>
  !m.toRole && ((m.from === handle && m.to === HUMAN) || (m.from === HUMAN && m.to === handle));

/** The team an agent works in, by its mail: the pane's team when the agent wrote there, else
 *  where it last wrote, else the pane's team (null when there is none). */
function agentTeam(
  messages: readonly ConversationMessage[],
  handle: string,
  home: InboxTeam | null,
): InboxTeam | null {
  const own = messages.filter((m) => m.from === handle);
  if (home && own.some((m) => sameTeam(m.team, home))) return home;
  // The latest by when it was written: a read lists teams one after another, not by time.
  let last: ConversationMessage | null = null;
  for (const m of own) if (!last || m.createdAtMs > last.createdAtMs) last = m;
  return last ? last.team : home;
}

/** The note an agent on an offline machine wears: its mail is kept on this Mac and pushed to it
 *  again when the machine reconnects. `machine` is its name, or {@link UNKNOWN_MACHINE_LABEL}. */
function offlineNote(machine: string): string {
  const named = machine === UNKNOWN_MACHINE_LABEL ? "A remote machine" : machine;
  return `${named} is offline. Messages are delivered when it reconnects.`;
}

/**
 * The inbox chat: an agent's conversation with you, opened from its row without switching the
 * CLI tab, in the quick view's frame. It reads the same session inbox the tab's inbox rail
 * reads and sends through the same composer, so a message here is the message the rail shows.
 * Opening it reads what the agent sent you, as the rail's conversation does.
 */
export function InboxChatPopover({
  name,
  statusWord,
  workspace,
  ask,
  handle,
  teamKey,
  offlineMachine,
  anchor,
  onClose,
  onOpenInCli,
}: InboxChatProps): React.JSX.Element {
  const {
    dialogRef,
    bodyRef,
    place,
    shown,
    gripProps,
    gripHint,
    arrowProps,
    onMouseDown,
    closeProps,
  } = useRowPopover({
    anchor,
    onClose,
    storageKey: SIZE_KEY,
    defaultSize: DEFAULT_SIZE,
    minSize: MIN_SIZE,
    chromePx: CHROME_PX,
    keepEscape: draftFocused,
  });
  const read = useSessionInbox(handle);
  // Only this agent's inbox: a read still naming the agent shown before is not shown for it.
  const data = read.data && read.data.handle === handle ? read.data : null;
  const { source, error, reload } = read;
  const sessions = useInboxSessions();
  const user = useCurrentUser();

  const messages = useMemo(() => (data?.messages ?? []).map(normalizeMessage), [data]);
  // The agent the chat shows: a handle names a session within its team, so the team is part of
  // it. Joined as JSON: a team name or a handle can hold any separator a plain join would use.
  const chatKey = JSON.stringify([teamKey, handle]);
  // The read spans every synced team; a handle names a session within its team, so the chat
  // shows only the agent's own team's mail (its other teams' mail is other conversations). The
  // team is settled by the first read and kept while the chat shows this agent: mail arriving in
  // another team never moves the conversation, or a send, there mid-chat.
  const readTeam = useMemo(
    () => agentTeam(messages, handle, teamOfKey(teamKey)),
    [messages, handle, teamKey],
  );
  const [pinned, setPinned] = useState<{ chat: string; team: InboxTeam | null } | null>(null);
  if (data && pinned?.chat !== chatKey) setPinned({ chat: chatKey, team: readTeam });
  const team = pinned?.chat === chatKey ? pinned.team : readTeam;
  // The shown team's mail: what the chat, its roles and where a send goes are read from.
  const teamMessages = useMemo(
    () => (team ? messages.filter((m) => sameTeam(m.team, team)) : messages),
    [messages, team],
  );
  const conversation = useMemo(
    () => teamMessages.filter((m) => between(m, handle)),
    [teamMessages, handle],
  );
  const dir = useMemo(() => {
    // Demo messages (dev only) name demo sessions; label them from the demo roster too.
    const rows =
      source === "fixture" ? [...(sessions ?? []), ...fixtureSessions()] : (sessions ?? []);
    return buildDirectory(rows, messages, { humanName: user?.display_name });
  }, [sessions, source, messages, user?.display_name]);
  const roles = useMemo(() => roleSummaries(dir, teamMessages), [dir, teamMessages]);
  // Where the New line goes: the first message it sent you that was unread as the chat opened.
  // Kept while the chat shows this agent, though opening it reads them.
  const [newLine, setNewLine] = useState<{ chat: string; from: string | null } | null>(null);
  const newFrom = newLine?.chat === chatKey ? newLine.from : undefined;
  if (newFrom === undefined && data) {
    const first = conversation.find((m) => m.from === handle && m.unread);
    setNewLine({ chat: chatKey, from: first ? msgKey(first) : null });
  }

  // On screen, the chat reads what the agent sent you; nothing else is marked.
  const windowShown = useDocumentVisible();
  const marked = useRef(new Set<string>());
  useEffect(() => {
    if (source !== "live" || !windowShown) return;
    for (const m of conversation) {
      const k = msgKey(m);
      if (m.from !== handle || !m.unread || marked.current.has(k)) continue;
      marked.current.add(k);
      void markInboxMessageRead(m.team, m.id).catch(() => marked.current.delete(k));
    }
  }, [source, windowShown, conversation, handle]);

  // Focus starts on the popup, then moves into the composer once the conversation has loaded
  // (unless the user has already moved it elsewhere).
  // A first read that failed hands it to Try again instead, the one thing to do then.
  const loaded = data !== null;
  const failedRead = !loaded && !!error;
  useEffect(() => {
    dialogRef.current?.focus({ preventScroll: true });
  }, [dialogRef]);
  useEffect(() => {
    if (!loaded && !failedRead) return;
    const dialog = dialogRef.current;
    const target = loaded
      ? dialog?.querySelector<HTMLElement>(".ibx-comp textarea")
      : dialog?.querySelector<HTMLElement>("[data-testid='ibx-read-retry']");
    const active = document.activeElement;
    if (target && (active === dialog || active === document.body || active === anchor)) {
      target.focus({ preventScroll: true });
    }
  }, [loaded, failedRead, dialogRef, anchor]);

  const closeRef = useRef(onClose);
  useLayoutEffect(() => {
    closeRef.current = onClose;
  });
  const view: InboxView = useMemo(
    () => ({
      me: HUMAN,
      targets: [handle],
      dir,
      // The shown team's mail, and that team to fall back on: a send goes to the team the chat
      // shows, never to another team the agent last wrote in.
      messages: teamMessages,
      roles,
      held: [],
      // Its own drafts: closing the chat and opening it again finds what was being written.
      drafts: `agent-chat:${chatKey}`,
      fallbackTeam: team ?? teamOfKey(teamKey),
      source,
      // The chat draws no Reply chips: everything here goes to this agent.
      replyTo: () => {},
      goTo: (key) => void goToSession(key),
      // An ended agent's mail goes to its role: the chat hands over to that role's inbox, in
      // this agent's team (another team's same-named role is another queue). The chat closes
      // first, handing focus back to its row; the rollup opens on the next frame, so it records
      // that row, not the chat's button that is gone, as where to return focus when it closes.
      // With no team to name there is none to open (every team's queue of that name is not this
      // one), so the chat offers none.
      openRoleInbox: team
        ? (roleKey) => {
            closeRef.current(true);
            requestAnimationFrame(() =>
              revealSlot("right-pane", {
                surface: "inbox",
                presentation: "overlay",
                intent: { conversation: roleKey, ...team, focusComposer: true },
              }),
            );
          }
        : undefined,
      // The header says the machine is offline; the composer's line need not say it again.
      offlineNoted: offlineMachine !== null,
    }),
    [handle, chatKey, dir, teamMessages, roles, teamKey, team, source, offlineMachine],
  );

  // What a first message does, as the agent stands: it waits for an offline machine, and an
  // ended agent's goes to the role it held.
  const check = sendCheck(handle, dir, teamMessages);
  const emptyNote = offlineMachine
    ? `What you send waits for ${offlineMachine} to reconnect.`
    : check.kind === "ended"
      ? check.role
        ? `${name} has ended, so it won't read new messages; what you have to say goes to #${check.role}.`
        : `${name} has ended, so it won't read new messages.`
      : participant(dir, handle).state === "idle"
        ? `What you send lands in ${name}'s inbox, and it wakes to read it.`
        : `What you send lands in ${name}'s inbox.`;

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-label={`Messages with ${name}`}
      tabIndex={-1}
      data-testid="agent-inbox-chat"
      // ⌘W anywhere in it closes it, a draft kept, as the quick view's does.
      {...closeProps}
      className="ag-quick-send ag-chat"
      onMouseDown={onMouseDown}
      style={{
        position: "fixed",
        top: place?.top ?? 0,
        left: place?.left ?? 0,
        width: shown.width,
      }}
    >
      <span {...arrowProps} />
      <PopupHead
        name={name}
        statusWord={statusWord}
        workspace={workspace}
        ask={ask}
        onClose={() => onClose(true)}
      >
        {onOpenInCli && (
          <button
            type="button"
            className="ag-quick-send-btn ag-chat-open"
            title={`Show ${name}'s CLI tab with its inbox open`}
            data-testid="agent-inbox-chat-open-cli"
            onClick={onOpenInCli}
          >
            <SquareTerminal className="size-3.5" aria-hidden />
            Open in CLI
          </button>
        )}
      </PopupHead>
      {offlineMachine && (
        <div className="ag-chat-note" role="status">
          <WifiOff className="size-3" aria-hidden />
          <span>{offlineNote(offlineMachine)}</span>
        </div>
      )}
      <div ref={bodyRef} className="ibx ag-chat-body" style={{ height: shown.height }}>
        <InboxViewProvider value={view}>
          {error && data && <StaleNotice error={error} onRetry={() => void reload()} />}
          {error && !data ? (
            <ReadFailure what={`${name}'s inbox`} error={error} onRetry={() => void reload()} />
          ) : !data ? (
            <InboxLoading what={`${name}'s inbox`} />
          ) : (
            <Conversation
              thread={{ key: handle, messages: conversation }}
              request={null}
              defaultTo={handle}
              quick
              newFrom={newFrom ?? null}
              empty={
                <div className="ibx-empty" data-testid="agent-inbox-chat-empty">
                  <strong>No messages yet</strong>
                  {emptyNote}
                </div>
              }
            />
          )}
        </InboxViewProvider>
      </div>
      <div className="ag-quick-send-grip" data-testid="agent-inbox-chat-grip" {...gripProps}>
        <span id={gripHint.id} hidden>
          {gripHint.text}
        </span>
      </div>
    </div>
  );
}

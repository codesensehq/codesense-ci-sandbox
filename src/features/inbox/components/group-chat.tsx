// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { ChevronLeft, MessagesSquare } from "lucide-react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import type { SessionListRow } from "@/tauri/commands";
import { WorkspaceChatBody } from "@/features/workspace-chat/components/workspace-chat-body";
import { WorkspaceChatCompose } from "@/features/workspace-chat/components/workspace-chat-compose";
import { WorkspaceChatArchiveBanner } from "@/features/workspace-chat/components/workspace-chat-archive-banner";
import { chatIdForWorkspace } from "@/features/workspace-chat/lib/workspace-chat-id";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useWorkspaceChatActions } from "@/features/workspace-chat/hooks/use-workspace-chat-actions";
import { useWorkspaceChat } from "@/features/workspace-chat/hooks/use-workspace-chat";
import { useChatHumanView } from "@/features/workspace-chat/hooks/use-chat-human-view";
import { useInboxSessions } from "../hooks/use-inbox-sessions";
import { initials, middleTruncate, participant } from "../model";
import { inboxDrafts, useInboxView } from "./inbox-view-context";
import { Ico, navRow } from "./primitives";
import "@/features/workspace-chat/workspace-chat.css";

export const groupKey = (workspaceId: string) => `group:${workspaceId}`;
export interface GroupRoom {
  workspaceId: string;
  sessions: Array<{ handle: string; name: string; unread: number; readId: number }>;
}
export const groupUnread = (room: GroupRoom) => Math.max(0, ...room.sessions.map((s) => s.unread));
export const countLabel = (n: number) => (n > 99 ? "99+" : n);

export function roomsForSessions(rows: readonly SessionListRow[]): GroupRoom[] {
  const rooms = new Map<string, GroupRoom>();
  for (const row of rows) {
    if (!row.chatWorkspaceId || !["running", "idle", "needs-you"].includes(row.state)) continue;
    const room = rooms.get(row.chatWorkspaceId) ?? {
      workspaceId: row.chatWorkspaceId,
      sessions: [],
    };
    room.sessions.push({
      handle: row.handle,
      name: row.displayName,
      unread: row.groupUnread ?? 0,
      readId: row.groupReadId ?? 0,
    });
    rooms.set(room.workspaceId, room);
  }
  return [...rooms.values()].sort((a, b) => a.workspaceId.localeCompare(b.workspaceId));
}

export function useRoomLabel(workspaceId: string) {
  const workspaces = useViewerStore((s) => s.workspaces);
  return useMemo(() => {
    const storyId = workspaceId.startsWith("story:") ? workspaceId.slice(6) : null;
    const ws = Object.values(workspaces)
      .flat()
      .find(
        (w) =>
          w.id === workspaceId ||
          chatIdForWorkspace(w) === workspaceId ||
          (storyId && w.storyId === storyId),
      );
    const name =
      ws?.storyTitle ||
      ws?.storyId ||
      ws?.epicDocPath ||
      ws?.cwd?.split("/").filter(Boolean).at(-1) ||
      storyId ||
      workspaceId.replace(/^generic:/, "");
    return { name, short: ws?.storyId || storyId || middleTruncate(name, 24) };
  }, [workspaces, workspaceId]);
}
export function GroupAvatar() {
  return (
    <span className="ibx-av group" aria-hidden="true">
      <Ico icon={MessagesSquare} />
    </span>
  );
}

export function GroupChip({
  room,
  selected,
  onOpen,
}: {
  room: GroupRoom;
  selected: boolean;
  onOpen: (keyboard: boolean) => void;
}) {
  const label = useRoomLabel(room.workspaceId);
  const unread = groupUnread(room);
  const names = room.sessions.map((s) => s.name).join(", ");
  return (
    <button
      type="button"
      className="ibx-chip group"
      aria-pressed={selected}
      title={`Group chat for ${label.short} — ${label.name}. ${unread} posts ${names} hasn't read. Reading here doesn't mark them read.`}
      aria-label={`Open the ${label.short} group chat, ${unread} unread for ${names}`}
      onClick={(e) => onOpen(e.detail === 0)}
    >
      <Ico icon={MessagesSquare} sm />
      {label.short}
      {unread > 0 && <span className="n">{countLabel(unread)}</span>}
    </button>
  );
}
export function GroupRow({
  room,
  selected,
  onOpen,
  aggregate = false,
}: {
  room: GroupRoom;
  selected: boolean;
  onOpen: (keyboard: boolean) => void;
  aggregate?: boolean;
}) {
  const label = useRoomLabel(room.workspaceId);
  const chat = useWorkspaceChat(room.workspaceId);
  const last = chat.messages
    ? [...chat.messages].reverse().find((m) => m.kind === "chat")
    : undefined;
  const unread = groupUnread(room);
  return (
    <button
      type="button"
      className={unread ? "ibx-row unread" : "ibx-row"}
      aria-current={selected}
      data-thread={groupKey(room.workspaceId)}
      title={room.sessions.map((s) => `${s.name}: ${s.unread} unread`).join(" · ")}
      onClick={(e) => onOpen(e.detail === 0)}
      {...navRow(() => onOpen(true))}
    >
      <GroupAvatar />
      <div style={{ minWidth: 0 }}>
        <div className="l1">
          <span className="nm">{label.short}</span>
          {unread > 0 && <span className="cnt">{countLabel(unread)}</span>}
        </div>
        <div className="pv">
          {aggregate
            ? `${room.sessions.length} session${room.sessions.length === 1 ? "" : "s"} here`
            : last
              ? `${last.fromName}: ${last.text}`
              : "New chat"}
        </div>
      </div>
    </button>
  );
}

export function GroupChatPane({
  room,
  onBack,
  focusRequest = 0,
  visible = true,
  human = false,
}: {
  room: GroupRoom;
  onBack?: () => void;
  focusRequest?: number;
  visible?: boolean;
  human?: boolean;
}) {
  const label = useRoomLabel(room.workspaceId);
  const chat = useWorkspaceChat(room.workspaceId);
  const root = useRef<HTMLDivElement>(null);
  const user = useCurrentUser();
  const view = useInboxView();
  const drafts = inboxDrafts(view.drafts ?? "workspace-chat");
  const draftKey = groupKey(room.workspaceId);
  const subscribe = useCallback(
    (notify: () => void) =>
      drafts.subscribe((key) => {
        if (key === draftKey) notify();
      }),
    [drafts, draftKey],
  );
  const draftText = useSyncExternalStore(subscribe, () => drafts.get(draftKey)?.text ?? "");
  const pending = useSyncExternalStore(subscribe, () => drafts.inFlight.has(draftKey));
  const setDraftText = useCallback<React.Dispatch<React.SetStateAction<string>>>(
    (next) => {
      const text = typeof next === "function" ? next(drafts.get(draftKey)?.text ?? "") : next;
      if (text) drafts.set(draftKey, { text, to: null, inReplyTo: null });
      else drafts.delete(draftKey);
      drafts.announce(draftKey);
    },
    [drafts, draftKey],
  );
  const allSessions = useInboxSessions();
  const agents =
    allSessions?.filter((s) => s.chatWorkspaceId === room.workspaceId && s.state !== "exited")
      .length ?? room.sessions.length;
  const actions = useWorkspaceChatActions(
    room.workspaceId,
    label.name,
    view.fallbackTeam ? buildTeamKey(view.fallbackTeam.org, view.fallbackTeam.team) : "",
    chat,
  );
  const sendDraft = async (text: string) => {
    if (drafts.inFlight.has(draftKey)) return;
    drafts.inFlight.set(draftKey, text);
    drafts.delete(draftKey);
    drafts.announce(draftKey);
    try {
      await actions.send(text);
    } catch (error) {
      // A failed send returns to its room, including after the pane closed.
      // Anything written since it was submitted always wins.
      if (!drafts.has(draftKey)) {
        drafts.set(draftKey, { text, to: null, inReplyTo: null });
        drafts.announce(draftKey);
      }
      throw error;
    } finally {
      drafts.inFlight.delete(draftKey);
      drafts.announce(draftKey);
    }
  };
  const humanAfter = useChatHumanView(room.workspaceId, chat, root, visible);
  const reader = room.sessions.reduce<GroupRoom["sessions"][number] | null>(
    (a, b) => (!a || b.unread > a.unread ? b : a),
    null,
  );
  const unread = groupUnread(room);
  const names = room.sessions.map((s) => s.name).join(", ");
  const loaded = chat.messages !== null;
  useEffect(() => {
    if (focusRequest > 0 && loaded)
      root.current?.querySelector<HTMLTextAreaElement>("textarea")?.focus({ preventScroll: true });
  }, [focusRequest, loaded]);
  return (
    <div className="ibx-group-pane" ref={root}>
      <div className="ibx-th-head">
        {onBack && (
          <button
            type="button"
            className="ibx-iconbtn"
            aria-label="Back to the inbox"
            onClick={onBack}
          >
            <Ico icon={ChevronLeft} />
          </button>
        )}
        <GroupAvatar />
        <div style={{ minWidth: 0 }}>
          <div className="nm">{label.name}</div>
          <div className="sub">
            Group chat · {label.short} · you + {agents} agent{agents === 1 ? "" : "s"}
            {!human &&
              room.sessions.map((s) => (
                <span key={s.handle}>
                  {" "}
                  · {s.name}: {s.unread > 0 ? `hasn't read ${s.unread}` : "all read"}
                </span>
              ))}
          </div>
        </div>
      </div>
      <section className="wc" data-density="b" aria-label={`${label.short} group chat`}>
        <WorkspaceChatBody
          messages={chat.messages}
          loading={chat.loading}
          error={chat.error}
          density="b"
          emptyState={
            <div className="wc-newchat">
              <strong>New chat</strong>
              <span>
                {agents
                  ? `Everything posted here goes to everyone working in ${label.short}: you and ${agents} agent${agents === 1 ? "" : "s"} now, plus any agent that joins later.`
                  : `Everything posted here goes to every agent that works in ${label.short}, including ones that join later.`}
              </span>
            </div>
          }
          unreadAfter={human ? humanAfter : reader?.readId}
          unreadExclude={human ? "human" : reader?.handle}
          unreadLabel={
            human
              ? "New since you looked"
              : unread
                ? `${unread} unread by ${reader?.name ?? names}`
                : undefined
          }
          identify={(from, fromName) => {
            const p = participant(view.dir, from);
            const name = view.dir.has(from) ? p.name : fromName || p.name;
            return {
              name,
              color: p.color,
              initials: initials(name),
              tag: view.me === from ? "this session" : undefined,
              machine: p.machine ?? undefined,
            };
          }}
        />
        {chat.archived ? (
          <WorkspaceChatArchiveBanner
            displayName={label.name}
            onUnarchive={actions.unarchive}
            onExport={actions.exportFn}
          />
        ) : (
          <WorkspaceChatCompose
            key={room.workspaceId}
            draft={{ text: draftText, setText: setDraftText }}
            displayName={user?.display_name || "you"}
            sending={pending || actions.sending}
            error={actions.sendError}
            onDismissError={actions.dismissError}
            onSend={sendDraft}
            placeholder={`Message everyone in ${label.short}…`}
          />
        )}
        {!human && (
          <div className="wc-foot-note">Reading here doesn't mark anything read for {names}.</div>
        )}
      </section>
    </div>
  );
}

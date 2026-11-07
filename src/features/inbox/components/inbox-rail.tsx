// SPDX-License-Identifier: MIT
import {
  ContextMenu,
  ContextMenuTrigger,
  ContextMenuContent,
  ContextMenuItem,
} from "@/components/ui/context-menu";
import { useRoleQueues } from "@/shared/hooks/use-role-queues";
import { ROLE_QUEUE_MENU_ITEMS } from "../lib/role-queue-menu-items";
import { openRoleEditor } from "@/features/agents/stores/role-editor-store";
import { InboxRole } from "./inbox-role";
import { useEffectEvent, useEffect, useMemo, useRef, useState } from "react";
import { useGroupChatSelection } from "../hooks/use-group-chat-selection";
import { toast } from "sonner";
import { ArrowUpRight, GitFork, PanelRightOpen, Plus, X } from "lucide-react";

import { openAgentRunning } from "@/features/agents/agents-virtual-tab";
import { RemoteServerUpdateAction } from "@/features/viewer/components/remote-server-update";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import { useTeamAgentIdentities } from "@/features/viewer/hooks/use-team-agent-identities";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { StatusMark } from "@/shared/components/status-mark";
import { agentStatusWords } from "@/shared/lib/terminal-dot-words";
import { useSubagentsOpen } from "@/shared/stores/agent-state-store";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import { markInboxMessageRead } from "@/tauri/commands";
import {
  closeAgentInboxRail,
  markAgentInboxRailComposeHandled,
  setAgentInboxRailLens,
  useAgentInboxRailComposeHandled,
  useAgentInboxRailLens,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { NO_TEAM_SENTINEL, parseTeamKey } from "@/shared/lib/team-key";
import type { AgentSessionRow, InboxTeam } from "@/tauri/commands";

import { goToSession } from "../actions";
import { fixtureSessions } from "../fixtures";
import { useComposeRequest } from "../hooks/use-compose-request";
import { useDocumentVisible } from "../hooks/use-document-visible";
import { useInboxSessions } from "../hooks/use-inbox-sessions";
import { useSessionInbox } from "../hooks/use-session-inbox";
import {
  HUMAN,
  buildDirectory,
  counterpart,
  heldRoles,
  machineLabel,
  middleTruncate,
  msgKey,
  normalizeMessage,
  participant,
  roleSummaries,
  threadsFor,
} from "../model";
import { useSlotTarget } from "@/features/shell/slots/reveal-slot";
import {
  InboxViewProvider,
  makeReplyRequest,
  type InboxView,
  type ReplyRequest,
} from "./inbox-view-context";
import type { ConversationMessage } from "../api";
import { MailspoolLens } from "./mailspool-lens";
import { Avatar, Ico, InboxLoading, LensSwitch, ReadFailure, StaleNotice } from "./primitives";
import { ThreadsLens } from "./threads-lens";
import { WireLens } from "./wire-lens";

import { GroupChip, groupKey, type GroupRoom } from "./group-chat";
import { useAgentInboxRailStore } from "@/features/viewer/stores/agent-inbox-rail-store";
/** The team a session's first message goes to when no earlier message names one. */
export function teamOfKey(teamKey: string): InboxTeam | null {
  if (teamKey === NO_TEAM_SENTINEL) return null;
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  return teamName ? { org: orgSlug, team: teamName } : null;
}

const NO_ROWS: readonly AgentSessionRow[] = [];

/**
 * One tab's inbox, docked beside its terminal. Keyed by the tab's session — hand-started
 * agents included — so it shows everything that session sent, received, had typed into its
 * terminal, or holds through a role. Reading here never marks anything read and never claims.
 */
export function InboxRail({
  tabId,
  sessionId,
  teamKey,
  composeRequest,
  narrow,
  visible = true,
  style,
}: {
  tabId: string;
  sessionId: string;
  teamKey: string;
  composeRequest: number;
  narrow: boolean;
  visible?: boolean;
  style?: React.CSSProperties;
}) {
  const roster = useAgentListProjectionStore((s) => s.sessions);
  const rosterRow = roster?.find((r) => r.sessionId === sessionId);
  const { data, source, error, reload } = useSessionInbox(sessionId);
  const handle = data?.handle ?? "";
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const inboxTarget = useSlotTarget("right-pane", {
    surface: "inbox",
    intent: {
      conversation: handle,
      org: orgSlug,
      team: teamName ?? "",
      focusComposer: true,
    },
  });
  const rosterName = rosterRow?.displayName ?? null;
  // The session's live status (the one status read every agent surface shares), and the
  // saved Agent it runs, whose home is the Agents tab.
  const statusRows = useMemo(() => (rosterRow ? [rosterRow] : NO_ROWS), [rosterRow]);
  const statusOf = useAgentStatusOf(statusRows);
  const status = rosterRow ? statusOf(rosterRow) : null;
  const subagents = useSubagentsOpen(sessionId);
  const identities = useTeamAgentIdentities(teamKey);
  const agentKey = identities.find((i) => i.row.sessionId === sessionId)?.agentKey ?? null;
  const sessions = useInboxSessions();
  const liveRoomRow = sessions?.find((s) => s.handle === handle);
  const workspaceId = liveRoomRow?.chatWorkspaceId ?? rosterRow?.chatWorkspaceId;
  const room: GroupRoom | null = workspaceId
    ? {
        workspaceId,
        sessions: [
          {
            handle: handle || rosterRow?.handle || sessionId,
            name: liveRoomRow?.displayName || rosterName || handle || sessionId,
            unread: liveRoomRow?.groupUnread ?? rosterRow?.groupUnread ?? 0,
            readId: liveRoomRow?.groupReadId ?? rosterRow?.groupReadId ?? 0,
          },
        ],
      }
    : null;
  const groupRequest = useAgentInboxRailStore((s) => s.bySession[sessionId]?.groupRequest ?? 0);
  const groupHandled = useAgentInboxRailStore((s) => s.bySession[sessionId]?.groupHandled ?? 0);
  const user = useCurrentUser();
  const lens = useAgentInboxRailLens(tabId);
  const railRef = useRef<HTMLElement>(null);

  const { queues } = useRoleQueues(teamKey);
  const messages = useMemo(() => (data?.messages ?? []).map(normalizeMessage), [data]);
  const dir = useMemo(() => {
    // Demo messages (dev only) name demo sessions; label them from the demo roster too.
    const rows =
      source === "fixture" ? [...(sessions ?? []), ...fixtureSessions()] : (sessions ?? []);
    return buildDirectory(rows, messages, {
      humanName: user?.display_name,
      queues,
      handleForSession: (id) => roster?.find((r) => r.sessionId === id)?.handle ?? null,
      me: data ? { handle, roles: data.roles, name: rosterName } : undefined,
    });
  }, [sessions, source, messages, user?.display_name, handle, data, rosterName, queues, roster]);
  const me = participant(dir, handle);
  const held = heldRoles(me);
  const roles = useMemo(
    () =>
      roleSummaries(
        dir,
        messages,
        [],
        queues.map((q) => q.role),
      ),
    [dir, messages, queues],
  );

  const [thread, setThread] = useState<string | null>(null);
  const [request, setRequest] = useState<ReplyRequest | null>(null);
  // Another session's inbox starts fresh: no thread or reply target carries over.
  const [shownSession, setShownSession] = useState(sessionId);
  if (shownSession !== sessionId) {
    setShownSession(sessionId);
    setThread(null);
    setRequest(null);
  }
  const groupSelection = useGroupChatSelection({
    selected: thread,
    lens,
    onSelect: setThread,
    onLensChange: (next) => setAgentInboxRailLens(tabId, next),
  });
  const toggleGroup = (keyboard: boolean) => {
    if (workspaceId) groupSelection.toggle(groupKey(workspaceId), keyboard);
  };
  const toggleRequestedGroup = useEffectEvent(() => toggleGroup(true));
  const readyForGroup = data !== null;
  useEffect(() => {
    if (groupRequest <= groupHandled || !workspaceId || !readyForGroup) return;
    const frame = requestAnimationFrame(() => {
      toggleRequestedGroup();
      useAgentInboxRailStore.getState().markGroupHandled(sessionId, groupRequest);
    });
    return () => cancelAnimationFrame(frame);
  }, [groupRequest, groupHandled, workspaceId, readyForGroup, sessionId]);
  const replyTo = (asked: string, inReplyTo?: ConversationMessage | null) => {
    // The composer always sends as the human: answering the human here means writing to this
    // session, never a note to yourself.
    const key = asked === HUMAN || asked === handle ? handle : asked;
    // This session's conversation with you is filed under the human.
    const threadKey = key === handle ? HUMAN : key;
    const known = threadsFor(messages, handle).some((t) => t.key === threadKey);
    // Mailspool replies on a line it shows; to someone with no line yet, Threads writes it.
    const toThreads = lens === "spool" && !known;
    if (toThreads) setAgentInboxRailLens(tabId, "threads");
    // In Threads a reply or a Write to always opens a conversation, so no other thread's
    // composer takes the request: the answered message's own (as the docked panel does), else
    // that party's, empty when nothing was exchanged yet.
    if (toThreads || lens === "threads")
      setThread(inReplyTo ? counterpart(inReplyTo, handle) : threadKey);
    setRequest(makeReplyRequest(key, inReplyTo));
  };

  // The session's conversation with you, open in Threads and on screen, reads what it sent you,
  // as the old rail did: mail read here leaves the unread badge. Nothing else is marked.
  const windowShown = useDocumentVisible();
  const shownThread =
    thread ??
    (lens === "threads" && !narrow ? (threadsFor(messages, handle)[0]?.key ?? null) : null);
  const marked = useRef(new Set<string>());
  useEffect(() => {
    if (
      !visible ||
      source !== "live" ||
      lens !== "threads" ||
      shownThread !== HUMAN ||
      !windowShown
    )
      return;
    // Laid out with a size: a rail collapsed to nothing (a team in viewer mode squeezes the
    // terminal section to 0) is not on screen.
    const box = railRef.current?.getBoundingClientRect();
    if (!box || box.width <= 0 || box.height <= 0) return;
    for (const m of messages) {
      const k = msgKey(m);
      if (m.to !== HUMAN || m.from !== handle || !m.unread || marked.current.has(k)) continue;
      marked.current.add(k);
      void markInboxMessageRead(m.team, m.id).catch(() => marked.current.delete(k));
    }
  }, [visible, source, lens, shownThread, windowShown, messages, handle]);

  const view: InboxView = {
    me: handle,
    dir,
    messages,
    roles,
    held,
    // Per tab and session: a roster handle arriving keeps them, a new session in the tab doesn't.
    drafts: `tab:${tabId}:${sessionId}`,
    fallbackTeam: teamOfKey(teamKey),
    source,
    replyTo,
    goTo: (key) => void goToSession(key),
  };

  // "Message this agent" from elsewhere: focus the composer. A request made while the inbox
  // is still loading (no composer yet) is honoured once it shows.
  const composeHandled = useAgentInboxRailComposeHandled(sessionId);
  useComposeRequest({
    request: composeRequest,
    handled: composeHandled,
    onHandled: (n) => markAgentInboxRailComposeHandled(sessionId, n),
    ready: data != null,
    scope: tabId,
    lens,
    setLens: (l) => setAgentInboxRailLens(tabId, l),
    // "Message this agent" writes to this session: open its conversation with you, where the
    // composer addresses it, never whoever it last talked with.
    open: () => {
      if (lens !== "threads" || thread === HUMAN) return false;
      setThread(HUMAN);
      return true;
    },
    openKey: thread,
    focus: () => {
      // Aimed at this session in any lens (Wire's composer too), and focused by the request.
      setRequest(makeReplyRequest(handle));
      requestAnimationFrame(() =>
        railRef.current?.querySelector<HTMLTextAreaElement>(".ibx-comp textarea")?.focus(),
      );
    },
  });

  const close = () => {
    const pane = railRef.current?.closest("[data-pane-id], [data-pane-active]");
    closeAgentInboxRail(sessionId);
    pane?.querySelector<HTMLTextAreaElement>(".xterm textarea")?.focus();
  };

  const queueChip = (role: string, title: string) => {
    const n = roles.find((r) => r.role === role)?.pending ?? 0;
    return (
      <ContextMenu key={role}>
        <ContextMenuTrigger asChild>
          <button
            type="button"
            className="ibx-chip role"
            title={`${title} · role:${role} · ${n} pending`}
            onClick={() => {
              setAgentInboxRailLens(tabId, "threads");
              setThread(`#${role}`);
            }}
          >
            <InboxRole role={role} />
            {n > 0 && <span className="n">{n}</span>}
          </button>
        </ContextMenuTrigger>
        <ContextMenuContent>
          {ROLE_QUEUE_MENU_ITEMS.map((item) => (
            <ContextMenuItem
              key={item.id}
              onSelect={() => {
                if (item.id === "edit-role") void openRoleEditor(teamKey, role);
                else {
                  setAgentInboxRailLens(tabId, "threads");
                  setThread(`#${role}`);
                }
              }}
            >
              {item.label}
            </ContextMenuItem>
          ))}
        </ContextMenuContent>
      </ContextMenu>
    );
  };
  const launched = [...dir.values()].filter((p) => p.launchedBy === handle).length;

  return (
    <InboxViewProvider value={view}>
      <aside
        ref={railRef}
        className="ibx ibx-rail"
        aria-label="Inbox"
        data-testid="inbox-rail"
        data-session-id={sessionId}
        data-narrow={narrow ? "true" : "false"}
        style={style}
        onKeyDown={(e) => {
          const t = e.target as HTMLElement;
          if (e.key !== "Escape" || e.defaultPrevented || t.matches("input, textarea")) return;
          // Stacked, Escape steps back from a thread to the list before it closes the inbox.
          if (narrow && thread && lens === "threads") setThread(null);
          else close();
        }}
      >
        <div className="ibx-rh">
          <div className="ibx-rh-top">
            <div className="ibx-scope">
              <Avatar k={handle} dir={dir} sm />
              <span className="t">{me.name}</span>
              {/* The raw handle is a path; it trails the name, cut in the middle. */}
              <span className="h" title={handle}>
                @{middleTruncate(handle, 32)} · {machineLabel(me)}
              </span>
            </div>
            <LensSwitch
              lens={lens}
              onChange={(l) => setAgentInboxRailLens(tabId, l)}
              label="Inbox lens"
            />
            {/* A saved Agent also has a home in the Agents tab; a hand-started or remote
                session has nowhere else to go, so no arrow. */}
            {agentKey && (
              <button
                type="button"
                className="ibx-iconbtn"
                title="Open in Agents tab"
                aria-label={`Open ${me.name} in the Agents tab`}
                data-testid="inbox-rail-open-agents-tab"
                onClick={() => void openAgentRunning(agentKey)}
              >
                <Ico icon={ArrowUpRight} />
              </button>
            )}
            <button
              type="button"
              className="ibx-iconbtn"
              title="Open in the Inbox"
              aria-label="Open in the Inbox"
              data-testid="inbox-rail-open-inbox-pane"
              onClick={inboxTarget.activate}
            >
              <Ico icon={PanelRightOpen} />
            </button>
            <button
              type="button"
              className="ibx-iconbtn"
              title="Close inbox"
              aria-label="Close inbox"
              onClick={close}
            >
              <Ico icon={X} />
            </button>
          </div>
          {status && (
            <div className="ibx-rh-status" data-testid="inbox-rail-status">
              <StatusMark mark={markOfAgentStatus(status)} />
              <span>{agentStatusWords(status, subagents)}</span>
              <RemoteServerUpdateAction sessionId={sessionId} className="ibx-rh-update" />
            </div>
          )}
          {!status && <RemoteServerUpdateAction sessionId={sessionId} className="ibx-rh-update" />}
          <div className="ibx-tray">
            {room && (
              <GroupChip
                room={room}
                selected={lens === "threads" && thread === groupKey(room.workspaceId)}
                onOpen={toggleGroup}
              />
            )}
            {held.map((r) =>
              queueChip(r, `Queued for #${r}. Stays queued until this session claims it.`),
            )}
            {held.length === 0 && (
              <>
                <span>No role held</span>
                <button
                  type="button"
                  className="ibx-chip ghost"
                  onClick={() =>
                    toast(
                      "Take a role: register_role lets this session claim queued messages for that role.",
                    )
                  }
                >
                  <Ico icon={Plus} sm />
                  Take a role
                </button>
              </>
            )}
            {source === "fixture" && (
              <span className="ibx-source" title="The inbox command is not available in this build">
                demo data
              </span>
            )}
            {launched > 0 && (
              <span
                style={{ marginLeft: "auto", display: "inline-flex", gap: 5, alignItems: "center" }}
              >
                <Ico icon={GitFork} sm />
                launched {launched}
              </span>
            )}
          </div>
        </div>
        {error && data && <StaleNotice error={error} onRetry={() => void reload()} />}
        {/* Keyed by session: no recipient, draft or Reply carries into another session's inbox. */}
        <div className="ibx-body" key={sessionId}>
          {error && !data ? (
            <ReadFailure what="this inbox" error={error} onRetry={() => void reload()} />
          ) : !data ? (
            <InboxLoading what="this inbox" />
          ) : lens === "threads" ? (
            <ThreadsLens
              rooms={room ? [room] : []}
              visible={visible}
              groupFocusRequest={groupSelection.focusRequest}
              onGroupSelect={groupSelection.open}
              selected={thread}
              onSelect={setThread}
              request={request}
              stacked={narrow}
            />
          ) : lens === "spool" ? (
            <MailspoolLens request={request} />
          ) : (
            <WireLens request={request} />
          )}
        </div>
      </aside>
    </InboxViewProvider>
  );
}

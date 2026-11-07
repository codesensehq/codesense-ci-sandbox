// SPDX-License-Identifier: MIT
import { InboxRole } from "./inbox-role";
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Archive, ChevronLeft, Layers } from "lucide-react";

import type { ConversationMessage } from "../api";
import {
  HUMAN,
  dayHeads,
  isSelf,
  defaultRecipient,
  formatClock,
  heldRoles,
  isRoleKey,
  claimBadge,
  labelOf,
  participant,
  messageRowPreview,
  repliesGoTo,
  roleSummaries,
  stateLabel,
  threadLanes,
  threadsFor,
  machineLabel,
  msgKey,
  type Thread,
} from "../model";
import { useArrivalNote, useUnseenBelow } from "../hooks/use-new-mail";
import { useStickToBottom } from "../hooks/use-stick-to-bottom";

import { GroupRow, GroupChatPane, groupKey, type GroupRoom } from "./group-chat";
/** The latest message `key` sent, which a Reply to them answers. */
export const lastFrom = (messages: readonly ConversationMessage[], key: string) =>
  [...messages].reverse().find((m) => m.from === key) ?? null;
import { Composer } from "./composer";
import { ArrivalsNote, JumpToLatest } from "./jump-to-latest";
import { selfOf, useInboxView, type ReplyRequest } from "./inbox-view-context";
import {
  Avatar,
  ClaimVia,
  GoButton,
  HoverActions,
  repliesViaKey,
  hasTypedVia,
  Ico,
  MessageBody,
  RepliesVia,
  TypedVia,
  navRow,
} from "./primitives";

/** The messages in `ms` sent to the human: what an Archive acts on. */
const toYou = (ms: readonly ConversationMessage[]) => ms.filter((m) => m.to === HUMAN);

/** The conversation the pane shows: a thread, or a party with nothing exchanged yet. */
export type PaneThread = Pick<Thread, "key" | "messages">;

/**
 * Threads: lanes (Unread · Queued for my role · Conversations) beside the open conversation.
 * In the human's inbox the list shows conversations and role queues.
 * `stacked` (a narrow pane) shows the list or the open thread, never both, with a way back.
 */
export function ThreadsLens({
  selected,
  onSelect,
  request,
  stacked = false,
  rooms = [],
  groupFocusRequest = 0,
  onGroupSelect,
  visible = true,
}: {
  rooms?: GroupRoom[];
  groupFocusRequest?: number;
  onGroupSelect?: (key: string, keyboard: boolean) => void;
  visible?: boolean;
  selected: string | null;
  onSelect: (key: string | null) => void;
  request: ReplyRequest | null;
  stacked?: boolean;
}) {
  const view = useInboxView();
  const { me, dir, messages, held } = view;
  const human = view.scope === "human";
  const self = selfOf(view);
  const threads = useMemo(() => threadsFor(messages, self), [messages, self]);
  // The human's inbox opens nothing by itself: opening a message to the human reads it.
  const group = rooms.find((r) => groupKey(r.workspaceId) === selected);
  const missingGroup = !!selected?.startsWith("group:") && !group;
  useEffect(() => {
    if (missingGroup) onSelect(null);
  }, [missingGroup, onSelect]);
  const current: PaneThread | null = missingGroup
    ? null
    : group
      ? { key: selected!, messages: [] }
      : (threads.find((t) => t.key === selected) ??
        // A party picked with nothing exchanged yet (a role chip, a Write to) opens empty, never
        // someone else's conversation.
        (selected
          ? { key: selected, messages: [] }
          : // Side by side, a tab's inbox opens its latest conversation; stacked, it starts on the
            // list, and the human's inbox opens nothing by itself.
            human || stacked
            ? null
            : (threads[0] ?? null)));
  // Opened by itself (nothing picked): new mail elsewhere would swap it for another thread
  // under the caret, so the first focus inside the pane (the composer, mostly) pins it.
  const auto = selected === null && current !== null;
  const lanes = useMemo(() => threadLanes(threads, held), [threads, held]);
  const row = (t: Thread) => (
    <ThreadRow key={t.key} thread={t} current={t.key === current?.key} onSelect={onSelect} />
  );

  const list = human ? (
    <HumanLanes threads={threads} current={current?.key ?? null} onSelect={onSelect} />
  ) : (
    <>
      {lanes.unread.length > 0 && (
        <>
          <div className="ibx-lane">
            Unread <span className="c">{lanes.unread.length}</span>
          </div>
          {lanes.unread.map(row)}
        </>
      )}
      {lanes.role.length > 0 && (
        <>
          <div className="ibx-lane role">
            {held[0] ? (
              <>
                Queued for <InboxRole role={held[0]} />
              </>
            ) : (
              "Queued for my role"
            )}
          </div>
          {lanes.role.map(row)}
        </>
      )}
      {lanes.rest.length > 0 && (
        <>
          <div className="ibx-lane">Conversations</div>
          {lanes.rest.map(row)}
        </>
      )}
      {threads.length === 0 && (
        <div className="ibx-empty" style={{ padding: "30px 12px" }} data-testid="ibx-empty">
          <strong>No messages yet</strong>
          {view.self
            ? `Anything sent to or from its sessions${held[0] ? ` or #${held[0]}` : ""} shows up here, including text typed into their terminals.`
            : `Anything sent to or from @${me === HUMAN ? "you" : labelOf(dir, me)} shows up here, including text typed into its terminal.`}
        </div>
      )}
    </>
  );

  // Stacked with no conversation at all, the list says nothing more than the empty pane, and
  // the pane holds the composer: show that, so a request to write always finds one. Once
  // shown it stays (pinned) when the first conversation arrives, so a draft being typed is
  // never unmounted under the caret; Back returns to the list, and a send opens its thread.
  const freshNow = stacked && !current && !human && threads.length === 0 && rooms.length === 0;
  const [freshPinned, setFreshPinned] = useState(false);
  if (freshNow && !freshPinned) setFreshPinned(true);
  else if (freshPinned && (!stacked || !!current || human)) setFreshPinned(false);
  const fresh =
    stacked && !current && !human && rooms.length === 0 && (threads.length === 0 || freshPinned);
  const showList = !stacked || (!current && !fresh);
  const showPane = !stacked || !!current || fresh;

  // Stacked, Back (or Escape) unmounts the pane that held focus: put it on the list, on the
  // row of the conversation just left, so the keyboard keeps its place (and a further Escape
  // still reaches the inbox).
  const listRef = useRef<HTMLDivElement>(null);
  const left = useRef<string | null>(null);
  // Whether focus was in the pane as it unmounted (read by a probe inside it, before its DOM
  // goes): a pane swapped out by a live refresh while focus was elsewhere moves nothing.
  const paneHadFocus = useRef(false);
  useLayoutEffect(() => {
    const leftKey = left.current;
    left.current = stacked && !showList ? (current?.key ?? "") : null;
    if (leftKey === null || !showList) return;
    const hadFocus = paneHadFocus.current;
    paneHadFocus.current = false;
    const active = document.activeElement;
    if (!hadFocus || (active && active !== document.body && active.isConnected)) return;
    const list = listRef.current;
    if (!list) return;
    const rows = [...list.querySelectorAll<HTMLElement>("[data-ibx-nav]")];
    const back = rows.find((r) => r.dataset.thread === leftKey) ?? rows[0] ?? list;
    back.focus();
  });
  return (
    <>
      {showList && (
        <div
          ref={listRef}
          tabIndex={stacked ? -1 : undefined}
          className={stacked ? "ibx-th-list full" : "ibx-th-list"}
          role="list"
          aria-label="Conversations"
          data-ibx-navscope=""
        >
          {rooms.length > 0 && (
            <>
              <div className="ibx-lane">{view.self ? "Group chats" : "Group chat"}</div>
              {rooms.map((room) => (
                <GroupRow
                  key={room.workspaceId}
                  room={room}
                  selected={group === room}
                  aggregate={!!view.self}
                  onOpen={(keyboard) =>
                    onGroupSelect
                      ? onGroupSelect(groupKey(room.workspaceId), keyboard)
                      : onSelect(groupKey(room.workspaceId))
                  }
                />
              ))}
            </>
          )}
          {list}
        </div>
      )}
      {showPane && (
        <div
          className="ibx-th-pane"
          onFocus={auto && current ? () => onSelect(current.key) : undefined}
        >
          <PaneFocusProbe into={paneHadFocus} />
          {group ? (
            <GroupChatPane
              key={group.workspaceId}
              room={group}
              focusRequest={groupFocusRequest}
              visible={visible}
              onBack={stacked ? () => onSelect(null) : undefined}
            />
          ) : current ? (
            <ThreadPane
              thread={current}
              request={request}
              onBack={stacked ? () => onSelect(null) : undefined}
            />
          ) : (
            <>
              {fresh && threads.length > 0 && (
                <div className="ibx-th-head">
                  <button
                    type="button"
                    className="ibx-iconbtn"
                    title="Back"
                    aria-label="Back to the inbox"
                    onClick={() => setFreshPinned(false)}
                  >
                    <Ico icon={ChevronLeft} />
                  </button>
                </div>
              )}
              <div className="ibx-empty">
                {human ? (
                  <>
                    <strong>Pick a conversation</strong>
                    Open a conversation or a role queue. Replies go out as you, to an @session or a
                    #role.
                  </>
                ) : (
                  <>
                    <strong>Start a conversation</strong>
                    Pick an @session or a #role below. A role queues the message until an agent
                    claims it.
                  </>
                )}
              </div>
              <Composer
                defaultTo={null}
                request={request}
                // Stacked, the first message opens its conversation instead of the list.
                // In a tab's inbox a message to the tab's own session lives in its
                // conversation with you.
                onSent={
                  fresh
                    ? (to) => onSelect(!view.self && to === view.me && to !== HUMAN ? HUMAN : to)
                    : undefined
                }
              />
            </>
          )}
        </div>
      )}
    </>
  );
}

/**
 * Reports, as its pane unmounts, whether focus was inside it. A child's layout cleanup runs
 * before its parent's DOM is removed, so focus is still where the user left it.
 */
function PaneFocusProbe({ into }: { into: React.RefObject<boolean> }) {
  const mark = useRef<HTMLSpanElement>(null);
  useLayoutEffect(() => {
    // Held here: React detaches refs before a deleted subtree's cleanups run.
    const pane = mark.current?.parentElement ?? null;
    return () => {
      into.current = !!pane?.contains(document.activeElement);
    };
  }, [into]);
  return <span ref={mark} hidden />;
}

/** The human's conversations, newest first, followed by every role queue. */
function HumanLanes({
  threads,
  current,
  onSelect,
}: {
  threads: Thread[];
  current: string | null;
  onSelect: (key: string | null) => void;
}) {
  const view = useInboxView();
  const { roles } = view;
  const convs = threads.filter((t) => !isRoleKey(t.key));
  const roleThread = (role: string) => threads.find((t) => t.key === `#${role}`);
  return (
    <>
      <div data-testid="ibx-human-conversations">
        {convs.length > 0 && <div className="ibx-lane">Conversations</div>}
        {convs.map((t) => (
          <ThreadRow key={t.key} thread={t} current={t.key === current} onSelect={onSelect} />
        ))}
      </div>
      {roles.length > 0 && (
        <div data-testid="ibx-human-roles">
          <div className="ibx-lane role">
            <Ico icon={Layers} sm />
            Role queues
          </div>
          {roles.map((r) => {
            const t = roleThread(r.role);
            return (
              <button
                key={r.role}
                type="button"
                className="ibx-qrow"
                data-thread={`#${r.role}`}
                aria-current={current === `#${r.role}`}
                title={t ? `Open #${r.role}` : `Write to #${r.role}`}
                onClick={() => onSelect(`#${r.role}`)}
                {...navRow()}
              >
                <InboxRole role={r.role} iconOnly size={22} />
                <span className="k">
                  <InboxRole role={r.role} nameOnly />
                </span>
                <span className={r.pending ? "n" : "n z"}>{r.pending}</span>
                <span className="d">{r.note}</span>
              </button>
            );
          })}
          <div className="ibx-note">
            Send to a role, not to the controller. A role message stays queued until an agent claims
            it; the controller watches every queue.
          </div>
        </div>
      )}
      {convs.length === 0 && roles.length === 0 && (
        <div className="ibx-empty" style={{ padding: "30px 12px" }} data-testid="ibx-empty">
          <strong>No messages yet</strong>
          What agents send you lands here, along with your replies and the role queues.
        </div>
      )}
    </>
  );
}

function ThreadRow({
  thread,
  current: isCurrent,
  onSelect: select,
}: {
  thread: Thread;
  current: boolean;
  onSelect: (key: string | null) => void;
}) {
  const view = useInboxView();
  const { dir } = view;
  const k = thread.key;
  // "You" is the human reading; a tab's own messages go by its name.
  const from = thread.last.from;
  const by = from === HUMAN ? "You: " : isSelf(selfOf(view), from) ? `${labelOf(dir, from)}: ` : "";
  return (
    <div
      className={thread.unread ? "ibx-row unread" : "ibx-row"}
      role="button"
      tabIndex={0}
      aria-current={isCurrent}
      data-thread={k}
      onClick={() => select(k)}
      {...navRow(() => select(k))}
    >
      <Avatar k={k} dir={dir} team={thread.last.team} />
      <div style={{ minWidth: 0 }}>
        <div className="l1">
          <span className={isRoleKey(k) ? "nm role" : "nm"}>
            {isRoleKey(k) ? (
              <InboxRole role={k.slice(1)} team={thread.last.team} nameOnly />
            ) : (
              labelOf(dir, k)
            )}
          </span>
          <span className="tm">{formatClock(thread.last.createdAtMs)}</span>
        </div>
        <div className="pv">
          {by}
          {messageRowPreview(thread.last)}
        </div>
      </div>
      <HoverActions
        k={k}
        dir={dir}
        onReply={() => {
          const last = lastFrom(thread.messages, k);
          view.replyTo(last ? repliesGoTo(last) : k, last);
        }}
        onGo={() => view.goTo(k)}
        onOpen={() => select(k)}
        // Archive acts on what the conversation sent you: nothing to archive, no button.
        onArchive={
          view.archive && toYou(thread.messages).length > 0
            ? () => view.archive?.(toYou(thread.messages))
            : undefined
        }
      />
    </div>
  );
}

function ThreadPane({
  thread,
  request,
  onBack,
}: {
  thread: PaneThread;
  request: ReplyRequest | null;
  /** Shown on a narrow pane, where the thread replaced the list. */
  onBack?: () => void;
}) {
  const view = useInboxView();
  const { dir, messages, held } = view;
  const k = thread.key;
  const isRole = isRoleKey(k);
  const p = participant(dir, k);
  const roleNote = isRole
    ? (roleSummaries(dir, messages, view.roles).find((r) => r.role === k.slice(1))?.note ?? "")
    : null;
  const sub = isRole
    ? roleNote
    : p.kind === "human"
      ? "you, the human"
      : [stateLabel(p.state), p.agent, machineLabel(p)].filter(Boolean).join(" · ");
  const self = selfOf(view);
  return (
    <>
      <div className="ibx-th-head">
        {onBack && (
          <button
            type="button"
            className="ibx-iconbtn"
            title="Back (Esc)"
            aria-label="Back to the inbox"
            onClick={onBack}
          >
            <Ico icon={ChevronLeft} />
          </button>
        )}
        <Avatar k={k} dir={dir} team={thread.messages.at(-1)?.team} />
        <div style={{ minWidth: 0 }}>
          <div className="nm">
            {isRoleKey(k) ? (
              <InboxRole role={k.slice(1)} team={thread.messages.at(-1)?.team} nameOnly />
            ) : (
              labelOf(dir, k)
            )}
          </div>
          <div className="sub">{sub}</div>
        </div>
        <span className="ibx-sp" />
        {view.archive && toYou(thread.messages).length > 0 && (
          <button
            type="button"
            className="ibx-gobtn"
            title="Archive what this conversation sent you"
            onClick={() => {
              view.archive?.(toYou(thread.messages));
              onBack?.();
            }}
          >
            <Ico icon={Archive} sm />
            <span className="lbl">Archive</span>
          </button>
        )}
        {!isRole && p.kind !== "human" && <GoButton k={k} dir={dir} onGo={() => view.goTo(k)} />}
      </div>
      <Conversation
        thread={thread}
        request={request}
        defaultTo={defaultRecipient(k, self, held, thread.messages)}
      />
    </>
  );
}

/**
 * One conversation: its messages, oldest first under day separators, kept at the latest as
 * mail lands (with a Jump to latest pill for a reader who scrolled up), and the composer.
 * A conversation's pane and the Agents pane's inbox chat both draw it. `quick` is the chat's
 * compact form: no Reply-to chips under each message, and the quick composer.
 */
export function Conversation({
  thread,
  request,
  defaultTo,
  quick = false,
  newFrom = null,
  empty = null,
}: {
  thread: PaneThread;
  request: ReplyRequest | null;
  defaultTo: string | null;
  quick?: boolean;
  /** The first message that was unread when the conversation opened: a New line goes above it. */
  newFrom?: string | null;
  /** Shown in place of the messages while there are none. */
  empty?: React.ReactNode;
}) {
  const view = useInboxView();
  const { dir } = view;
  const scroller = useRef<HTMLDivElement>(null);
  const last = thread.messages.at(-1);
  // The count and the newest key: at the 500 cap a message arrives as the oldest leaves.
  const { away, jump } = useStickToBottom(
    scroller,
    thread.key,
    `${thread.messages.length}:${last ? msgKey(last) : ""}`,
  );
  const self = selfOf(view);
  const unseen = useUnseenBelow(thread.messages, self, away, thread.key);
  const arrivals = useArrivalNote(thread.messages, self, dir, thread.key);
  // What you just sent is shown, even if you had scrolled up to read before writing it.
  const lastOwn = last && isSelf(self, last.from) ? msgKey(last) : null;
  const sentFrom = useRef(lastOwn);
  useEffect(() => {
    if (lastOwn !== null && lastOwn !== sentFrom.current) jump();
    sentFrom.current = lastOwn;
  }, [lastOwn, jump]);

  const heads = dayHeads(thread.messages);
  return (
    <>
      <div className="ibx-scrollwrap">
        <div
          className="ibx-msgs"
          ref={scroller}
          // Reachable by Tab to scroll with the keys, and where focus lands after a jump.
          tabIndex={0}
          role="region"
          aria-label={`Conversation with ${labelOf(dir, thread.key)}`}
          data-testid="ibx-msgs"
        >
          {thread.messages.length === 0 && empty}
          {thread.messages.map((m, i) => {
            const head = heads[i];
            const key = msgKey(m);
            return (
              <Fragment key={key}>
                {head && <div className="ibx-day">{head}</div>}
                {key === newFrom && (
                  <div className="ibx-new" data-testid="ibx-new">
                    <span>New</span>
                  </div>
                )}
                <Bubble
                  m={m}
                  plain={quick}
                  grouped={quick && !head && key !== newFrom && follows(thread.messages[i - 1], m)}
                />
              </Fragment>
            );
          })}
        </div>
        <JumpToLatest
          unseen={away ? unseen : 0}
          onJump={() => {
            jump();
            // The pill goes away; focus lands on the conversation, not the page body.
            scroller.current?.focus({ preventScroll: true });
          }}
        />
      </div>
      <ArrivalsNote note={arrivals} />
      <Composer defaultTo={defaultTo} scope={thread.key} request={request} quick={quick} />
    </>
  );
}

/** Within a few minutes after `prev`, from the same sender: drawn under it, without its own
 *  sender and time. A message stamped before `prev` (out of order) starts its own group. */
const GROUP_MS = 5 * 60_000;
const follows = (prev: ConversationMessage | undefined, m: ConversationMessage) => {
  if (!prev || prev.from !== m.from || prev.via !== m.via) return false;
  const gap = m.createdAtMs - prev.createdAtMs;
  return gap >= 0 && gap < GROUP_MS;
};

/** One message: in (left) or out (right), terminal-typed in a dashed mono box, with its claim state. */
export function Bubble({
  m,
  plain = false,
  grouped = false,
}: {
  m: ConversationMessage;
  /** Follows the sender's previous message closely: no sender and time of its own (how it came
   *  and where replies go are still said). */
  grouped?: boolean;
  /** Without the Reply-to chips: where only one party can be answered (the inbox chat). */
  plain?: boolean;
}) {
  const view = useInboxView();
  const { dir, me, held } = view;
  const out = isSelf(selfOf(view), m.from);
  const term = hasTypedVia(m);
  const sender = participant(dir, m.from);
  const senderRole = heldRoles(sender)[0] ?? null;
  const queuedHere =
    !!m.toRole && held.includes(m.toRole) && (!m.delivery || m.delivery.state === "pending");
  const cls = ["ibx-m", out ? "out" : "in", term ? "term" : "", grouped ? "grouped" : ""]
    .filter(Boolean)
    .join(" ");
  // A grouped message drops who and when, but keeps every mark its line would carry.
  const repliesTo = repliesViaKey(m);
  const claim = claimBadge(m, dir);
  const meta = !grouped || term || repliesTo !== null || claim !== null;
  return (
    <div className={cls} data-mid={msgKey(m)}>
      {meta && (
        <div className="meta">
          {grouped ? null : plain ? (
            // The chat's line: who, then when (the full date on hover), spaced by the gap.
            <>
              <span className="who">{m.from === HUMAN ? "You" : labelOf(dir, m.from)}</span>
              <time
                className="ibx-mono"
                dateTime={
                  Number.isFinite(m.createdAtMs) ? new Date(m.createdAtMs).toISOString() : undefined
                }
                title={
                  Number.isFinite(m.createdAtMs)
                    ? new Date(m.createdAtMs).toLocaleString()
                    : undefined
                }
              >
                {formatClock(m.createdAtMs)}
              </time>
            </>
          ) : (
            <>
              {m.from === HUMAN ? "You" : labelOf(dir, m.from)} ·{" "}
              <span className="ibx-mono">{formatClock(m.createdAtMs)}</span>
            </>
          )}
          {term && <TypedVia />}
          <ClaimVia badge={claim} />
          <RepliesVia to={repliesTo} dir={dir} team={m.team} />
        </div>
      )}
      <div className="bub">
        <MessageBody m={m} liveDashboards={me === HUMAN && !view.self} />
      </div>
      {plain ? null : queuedHere ? (
        <div className="ibx-rto">
          <span>Reading does not claim it. An agent holding #{m.toRole} claims it.</span>
          {m.from !== me && (
            <>
              <span style={{ flex: 1 }} />
              <span>Reply to</span>
              <button
                type="button"
                className="ibx-chip"
                onClick={() => view.replyTo(repliesGoTo(m), m)}
              >
                @{labelOf(dir, m.from)}
              </button>
            </>
          )}
        </div>
      ) : (
        !out &&
        !term &&
        senderRole && (
          <div className="ibx-rto">
            <span>Reply to</span>
            <button
              type="button"
              className="ibx-chip"
              onClick={() => view.replyTo(repliesGoTo(m), m)}
            >
              @{sender.name}
            </button>
            <button
              type="button"
              className="ibx-chip role"
              onClick={() => view.replyTo(`#${senderRole}`, m)}
            >
              #{senderRole}
            </button>
          </div>
        )
      )}
    </div>
  );
}

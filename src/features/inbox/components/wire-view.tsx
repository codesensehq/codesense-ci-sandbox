// SPDX-License-Identifier: MIT
import { InboxRole } from "./inbox-role";
import { Fragment, useMemo, useRef } from "react";
import { Reply, X } from "lucide-react";

import type { ConversationMessage, RoleSummary } from "../api";
import {
  HUMAN,
  dayHeads,
  formatClock,
  isRoleKey,
  isUnreadFor,
  labelOf,
  msgKey,
  participant,
  messageRowPreview,
  replyTarget,
  stateLabel,
  toKeyOf,
  wireArrows,
  type Directory,
  type Self,
  type WireColumn,
} from "../model";
import { useStickToBottom } from "../hooks/use-stick-to-bottom";
import {
  Avatar,
  GoButton,
  Ico,
  MessageBody,
  RepliesVia,
  TypedVia,
  navRow,
  repliesViaKey,
} from "./primitives";

/** The narrowest a lifeline column gets before the diagram scrolls sideways instead. */
export const WIRE_COL_MIN_PX = 132;

/**
 * Wire: a sequence diagram. One lifeline per participant (dashed for role queues), one arrow
 * per message coloured by kind — inbox blue, typed-into-terminal green dashed, role violet,
 * to-you amber. Newest at the bottom; the view opens scrolled there.
 */
export function WireView({
  columns,
  messages,
  dir,
  roles,
  me,
  selected,
  onSelect,
}: {
  columns: WireColumn[];
  messages: ConversationMessage[];
  dir: Directory;
  roles: readonly RoleSummary[];
  /** The tab's handle (marked "this tab"), or null in the rollup. */
  me: string | null;
  /** The selected message's key (see `msgKey`). */
  selected: string | null;
  onSelect: (key: string | null) => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const n = Math.max(1, columns.length);
  const colW = 100 / n;
  const arrows = useMemo(() => wireArrows(columns, messages), [columns, messages]);
  const heads = useMemo(() => dayHeads(arrows.map((a) => a.message)), [arrows]);
  useStickToBottom(scroller, columns.map((c) => c.key).join(" "), arrows.length);

  return (
    <div className="ibx-wscroll" ref={scroller} data-testid="ibx-wire">
      {/* Crowded, the lifelines keep a readable width and the diagram scrolls sideways; the
          heads ride inside the scroller (sticky on top) so they stay over their lifelines. */}
      <div className="ibx-wcanvas" style={{ minWidth: `${n * WIRE_COL_MIN_PX}px` }}>
        <div className="ibx-wheads" style={{ gridTemplateColumns: `repeat(${n}, minmax(0, 1fr))` }}>
          {columns.map((c) =>
            c.queue ? (
              <div key={c.key} className="ibx-whead queue" title={c.key}>
                <Avatar k={c.key} dir={dir} sm />
                <span className="nm">
                  <InboxRole role={c.key.slice(1)} nameOnly />
                </span>
                <span className="sub">
                  {roles.find((r) => `#${r.role}` === c.key)?.holder ? "held" : "queue"}
                </span>
              </div>
            ) : (
              <div key={c.key} className="ibx-whead" title={labelOf(dir, c.key)}>
                <Avatar k={c.key} dir={dir} sm />
                <span className="nm">
                  {labelOf(dir, c.key)}
                  {c.key === me && me !== HUMAN && <span className="x"> (this tab)</span>}
                </span>
                <span className="sub">{headSub(c.key, dir)}</span>
              </div>
            ),
          )}
        </div>
        <div className="ibx-wgrid" data-ibx-navscope="">
          {columns.map((c, i) => (
            <div
              key={c.key}
              className={c.queue ? "ibx-life queue" : "ibx-life"}
              style={{ left: `${colW * i + colW / 2}%` }}
            />
          ))}
          {arrows.map((a, i) => {
            const m = a.message;
            const key = msgKey(m);
            const cls = [
              "ibx-arrow",
              a.dir,
              ...a.kinds,
              selected === key ? "sel" : "",
              // The rollup has no "me": anything still unread by its recipient stands out.
              (me ? isUnreadFor(m, me) : m.unread) ? "unread" : "",
            ]
              .filter(Boolean)
              .join(" ");
            const text = messageRowPreview(m);
            const toggle = () => onSelect(selected === key ? null : key);
            const day = heads[i];
            return (
              <Fragment key={key}>
                {day && (
                  <div className="ibx-wday">
                    <span>{day}</span>
                  </div>
                )}
                <div className="ibx-wrow">
                  <span className="ibx-wtime">{formatClock(m.createdAtMs)}</span>
                  <span className="ibx-tick" style={{ left: `${a.tickPct}%` }} />
                  <div
                    className={cls}
                    style={{ left: `${a.leftPct}%`, width: `${a.widthPct}%` }}
                    role="button"
                    tabIndex={0}
                    title={text}
                    aria-label={`${labelOf(dir, m.from)} to ${labelOf(dir, toKeyOf(m))}: ${text}`}
                    aria-pressed={selected === key}
                    data-arrow={key}
                    onClick={toggle}
                    {...navRow(toggle)}
                  >
                    <div className="lb">
                      {m.via === "terminal" ? "⇢ " : ""}
                      {text}
                    </div>
                    <div className="ln" />
                    <div className="hd" />
                  </div>
                </div>
              </Fragment>
            );
          })}
          <div style={{ height: 16 }} />
        </div>
      </div>
    </div>
  );
}

function headSub(key: string, dir: Directory): string {
  const p = participant(dir, key);
  if (p.kind === "human") return "human";
  return stateLabel(p.state) + (p.machine ? ` · ${p.machine}` : "");
}

/** The selected arrow's message with Reply and Go to session, under the diagram. */
export function WireDetail({
  m,
  me,
  dir,
  onClose,
  onReply,
  onGo,
  held,
}: {
  m: ConversationMessage;
  me: Self;
  dir: Directory;
  /** For a saved agent's sessions, the roles it holds: another agent's queue is never answered. */
  held?: readonly string[];
  onClose: () => void;
  onReply: (key: string, inReplyTo: ConversationMessage) => void;
  onGo: (key: string) => void;
}) {
  const replyKey = replyTarget(m, me, held);
  const other = typeof me === "string" ? (m.from === me ? m.to : m.from) : replyKey;
  const goable =
    other && !isRoleKey(other) && participant(dir, other).kind === "session" ? other : null;
  return (
    <div className="ibx-wdetail" data-testid="ibx-wire-detail">
      <div className="mh">
        <Avatar k={m.from} dir={dir} sm />
        <b>{labelOf(dir, m.from)}</b> →{" "}
        <b>{m.toRole ? <InboxRole role={m.toRole} team={m.team} /> : labelOf(dir, toKeyOf(m))}</b>
        <span className="ibx-mono">{formatClock(m.createdAtMs)}</span>
        {m.via === "terminal" && <TypedVia />}
        <RepliesVia to={repliesViaKey(m)} dir={dir} team={m.team} />
        <span style={{ flex: 1 }} />
        <button
          type="button"
          className="ibx-iconbtn"
          title="Close"
          aria-label="Close"
          onClick={onClose}
        >
          <Ico icon={X} sm />
        </button>
      </div>
      <div className={m.via === "terminal" ? "tx term" : "tx"}>
        <MessageBody m={m} />
      </div>
      <div className="acts">
        <button type="button" className="ibx-gobtn" onClick={() => onReply(replyKey, m)}>
          <Ico icon={Reply} sm />
          Reply
        </button>
        {goable && (
          <GoButton
            k={goable}
            dir={dir}
            onGo={() => onGo(goable)}
            label={`Go to ${participant(dir, goable).name}`}
          />
        )}
      </div>
    </div>
  );
}

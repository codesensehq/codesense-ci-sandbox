// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import { toast } from "sonner";

import { sendFromInbox } from "../actions";
import type { ConversationMessage } from "../api";
import {
  HUMAN,
  counterpart,
  defaultRecipient,
  direction,
  filterSpool,
  formatClock,
  isRoleKey,
  isSelf,
  isUnreadFor,
  labelOf,
  msgKey,
  messageRowPreview,
  replyKeyFor,
  sendCheck,
  spoolItems,
  type SpoolFilter,
} from "../model";
import { useArrivalNote, useUnseenBelow } from "../hooks/use-new-mail";
import { NEAR_BOTTOM_PX, useAwayFromBottom } from "../hooks/use-stick-to-bottom";
import { selfOf, takeReplyRequest, useInboxView, type ReplyRequest } from "./inbox-view-context";
import { ArrivalsNote, JumpToLatest } from "./jump-to-latest";
import { HoverActions, MessageBody, goBlockedReason, isComposing } from "./primitives";

/** Estimated heights before a line is measured: a one-line row, a day divider. */
const ROW_PX = 26;
const DAY_PX = 31;

const FILTERS: [SpoolFilter, string][] = [
  ["all", "all"],
  ["unread", "is:unread"],
  ["roles", "is:role"],
  ["terminal", "via:terminal"],
];

/**
 * Mailspool: one dense line per message with a filter prompt. j/k move, ↵ opens the thread
 * inline with a `› reply to …:` line, r replies, g goes to the session.
 *
 * Chronological, oldest at the top: it opens at the newest line and follows new mail while the
 * reader is at the bottom; a reader who scrolled up stays put and is offered "Jump to latest".
 * Only the lines in view (plus a margin) are drawn, so the newest 500 the inbox keeps stay fast.
 */
export function MailspoolLens({ request }: { request: ReplyRequest | null }) {
  const view = useInboxView();
  const { dir, messages } = view;
  const [filter, setFilter] = useState<SpoolFilter>("all");
  const [query, setQuery] = useState("");
  const [sel, setSel] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const self = selfOf(view);
  const shown = useMemo(
    () => filterSpool(messages, self, filter, query, dir),
    [messages, self, filter, query, dir],
  );
  const cur = shown.find((m) => msgKey(m) === sel) ?? null;

  // A line to scroll to once it is in the list (a request may first clear the filter). A new
  // object each time, so asking for the same line again scrolls again.
  const [reveal, setReveal] = useState<{ key: string } | null>(null);

  // The reply line exists once its row is drawn, which a scroll to it can take a few frames.
  const focusReply = (tries = 12) =>
    requestAnimationFrame(() => {
      const input = listRef.current?.querySelector<HTMLInputElement>("[data-logreply]");
      // The reveal below scrolls it into view; focusing must not scroll a second time.
      if (input) input.focus({ preventScroll: true });
      else if (tries > 0) focusReply(tries - 1);
    });

  // A Reply action from elsewhere opens that party's latest line with its reply prompt.
  // With no line for that party there is nothing to open: the request is left for the lens the
  // inbox switches to (Threads), never spent here. A line the filter hides is shown again.
  useEffect(() => {
    if (!request) return;
    const last = [...messages].reverse().find((m) => counterpart(m, self) === request.key);
    if (!last || !takeReplyRequest(request)) return;
    if (!shown.includes(last)) {
      setFilter("all");
      setQuery("");
    }
    setSel(msgKey(last));
    setOpen(msgKey(last));
    setReveal({ key: msgKey(last) });
    focusReply();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `shown` is read when a request lands
  }, [request, messages, self]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if ((e.target as HTMLElement).matches("input, textarea")) return;
    // Single keys only: ⌘R, ⌘K, ⌃J… belong to the app and the system.
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const i = shown.findIndex((m) => msgKey(m) === sel);
    const at = (j: number) => {
      const m = shown[j];
      return m ? msgKey(m) : null;
    };
    if ((e.key === "j" || e.key === "ArrowDown" || e.key === "k" || e.key === "ArrowUp") && i < 0) {
      // Nothing selected yet: start where the reader is looking, the lowest line in view (the
      // newest, since the log opens at the bottom), not 500 lines up at the oldest.
      e.preventDefault();
      setSel(lowestInView());
    } else if (e.key === "j" || e.key === "ArrowDown") {
      e.preventDefault();
      setSel(at(Math.min(shown.length - 1, i + 1)));
    } else if (e.key === "k" || e.key === "ArrowUp") {
      e.preventDefault();
      setSel(at(Math.max(0, i - 1)));
    } else if (e.key === "Enter" && sel !== null) {
      e.preventDefault();
      setOpen((o) => (o === sel ? null : sel));
    } else if (e.key === "r" && cur) {
      e.preventDefault();
      setOpen(msgKey(cur));
      setReveal({ key: msgKey(cur) });
      focusReply();
    } else if (e.key === "g" && cur) {
      const k = counterpart(cur, self);
      if (isRoleKey(k)) return;
      // The same gate as the row's Go button: you, an ended or offline session say why.
      const blocked = goBlockedReason(k, dir);
      if (blocked) toast(blocked);
      else view.goTo(k);
    }
  };

  const items = useMemo(() => spoolItems(shown), [shown]);
  const indexOf = useMemo(() => new Map(items.map((it, i) => [it.key, i])), [items]);
  const idBase = useId();
  // Every other character spelled out by its code, so two keys never share an id.
  const rowId = (k: string) =>
    `${idBase}-${k.replace(/[^A-Za-z0-9-]/g, (c) => `_${c.codePointAt(0)!.toString(16)}_`)}`;

  // A new filter is a new view of the spool: it opens at its newest line, like the first one.
  const resetKey = `${filter}\u0000${query}`;
  const away = useAwayFromBottom(listRef, resetKey);
  const unseen = useUnseenBelow(shown, self, away, resetKey);
  const arrivals = useArrivalNote(messages, self, dir, view.me);

  const estimateSize = useCallback(
    (i: number) => (items[i]?.kind === "day" ? DAY_PX : ROW_PX),
    [items],
  );
  const getItemKey = useCallback((i: number) => items[i]?.key ?? i, [items]);
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => listRef.current,
    estimateSize,
    getItemKey,
    overscan: 12,
    // The log's own breathing room, known to the windowing so "the end" is the real end.
    paddingStart: 4,
    paddingEnd: 10,
    // An open line stays drawn while scrolled away, so its reply draft and focus survive.
    rangeExtractor: (range) => {
      const drawn = defaultRangeExtractor(range);
      const o = open === null ? undefined : indexOf.get(open);
      return o === undefined || drawn.includes(o) ? drawn : [...drawn, o].sort((a, b) => a - b);
    },
    // Keep what the reader is looking at still when old lines are trimmed off the top (the
    // inbox keeps the newest 500), and keep a reader at the bottom there as lines grow.
    anchorTo: "end",
    scrollEndThreshold: NEAR_BOTTOM_PX,
  });

  // Open at the newest line, and again whenever the filter changes.
  useLayoutEffect(() => {
    if (items.length) virtualizer.scrollToEnd();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only a new view re-opens at the end
  }, [resetKey]);
  // Follow new mail while the reader is at the bottom. The newest line's key as well as the
  // count: at the 500 cap a new line arrives as the oldest leaves.
  const lastKey = items.at(-1)?.key;
  useLayoutEffect(() => {
    if (lastKey !== undefined && !away) virtualizer.scrollToEnd();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `away` is read when a line lands
  }, [lastKey, items.length]);
  // A line asked for (a reply request, r, a row's Reply) is scrolled to once it is listed; this
  // runs after the open-at-the-end above, so a request that cleared the filter lands on its line.
  const revealed = useRef<object | null>(null);
  useLayoutEffect(() => {
    if (!reveal || revealed.current === reveal) return;
    const i = indexOf.get(reveal.key);
    if (i === undefined) return;
    revealed.current = reveal;
    virtualizer.scrollToIndex(i, { align: "auto" });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the virtualizer is one instance
  }, [reveal, indexOf]);

  /** The lowest line in view, else the first line below the view's top, else the newest. */
  const lowestInView = (): string | null => {
    const el = listRef.current;
    let key: string | null = null;
    if (el) {
      const top = el.scrollTop;
      const bottom = top + el.clientHeight;
      let below: string | null = null;
      for (const v of virtualizer.getVirtualItems()) {
        const it = items[v.index];
        if (it?.kind !== "msg") continue;
        if (v.start < bottom && v.end > top) key = it.key;
        else if (below === null && v.start >= bottom) below = it.key;
      }
      key ??= below;
    }
    const newest = shown.at(-1);
    return key ?? (newest ? msgKey(newest) : null);
  };

  // A slot leaving the window with focus inside (a row's hover action) hands focus to the log,
  // never the page body. One stable callback, so React calls it only as slots come and go.
  const slotRef = useCallback(
    (node: HTMLDivElement | null) => {
      virtualizer.measureElement(node);
      if (!node) return;
      return () => {
        if (node.contains(document.activeElement)) listRef.current?.focus({ preventScroll: true });
        // Still attached here: let the virtualizer forget it once it is gone.
        queueMicrotask(() => virtualizer.measureElement(null));
      };
    },
    [virtualizer],
  );

  const jumpToLatest = () => {
    virtualizer.scrollToEnd();
    // The pill goes away; the keyboard lands on the log rather than the page body.
    listRef.current?.focus({ preventScroll: true });
  };

  // The keyboard selection stays in view as j/k walk past the edge, drawn if it was not.
  useEffect(() => {
    if (sel === null) return;
    const i = indexOf.get(sel);
    if (i !== undefined) virtualizer.scrollToIndex(i, { align: "auto" });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only a move of the selection scrolls
  }, [sel]);

  const drawn = virtualizer.getVirtualItems();
  // Point assistive tech at the selected line only while it is drawn.
  const activeKey =
    cur && drawn.some((v) => items[v.index]?.key === msgKey(cur)) ? msgKey(cur) : null;
  return (
    <div className="ibx-spool">
      <div className="ibx-filter">
        <span className="pr">›</span>
        <input
          placeholder="from:@session  to:#role"
          aria-label="Filter messages"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            // ↓ or ↵ hands the keyboard to the results, starting at the first line.
            if ((e.key === "ArrowDown" || e.key === "Enter") && shown[0]) {
              e.preventDefault();
              if (!cur) setSel(msgKey(shown[0]));
              listRef.current?.focus();
            }
          }}
        />
        {FILTERS.map(([k, l]) => (
          <button
            key={k}
            type="button"
            className="f"
            aria-pressed={filter === k}
            onClick={() => setFilter(k)}
          >
            {l}
          </button>
        ))}
      </div>
      <div className="ibx-scrollwrap">
        <div
          className="ibx-log"
          tabIndex={0}
          ref={listRef}
          role="listbox"
          aria-label="Messages"
          aria-activedescendant={activeKey ? rowId(activeKey) : undefined}
          onKeyDown={onKeyDown}
          data-testid="ibx-log"
        >
          {items.length > 0 && (
            <div
              role="none"
              data-spool-sizer
              style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}
            >
              {drawn.map((v) => {
                const it = items[v.index];
                if (!it) return null;
                return (
                  <div
                    key={v.key}
                    role="none"
                    className="ibx-lslot"
                    data-index={v.index}
                    ref={slotRef}
                    style={{ transform: `translateY(${v.start}px)` }}
                  >
                    {it.kind === "day" ? (
                      <div className="ibx-lday">{it.label}</div>
                    ) : (
                      <>
                        <SpoolRow
                          m={it.m}
                          id={rowId(it.key)}
                          pos={it.pos + 1}
                          size={shown.length}
                          selected={sel === it.key}
                          onClick={() => {
                            // Clicking hands the keyboard to the list, so j/k carry on from here.
                            listRef.current?.focus({ preventScroll: true });
                            setSel(it.key);
                            setOpen((o) => (o === it.key ? null : it.key));
                          }}
                          onReply={() => {
                            setSel(it.key);
                            setOpen(it.key);
                            setReveal({ key: it.key });
                            focusReply();
                          }}
                        />
                        {open === it.key && <SpoolThread m={it.m} onClose={() => setOpen(null)} />}
                      </>
                    )}
                  </div>
                );
              })}
            </div>
          )}
          {shown.length === 0 && (
            <div className="ibx-empty" style={{ padding: 30 }}>
              {messages.length === 0 ? (
                <>
                  <strong>No messages yet</strong>
                  Sent, received and terminal-typed messages land here as one line each.
                </>
              ) : (
                "Nothing matches."
              )}
            </div>
          )}
        </div>
        <JumpToLatest unseen={away ? unseen : 0} onJump={jumpToLatest} />
      </div>
      <ArrivalsNote note={arrivals} />
      <div className="ibx-legend">
        <span>
          <kbd>j</kbd> <kbd>k</kbd> move
        </span>
        <span>
          <kbd>↵</kbd> open thread
        </span>
        <span>
          <kbd>r</kbd> reply
        </span>
        <span>
          <kbd>g</kbd> go to session
        </span>
        <span>
          <span className="in">←</span> in <span className="out">→</span> out{" "}
          <span className="term">⇢</span> typed <span className="q">⤳</span> role
        </span>
      </div>
    </div>
  );
}

function SpoolRow({
  m,
  id,
  pos,
  size,
  selected,
  onClick,
  onReply,
}: {
  m: ConversationMessage;
  id: string;
  /** 1-based place among the spool's lines, which are not all drawn at once. */
  pos: number;
  size: number;
  selected: boolean;
  onClick: () => void;
  onReply: () => void;
}) {
  const view = useInboxView();
  const { dir } = view;
  const self = selfOf(view);
  const k = counterpart(m, self);
  const d = direction(m, self);
  const cls = ["ibx-lrow", selected ? "sel" : "", isUnreadFor(m, self) ? "unread" : ""]
    .filter(Boolean)
    .join(" ");
  return (
    <div
      className={cls}
      id={id}
      role="option"
      aria-selected={selected}
      aria-posinset={pos}
      aria-setsize={size}
      data-log={msgKey(m)}
      onClick={onClick}
    >
      <span className="tm">{formatClock(m.createdAtMs)}</span>
      <span className={`dir ${d.kind}`} title={d.kind}>
        {d.glyph}
      </span>
      <span className={isRoleKey(k) ? "who r" : "who"}>
        {isRoleKey(k) ? k : `@${labelOf(dir, k)}`}
      </span>
      <span className="pv">{messageRowPreview(m)}</span>
      <HoverActions k={k} dir={dir} onReply={onReply} onGo={() => view.goTo(k)} onOpen={onReply} />
    </div>
  );
}

/** The expanded thread under a row, ending in a mono `› reply to …:` line. */
function SpoolThread({ m, onClose }: { m: ConversationMessage; onClose: () => void }) {
  const view = useInboxView();
  const { dir, messages, held } = view;
  const self = selfOf(view);
  const k = counterpart(m, self);
  const thread = messages.filter((x) => counterpart(x, self) === k);
  const [text, setText] = useState("");
  // One send at a time: a held or repeated ↵ must not send the reply twice.
  const sendingRef = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  // A reply line answers: an ended session's line goes to the role it held, if any.
  const to = replyKeyFor(
    defaultRecipient(k, self, held, thread) ?? k,
    dir,
    messages,
    view.roleScope,
  );
  const check = sendCheck(to, dir, messages, view.roleScope);
  const target = isRoleKey(to) ? to : `@${labelOf(dir, to)}`;
  const noteId = useId();
  // The reply answers the line that was opened, else the latest one the other side sent.
  const answers = !isSelf(self, m.from)
    ? m
    : ([...thread].reverse().find((x) => !isSelf(self, x.from)) ?? null);
  return (
    <div className="ibx-lx">
      {thread.map((x) => (
        <div key={msgKey(x)} className={x.via === "terminal" ? "li term" : "li"}>
          <div className="mh">
            {formatClock(x.createdAtMs)}
            {"  "}
            {x.from === HUMAN ? "you" : labelOf(dir, x.from)}
            {x.via === "terminal" ? "  ⇢ typed into terminal" : ""}
          </div>
          <div className="tx">
            <MessageBody m={x} />
          </div>
        </div>
      ))}
      <label className="rp">
        <span>› reply to {target}:</span>
        <input
          data-logreply={k}
          placeholder="type, ↵ to send, esc to close"
          aria-disabled={check.kind === "ended" || undefined}
          aria-describedby={check.kind === "ok" ? undefined : noteId}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={async (e) => {
            if (isComposing(e)) return;
            if (e.key === "Escape") {
              e.stopPropagation();
              // Back to the log, not the page body, once the reply line closes.
              e.currentTarget.closest<HTMLElement>(".ibx-log")?.focus();
              onClose();
            } else if (e.key === "Enter") {
              e.preventDefault();
              if (check.kind === "ended") return;
              if (sendingRef.current || !text.trim()) return;
              sendingRef.current = true;
              const sent = text;
              let ok = false;
              try {
                ok = await sendFromInbox({
                  to,
                  text: sent,
                  messages: view.messages,
                  fallbackTeam: view.fallbackTeam,
                  dir,
                  roles: view.roles,
                  inReplyTo: answers,
                });
              } finally {
                sendingRef.current = false;
              }
              if (ok && mounted.current) setText((t) => (t === sent ? "" : t));
            }
          }}
        />
      </label>
      {check.kind === "ended" ? (
        <div id={noteId} className="rp-note ended" data-testid="ibx-ended" role="status">
          {check.reason}
        </div>
      ) : check.kind === "offline" ? (
        <div id={noteId} className="rp-note">
          {check.hint}
        </div>
      ) : null}
    </div>
  );
}

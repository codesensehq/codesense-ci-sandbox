// SPDX-License-Identifier: MIT
import { useRoleConfigs, useRoleConfigsStore } from "@/shared/hooks/use-role-configs";
import { InboxRole } from "./inbox-role";
import { selectedIndexById } from "@/shared/lib/picker-selection";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { ArrowUp, ChevronDown, Loader2, Send } from "lucide-react";

import { sendFromInbox, teamFor } from "../actions";
import {
  HUMAN,
  isRoleKey,
  labelOf,
  machineLabel,
  participant,
  replyKeyFor,
  routingHint,
  sendCheck,
  stateLabel,
  type Participant,
} from "../model";
import { Avatar, Ico, isComposing } from "./primitives";
import {
  DraftStore,
  inboxDrafts,
  takeReplyRequest,
  type InboxDraft,
  useInboxView,
  type ReplyRequest,
} from "./inbox-view-context";
import type { ConversationMessage } from "../api";

/**
 * The composer every lens shares: a To picker (@sessions with state and machine, #roles with
 * their holder or queue note, @you) above the message box, with a line saying what happens to
 * the message once sent. It always sends as the human.
 *
 * `defaultTo` follows the selection; an explicit pick (or a Reply action, via `request`) wins
 * until the selection (`scope`, else `defaultTo`) changes. A live refresh that moves
 * `defaultTo` within the same selection never re-aims an explicit pick. Given a `scope` (a
 * thread), the draft belongs to it: switching threads keeps each one's draft and never carries
 * text written for one party over to another. Without one the draft simply stays in the box.
 */
export function Composer({
  defaultTo,
  scope,
  request,
  onSent,
  quick = false,
}: {
  defaultTo: string | null;
  /** What is selected (a thread key); defaults to `defaultTo`. */
  scope?: string | null;
  /** A Reply action: aim at `key` and take focus. `nonce` makes repeats count. */
  request?: ReplyRequest | null;
  /** Called with the recipient once a message has gone out. */
  onSent?: (to: string) => void;
  /**
   * The inbox chat's composer: the recipient is fixed (no To picker), Enter sends and
   * Shift+Enter starts a new line, and a send that fails says so under the box.
   */
  quick?: boolean;
}) {
  const view = useInboxView();
  const { dir, roles } = view;
  const selection = scope === undefined ? defaultTo : scope;
  // Each thread (`scope`) keeps its own draft, with the recipient it was written for (an
  // explicit pick or Reply, and the message it answers); an unscoped composer keeps one box. A
  // view with a `drafts` namespace keeps them past the composer itself, so closing the inbox
  // (or the rollup) and opening it again finds what was being written, still aimed where it was.
  const localDrafts = useRef(new DraftStore());
  const drafts = view.drafts ? inboxDrafts(view.drafts) : localDrafts.current;
  const draftKey = scope === undefined ? UNSCOPED : scope;
  const [picked, setPicked] = useState<Pick | null>(() => pickOf(drafts.get(draftKey), selection));
  const [text, setText] = useState(() => drafts.get(draftKey)?.text ?? "");
  const [picking, setPicking] = useState(false);
  // The draft whose send is on its way, and the draft whose last send failed (and why; cleared
  // by the next edit or send). Each shows only on its own draft: a thread switched to meanwhile
  // is neither read-only nor marked as failed.
  const [sendingFor, setSendingFor] = useState<{ scope: string | null } | null>(null);
  // A failure the backend refused (`retryable` false) is not offered again as it stands.
  const [failure, setFailure] = useState<{
    scope: string | null;
    why: string;
    retryable: boolean;
  } | null>(null);
  // An Enter on an empty quick box: a brief "write a message first" in place of the hint.
  const [emptyNudge, setEmptyNudge] = useState(false);
  useEffect(() => {
    if (!emptyNudge) return;
    const timer = window.setTimeout(() => setEmptyNudge(false), EMPTY_NUDGE_MS);
    return () => window.clearTimeout(timer);
  }, [emptyNudge]);
  const sendingRef = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [draftScope, setDraftScope] = useState(draftKey);
  if (draftScope !== draftKey) {
    keepDraft(drafts, draftScope, text, picked && picked.base === draftScope ? picked : null);
    setDraftScope(draftKey);
    const saved = drafts.get(draftKey);
    setText(saved?.text ?? "");
    setPicked(pickOf(saved, selection));
  }
  const sending = sendingFor !== null && sendingFor.scope === draftKey;
  const failedNow = failure !== null && failure.scope === draftKey ? failure : null;
  const failed = failedNow?.why ?? null;
  /** Clear `key`'s failure (another draft's stays). */
  const clearFailure = (key: string | null) =>
    setFailure((f) => (f !== null && f.scope === key ? null : f));
  const boxRef = useRef<HTMLTextAreaElement>(null);
  const hintId = useId();
  // The box re-grows from the swapped-in draft's own height.
  useEffect(() => {
    const box = boxRef.current;
    if (box) {
      box.style.height = "";
      if (box.value) grow(box);
    }
  }, [draftScope]);
  const toRef = useRef<HTMLButtonElement>(null);
  // Which draft the box shows now, for a send that resolves after a thread switch.
  const shownScope = useRef(draftKey);
  shownScope.current = draftKey;
  const current = picked && picked.base === selection ? picked : null;
  const to = current ? current.to : defaultTo;
  const recipientTeam =
    current?.inReplyTo?.team ??
    current?.team ??
    (to
      ? teamFor(
          to,
          view.messages,
          (isRoleKey(to) ? roles.find((r) => r.role === to.slice(1))?.team : null) ??
            view.fallbackTeam,
        )
      : null);
  // The latest box and pick, for a send that settles after either moved.
  const textRef = useRef(text);
  textRef.current = text;
  const currentRef = useRef(current);
  currentRef.current = current;
  // The kept draft follows the box and its recipient.
  useEffect(() => {
    keepDraft(drafts, draftKey, text, current);
  }, [drafts, draftKey, text, current]);
  // A send from a composer that has since gone failed: its text comes back here, into an
  // empty box (never over what is being written now).
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const draftKeyRef = useRef(draftKey);
  draftKeyRef.current = draftKey;
  useEffect(
    () =>
      drafts.subscribe((key) => {
        const back = drafts.get(key);
        if (key !== draftKeyRef.current || !back || textRef.current) return;
        setText(back.text);
        setPicked(pickOf(back, selectionRef.current));
      }),
    [drafts],
  );

  // A Reply is honoured once, even by a composer it mounts (a Reply that opens the thread).
  useEffect(() => {
    if (!takeReplyRequest(request)) return;
    // A Reply to a session that ended goes to the role it held, when it held one.
    const key = replyKeyFor(request.key, dir, view.messages, view.roleScope);
    setPicked({ base: selection, to: key, inReplyTo: request.inReplyTo ?? null });
    boxRef.current?.focus();
  }, [request, selection, dir, view.messages, view.roleScope]);

  const hint = routingHint(to, dir, roles);
  const check = sendCheck(to, dir, view.messages, view.roleScope);
  const ended = check.kind === "ended" ? check : null;
  const target = to ? (isRoleKey(to) ? to : `@${labelOf(dir, to)}`) : null;

  const send = async () => {
    // Nothing to send: the chat says so instead of doing nothing.
    if (quick && !text.trim() && to && !ended && !sendingRef.current) {
      setEmptyNudge(true);
      return;
    }
    // A ref, not the `sending` state: two ⌘↵ before a re-render must not both send.
    if (!to || ended || !text.trim() || sendingRef.current) return;
    sendingRef.current = true;
    const sentScope = draftKey;
    setSendingFor({ scope: sentScope });
    clearFailure(sentScope);
    setEmptyNudge(false);
    let why = "";
    // A send that settles with no reason given never reached the backend's answer.
    let retryable = true;
    const sent = text;
    const sentPick = current;
    // Out of the kept drafts while it is on its way: a surface closed and reopened meanwhile
    // must not offer it again. The box keeps it until the send lands.
    drafts.inFlight.set(sentScope, sent);
    if (drafts.get(sentScope)?.text === sent) drafts.delete(sentScope);
    let ok = false;
    try {
      ok = await sendFromInbox({
        to,
        text: sent,
        messages: view.messages,
        fallbackTeam: view.fallbackTeam,
        team: recipientTeam,
        dir,
        roles,
        inReplyTo: current?.inReplyTo ?? null,
        // The chat shows the message land at its foot. A role queue's note, or a session whose
        // machine is offline (the message waits), still says where it went.
        quiet: quick && !isRoleKey(to) && check.kind === "ok",
        // The chat says why under the box; elsewhere the toast does.
        onFailed: quick
          ? (reason, canRetry) => {
              why = reason;
              retryable = canRetry !== false;
            }
          : undefined,
      });
    } finally {
      sendingRef.current = false;
      if (drafts.inFlight.get(sentScope) === sent) drafts.inFlight.delete(sentScope);
      if (mounted.current) setSendingFor(null);
    }
    if (!ok) {
      if (mounted.current)
        setFailure({
          scope: sentScope,
          why: why.replace(/[\s.]+$/, "") || "it did not go out",
          retryable,
        });
      // Not sent: the draft is kept again, as it now stands (or as it was, once closed, handed
      // to a composer showing that thread now).
      if (mounted.current && shownScope.current === sentScope)
        keepDraft(drafts, sentScope, textRef.current, currentRef.current);
      else if (!drafts.has(sentScope)) {
        keepDraft(drafts, sentScope, sent, sentPick);
        drafts.announce(sentScope);
      }
      return;
    }
    // Clear only what was sent: text typed meanwhile (or another thread's draft) stays, and
    // no longer answers the message this send answered, even when the surface has closed.
    const kept = drafts.get(sentScope);
    if (kept?.text === sent) drafts.delete(sentScope);
    else if (kept?.inReplyTo && kept.inReplyTo === sentPick?.inReplyTo)
      drafts.set(sentScope, { ...kept, inReplyTo: null });
    if (!mounted.current) return;
    if (shownScope.current === sentScope) setText((t) => (t === sent ? "" : t));
    // A reply answers once; the next message in the thread is not another reply to it. Only
    // the pick this send used: a Reply made elsewhere while it was in flight keeps its target.
    setPicked((p) => (p && p === sentPick && p.inReplyTo ? { ...p, inReplyTo: null } : p));
    if (boxRef.current) boxRef.current.style.height = "";
    onSent?.(to);
  };

  // The chat says where a message goes in its own words; an offline or role recipient keeps
  // the routing line, which says what happens to it, unless the chat already says the machine
  // is offline. An ended recipient's note stands above the box instead.
  const quickHint = ended
    ? ""
    : to &&
        !isRoleKey(to) &&
        (check.kind === "ok" || (check.kind === "offline" && view.offlineNoted))
      ? `To ${labelOf(dir, to)}'s inbox · you stay on your tab`
      : hint.text;
  const endedNote = ended ? (
    <span className="ibx-tohint ended" data-testid="ibx-ended">
      {/* Only the reason is announced; the buttons beside it are controls, not news. */}
      <span role="status">{ended.reason}</span>
      {/* Not in the chat: it has no recipient picker, and role mail is not shown there. It
          offers the role's own inbox instead, when it can open one. */}
      {ended.role && quick && view.openRoleInbox && (
        <button
          type="button"
          className="ibx-link"
          data-testid="ibx-open-role-inbox"
          onClick={() => view.openRoleInbox?.(`#${ended.role}`)}
        >
          Open #{ended.role}'s inbox
        </button>
      )}
      {ended.role && !quick && (
        <button
          type="button"
          className="ibx-chip role"
          onClick={() => {
            setPicked({
              base: selection,
              to: `#${ended.role}`,
              inReplyTo: current?.inReplyTo ?? null,
            });
            boxRef.current?.focus();
          }}
        >
          Send to #{ended.role} instead
        </button>
      )}
    </span>
  ) : null;

  return (
    <div className={quick ? "ibx-comp quick" : "ibx-comp"} data-testid="ibx-composer">
      {quick ? (
        endedNote
      ) : (
        <div className="ibx-to">
          <span>To</span>
          <button
            type="button"
            className="ibx-tobtn"
            ref={toRef}
            aria-haspopup="listbox"
            aria-expanded={picking}
            aria-label={target ? `To ${target}, change recipient` : "Pick a recipient"}
            title={target ?? undefined}
            data-testid="ibx-to"
            onClick={() => setPicking((p) => !p)}
          >
            {to ? (
              <>
                <Avatar k={to} dir={dir} team={recipientTeam} sm />
                <span className="k">
                  {to && isRoleKey(to) ? (
                    <InboxRole role={to.slice(1)} team={recipientTeam} nameOnly />
                  ) : (
                    target
                  )}
                </span>
              </>
            ) : (
              <span className="k" style={{ color: "var(--ibx-mfg)" }}>
                pick…
              </span>
            )}
            <Ico icon={ChevronDown} sm />
          </button>
          {endedNote ?? (
            <span
              className={hint.role ? "ibx-tohint role" : "ibx-tohint"}
              data-testid="ibx-route-hint"
            >
              {hint.text}
            </span>
          )}
          {picking && (
            <RecipientPicker
              onPick={(key, team) => {
                setPicked({ base: selection, to: key, team });
                setPicking(false);
                requestAnimationFrame(() => boxRef.current?.focus());
              }}
              onClose={() => {
                setPicking(false);
                toRef.current?.focus();
              }}
            />
          )}
        </div>
      )}
      <div className="ibx-box">
        <textarea
          ref={boxRef}
          rows={1}
          value={text}
          aria-label="Message"
          placeholder={
            quick && to
              ? `Message ${labelOf(dir, to)}`
              : target
                ? `Message ${target}`
                : "Write a message"
          }
          readOnly={quick && sending}
          aria-describedby={quick ? hintId : undefined}
          onChange={(e) => {
            setText(e.target.value);
            clearFailure(draftKey);
            setEmptyNudge(false);
            grow(e.currentTarget);
          }}
          onKeyDown={(e) => {
            if (isComposing(e)) return;
            if (quick && e.key === "Enter" && e.altKey && !e.metaKey && !e.ctrlKey) {
              // ⌥↵ is a new line in the chat, as ⇧↵ is (the webview would not insert one). Not
              // while the box is read-only, sending; an edit clears a failure or nudge as typing does.
              e.preventDefault();
              if (sending) return;
              insertNewline(e.currentTarget, setText);
              clearFailure(draftKey);
              setEmptyNudge(false);
            } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey || (quick && !e.shiftKey))) {
              e.preventDefault();
              void send();
            } else if (e.key === "Escape") {
              // Leave the box, keep the draft: focus returns to what the composer answers.
              e.preventDefault();
              leaveComposer(e.currentTarget, toRef.current);
            }
          }}
        />
        {quick ? (
          <button
            type="button"
            className="ibx-send quick"
            aria-label={sending ? "Sending" : "Send"}
            aria-busy={sending || undefined}
            // Kept enabled on an empty box: a click says to write a message first.
            disabled={!to || !!ended || sending}
            title={ended?.reason ?? "Send (↵)"}
            onClick={() => void send()}
          >
            <Ico icon={sending ? Loader2 : ArrowUp} sm />
          </button>
        ) : (
          <button
            type="button"
            className="ibx-send"
            disabled={!to || !!ended || sending}
            title={ended?.reason}
            onClick={() => void send()}
          >
            <Ico icon={Send} sm />
            Send <kbd>⌘↵</kbd>
          </button>
        )}
      </div>
      {quick && (
        <div className="ibx-comp-foot" id={hintId}>
          {failed !== null && (
            <span className="ibx-comp-err" role="alert" data-testid="ibx-send-failed">
              Not sent: {failed}. Your message is still here
              {failedNow?.retryable ? "; press ↵ to try again." : "."}
            </span>
          )}
          {emptyNudge && failed === null && (
            <span className="ibx-comp-err" role="status" data-testid="ibx-send-empty">
              Write a message first
            </span>
          )}
          {/* Kept beside a failure: where the message was going is part of why it did not. */}
          {quickHint && !emptyNudge && (
            <span className="ibx-tohint" data-testid="ibx-route-hint" title={quickHint}>
              {quickHint}
            </span>
          )}
          <span className="ibx-comp-keys" aria-hidden>
            <kbd>↵</kbd> send <kbd>⇧↵</kbd> new line
          </span>
        </div>
      )}
    </div>
  );
}

/** An explicit recipient for the selection `base`: a pick from the To list, or a Reply. */
interface Pick {
  base: string | null;
  to: string;
  inReplyTo?: ConversationMessage | null;
  team?: ConversationMessage["team"] | null;
}

/** How long the chat's "write a message first" shows after an Enter on an empty box. */
const EMPTY_NUDGE_MS = 2000;

/** The kept key of an unscoped composer's one box. */
const UNSCOPED = "\u0000unscoped";

/** Record `text` (and its explicit recipient) as `key`'s draft; an emptied box leaves none. */
function keepDraft(drafts: DraftStore, key: string | null, text: string, pick: Pick | null) {
  // A text on its way is not a draft: kept, a surface reopened meanwhile would offer it again.
  if (text && drafts.inFlight.get(key) !== text)
    drafts.set(key, {
      text,
      to: pick?.to ?? null,
      inReplyTo: pick?.inReplyTo ?? null,
      team: pick?.team,
    });
  else drafts.delete(key);
}

/** A kept draft's explicit recipient, for the selection it comes back under. */
function pickOf(saved: InboxDraft | undefined, base: string | null): Pick | null {
  return saved?.to
    ? { base, to: saved.to, inReplyTo: saved.inReplyTo ?? null, team: saved.team }
    : null;
}

/** Put a line break at the box's caret (over any selection), the caret after it. */
function insertNewline(box: HTMLTextAreaElement, setText: (text: string) => void) {
  const { selectionStart: start, selectionEnd: end, value } = box;
  const next = `${value.slice(0, start)}\n${value.slice(end)}`;
  // Set on the element first, so the caret can follow before React re-renders the same value.
  box.value = next;
  box.setSelectionRange(start + 1, start + 1);
  setText(next);
  grow(box);
}

/** Grow the message box with its text, up to its CSS max-height. */
function grow(el: HTMLTextAreaElement) {
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}

/**
 * Escape out of the message box: back to the open row (the conversation the composer answers)
 * when there is one, else to the To picker (else the messages), so a keyboard user never lands on the page body.
 */
function leaveComposer(box: HTMLTextAreaElement, fallback: HTMLElement | null) {
  const root = box.closest(".ibx");
  const back = root?.querySelector<HTMLElement>(
    '[data-ibx-nav][aria-current="true"], .ibx-arrow.sel, .ibx-lrow.sel',
  );
  // With no To picker (the inbox chat), the conversation above the box.
  const target =
    back?.closest<HTMLElement>(".ibx-log") ??
    back ??
    fallback ??
    root?.querySelector<HTMLElement>(".ibx-msgs");
  if (target) target.focus();
  else box.blur();
}

function RecipientPicker({
  onPick,
  onClose,
}: {
  onPick: (key: string, team?: ConversationMessage["team"] | null) => void;
  onClose: () => void;
}) {
  const { dir, roles, targets, fallbackTeam, messages } = useInboxView();
  const byTeam = useRoleConfigsStore((s) => s.byTeam);
  useRoleConfigs(fallbackTeam ? `${fallbackTeam.org}:${fallbackTeam.team}` : undefined);
  const [query, setQuery] = useState("");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const listId = useId();

  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      const el = ref.current;
      if (
        el &&
        !el.contains(e.target as Node) &&
        !(e.target as Element).closest?.("[data-testid='ibx-to']")
      )
        onClose();
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  }, [onClose]);

  const q = query.replace(/^[@#]/, "").toLowerCase();
  const sessions = useMemo(
    () =>
      [...dir.values()].filter(
        // The human may write to any session, the one whose inbox this is included.
        (p: Participant) => p.kind === "session" && p.state !== "unknown",
      ),
    [dir],
  );
  const matches = (s: string) => !q || s.toLowerCase().includes(q);
  const roleOpt = (r: {
    role: string;
    holder: string | null;
    holderCount?: number;
    team?: ConversationMessage["team"] | null;
  }) => {
    const team = teamFor(`#${r.role}`, messages, r.team ?? fallbackTeam);
    const choices = team ? (byTeam[`${team.org}:${team.team}`] ?? []) : [];
    return {
      key: `#${r.role}`,
      team,
      label: `${choices.find((c) => c.id === r.role)?.name ?? r.role} #${r.role}`,
      detail:
        r.holderCount === 0
          ? "no holders · queued"
          : r.holderCount !== undefined
            ? `${r.holderCount} holder${r.holderCount === 1 ? "" : "s"}`
            : r.holder
              ? "1 holder"
              : "no holders · queued",
      group: "Roles: queued until an agent claims it",
    };
  };
  const sessionOpt = (p: Participant) => ({
    key: p.key,
    team: undefined,
    label: `@${p.name}`,
    // A session id names one launch: an ended one never reads its mail again.
    detail: `${p.state === "exited" ? "ended" : stateLabel(p.state)} · ${machineLabel(p)}`,
    group: "Sessions",
    ended: p.state === "exited",
  });
  // A scoped view (a saved agent) offers only its own sessions and its role queue.
  const scoped = targets
    ? targets.map((k) =>
        isRoleKey(k)
          ? {
              ...roleOpt(
                roles.find((r) => `#${r.role}` === k) ?? { role: k.slice(1), holder: null },
              ),
              group: "Its role inbox",
            }
          : { ...sessionOpt(participant(dir, k)), group: "Its sessions" },
      )
    : null;
  const opts: {
    key: string;
    label: string;
    detail: string;
    group: string;
    ended?: boolean;
    team?: ConversationMessage["team"] | null;
  }[] = (
    scoped ?? [
      ...sessions.map(sessionOpt),
      ...roles.map(roleOpt),
      { key: HUMAN, label: `@${labelOf(dir, HUMAN)}`, detail: "human", group: "People" },
    ]
  )
    // A leading # or @ narrows to roles or to people, so ↵ picks the kind that was typed.
    .filter((o) => !query.startsWith("#") || isRoleKey(o.key))
    .filter((o) => !query.startsWith("@") || !isRoleKey(o.key))
    .filter((o) => matches(`${o.label} ${o.detail}`));
  const at = selectedIndexById(opts, selectedKey, (o) => o.key);
  useEffect(() => {
    document.getElementById(`${listId}-${at}`)?.scrollIntoView?.({ block: "nearest" });
  }, [at, listId]);

  return (
    <div className="ibx-pick" ref={ref} data-testid="ibx-picker">
      <input
        autoFocus
        placeholder="@session or #role"
        aria-label="Find recipient"
        value={query}
        role="combobox"
        aria-expanded
        aria-controls={listId}
        aria-activedescendant={opts[at] ? `${listId}-${at}` : undefined}
        onChange={(e) => {
          setQuery(e.target.value);
          setSelectedKey(null);
        }}
        onKeyDown={(e) => {
          if (isComposing(e)) return;
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            const step = e.key === "ArrowDown" ? 1 : -1;
            const next = (at + step + opts.length) % Math.max(1, opts.length);
            setSelectedKey(opts[next]?.key ?? null);
          } else if (e.key === "Enter" && opts[at]) {
            e.preventDefault();
            onPick(opts[at].key, opts[at].team);
          }
        }}
      />
      <div id={listId} role="listbox" aria-label="Recipients">
        {opts.map((o, i) => {
          const head = i === 0 || opts[i - 1]!.group !== o.group ? o.group : null;
          return (
            <div key={o.key} role="presentation">
              {head && (
                <div className="g" role="presentation">
                  {head}
                </div>
              )}
              <div
                id={`${listId}-${i}`}
                role="option"
                aria-selected={i === at}
                className={["opt", i === at ? "hi" : "", o.ended ? "ended" : ""]
                  .filter(Boolean)
                  .join(" ")}
                title={`${o.label} · ${o.detail}`}
                onPointerMove={() => setSelectedKey(o.key)}
                onClick={() => onPick(o.key, o.team)}
              >
                {isRoleKey(o.key) ? null : <Avatar k={o.key} dir={dir} sm />}
                <span className="k">
                  {isRoleKey(o.key) ? (
                    <>
                      <InboxRole role={o.key.slice(1)} team={o.team} />
                      <span className="ml-1 text-xs text-muted-foreground">#{o.key.slice(1)}</span>
                    </>
                  ) : (
                    o.label
                  )}
                </span>
                <span className="d">{o.detail}</span>
              </div>
            </div>
          );
        })}
      </div>
      {opts.length === 0 && <div className="g">Nobody matches “{query}”</div>}
    </div>
  );
}

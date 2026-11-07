// SPDX-License-Identifier: MIT
import { InboxRole } from "./inbox-role";
import {
  AlertTriangle,
  Archive,
  ArrowRightLeft,
  ArrowUpRight,
  Check,
  List,
  MessageSquare,
  Reply,
  RotateCw,
  Terminal,
  type LucideIcon,
} from "lucide-react";

import { isLiveInboxMessage } from "@/features/weft/lib/live-inbox-message";
import { InboxMessageContent } from "@/features/weft/components/inbox-message-content";
import type { InboxLens } from "@/features/viewer/stores/agent-inbox-rail-store";

import type { ConversationMessage } from "../api";
import {
  dotClass,
  initials,
  isLive,
  isRoleKey,
  labelOf,
  participant,
  replyToKey,
  type ClaimBadge,
  type Directory,
} from "../model";

/** A key that belongs to an IME composition in progress: ↵ there picks a candidate. */
export function isComposing(e: React.KeyboardEvent): boolean {
  return e.nativeEvent.isComposing || e.keyCode === 229;
}

/**
 * Keyboard for a list row: ↑/↓ (and Home/End) move focus between the rows of the nearest
 * `[data-ibx-navscope]`, and Enter or Space activates a row that is not a native button.
 * Spread `navRow(activate)` onto the row; it marks the row and handles its keys.
 */
export function navRow(activate?: () => void) {
  return {
    "data-ibx-nav": "",
    onKeyDown: (e: React.KeyboardEvent<HTMLElement>) => {
      if (e.target !== e.currentTarget) return;
      const row = e.currentTarget;
      if ((e.key === "Enter" || e.key === " ") && activate && row.tagName !== "BUTTON") {
        e.preventDefault();
        activate();
        return;
      }
      const moves: Record<string, number> = {
        ArrowDown: 1,
        ArrowUp: -1,
        Home: -Infinity,
        End: Infinity,
      };
      const step = moves[e.key];
      if (step === undefined) return;
      const scope = row.closest("[data-ibx-navscope]") ?? row.parentElement;
      const rows = [...(scope?.querySelectorAll<HTMLElement>("[data-ibx-nav]") ?? [])];
      const i = rows.indexOf(row);
      const j = Math.max(
        0,
        Math.min(
          rows.length - 1,
          Number.isFinite(step) ? i + step : step < 0 ? 0 : rows.length - 1,
        ),
      );
      e.preventDefault();
      rows[j]?.focus();
      rows[j]?.scrollIntoView?.({ block: "nearest" });
    },
  };
}

/** The first read is in flight: say so, so the pane is never blank. */
export function InboxLoading({ what }: { what: string }) {
  return (
    <div className="ibx-empty ibx-loading" role="status" data-testid="ibx-loading">
      Loading {what}…
    </div>
  );
}

/** The first read failed: what failed, the backend's words, and a way to try again. */
export function ReadFailure({
  what,
  error,
  onRetry,
}: {
  what: string;
  error: string;
  onRetry?: () => void;
}) {
  return (
    <div className="ibx-empty" role="alert">
      <strong>Couldn’t read {what}</strong>
      <span className="ibx-err">{error}</span>
      {onRetry && (
        <button type="button" className="ibx-gobtn" data-testid="ibx-read-retry" onClick={onRetry}>
          <Ico icon={RotateCw} sm />
          Try again
        </button>
      )}
    </div>
  );
}

/** A refresh failed after an earlier read landed: keep that read on screen and say it is stale. */
export function StaleNotice({ error, onRetry }: { error: string; onRetry: () => void }) {
  return (
    <div className="ibx-stale" role="alert">
      <Ico icon={AlertTriangle} sm />
      <span className="tx" title={error}>
        Couldn’t refresh the inbox: {error}. Showing the last read.
      </span>
      <button type="button" className="ibx-gobtn" onClick={onRetry}>
        Retry
      </button>
    </div>
  );
}

/** A Lucide glyph at the inbox's two sizes (15px, 13px small) and stroke weight. */
export function Ico({ icon: Icon, sm }: { icon: LucideIcon; sm?: boolean }) {
  return <Icon className={sm ? "ibx-ico sm" : "ibx-ico"} strokeWidth={1.75} aria-hidden />;
}

/** A square initials avatar with a state dot; `#` for a role queue, the viewer's mark for the human. */
export function Avatar({
  k,
  dir,
  sm,
  team,
}: {
  k: string;
  dir: Directory;
  sm?: boolean;
  team?: ConversationMessage["team"] | null;
}) {
  const size = sm ? " sm" : "";
  if (isRoleKey(k)) {
    return (
      <span className={`ibx-av role${size}`}>
        <InboxRole role={k.slice(1)} team={team} iconOnly size={sm ? 20 : 28} />
      </span>
    );
  }
  const p = participant(dir, k);
  if (p.kind === "human") {
    return (
      <span className={`ibx-av human${size}`} aria-hidden>
        {p.name === "you" ? "You".slice(0, sm ? 1 : 3) : initials(p.name)}
      </span>
    );
  }
  return (
    <span className={`ibx-av${size}`} style={{ background: p.color }} aria-hidden>
      {initials(p.name)}
      <span className={`ibx-dot st ${dotClass(p.state)}`} />
    </span>
  );
}

/** A small mono tag naming the remote machine a session runs on. */
export function MachineTag({ k, dir }: { k: string; dir: Directory }) {
  const machine = isRoleKey(k) ? null : participant(dir, k).machine;
  return machine ? <span className="ibx-mtag">{machine}</span> : null;
}

/** Why "Go to session" cannot act, or null when it can. */
export function goBlockedReason(k: string, dir: Directory): string | null {
  const p = participant(dir, k);
  if (p.kind === "human") return "That’s you";
  if (p.state === "offline") return "Machine offline, cannot open";
  if (!isLive(p)) return "Session exited, cannot open";
  return null;
}

/**
 * The row hover cluster: Reply and Go to session (disabled with its reason when it cannot),
 * plus Archive where the human reads their own mail.
 */
export function HoverActions({
  k,
  dir,
  onReply,
  onGo,
  onOpen,
  onArchive,
}: {
  k: string;
  dir: Directory;
  onReply: () => void;
  onGo: () => void;
  onOpen?: () => void;
  onArchive?: () => void;
}) {
  const stop = (fn: () => void) => (e: React.MouseEvent) => {
    e.stopPropagation();
    fn();
  };
  if (isRoleKey(k)) {
    return (
      <div className="ibx-hov">
        <button
          type="button"
          title="Open queue"
          aria-label={`Open ${k}`}
          onClick={stop(onOpen ?? onReply)}
        >
          <Ico icon={Reply} sm />
        </button>
      </div>
    );
  }
  const blocked = goBlockedReason(k, dir);
  const name = participant(dir, k).name;
  return (
    <div className="ibx-hov">
      <button type="button" title="Reply" aria-label={`Reply to ${name}`} onClick={stop(onReply)}>
        <Ico icon={Reply} sm />
      </button>
      <button
        type="button"
        className="go"
        title={blocked ?? `Go to ${name}`}
        aria-label={blocked ? `Go to ${name}: ${blocked}` : `Go to ${name}`}
        aria-disabled={blocked ? true : undefined}
        onClick={stop(() => {
          if (!blocked) onGo();
        })}
      >
        <Ico icon={ArrowUpRight} sm />
      </button>
      {onArchive && (
        <button
          type="button"
          title="Archive"
          aria-label={`Archive ${name}'s message`}
          onClick={stop(onArchive)}
        >
          <Ico icon={Archive} sm />
        </button>
      )}
    </div>
  );
}

/** "Go to session", disabled with its reason when the session cannot be opened. */
export function GoButton({
  k,
  dir,
  onGo,
  label,
}: {
  k: string;
  dir: Directory;
  onGo: () => void;
  label?: string;
}) {
  const blocked = goBlockedReason(k, dir);
  return (
    <button
      type="button"
      className="ibx-gobtn"
      title={blocked ?? label ?? "Go to session"}
      aria-disabled={blocked ? true : undefined}
      onClick={() => {
        if (!blocked) onGo();
      }}
    >
      <Ico icon={ArrowUpRight} sm />
      <span className="lbl">{label ?? "Go to session"}</span>
    </button>
  );
}

/**
 * A message body: terminal-typed text verbatim, anything else through the shared renderer.
 * `liveDashboards`: where the human reads their own conversations (the docked panel and the
 * rollup's conversation), an agent's HTML to the human is a live dashboard whose buttons and
 * forms answer it, as the old conversation view rendered it. Everywhere else (a tab's inbox,
 * an agent's page, Mailspool, Wire) it stays sanitized static HTML, so its scripts run in one
 * place, not once per surface showing it.
 */
export function MessageBody({
  m,
  liveDashboards = false,
}: {
  m: ConversationMessage;
  liveDashboards?: boolean;
}) {
  if (m.via === "terminal") return <>{m.text}</>;
  const live =
    liveDashboards && !m.toRole && isLiveInboxMessage(m)
      ? { team: m.team, messageId: m.id, sender: m.from }
      : null;
  const capTarget =
    m.onClick?.type === "goto-agent" && m.onClick.agent_id.endsWith("#cap")
      ? m.onClick.agent_id.slice(0, -4)
      : null;
  return (
    <>
      <InboxMessageContent text={m.text} contentType={m.contentType} live={live} />
      {capTarget && (
        <button
          type="button"
          className="mt-2 underline underline-offset-2 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          onClick={() =>
            void import("@/features/agents/agents-virtual-tab").then(({ openAgentCap }) =>
              openAgentCap(capTarget),
            )
          }
        >
          Review spend cap
        </button>
      )}
    </>
  );
}

/** Whether `m` carries the "typed into terminal" marker. */
export const hasTypedVia = (m: ConversationMessage): boolean => m.via === "terminal";

/** Where replies to `m` go when its sender set a Reply-To other than itself, else null. */
export const repliesViaKey = (m: ConversationMessage): string | null => {
  const key = replyToKey(m);
  return key && key !== m.from ? key : null;
};

/** The dashed "typed into terminal" marker. */
export function TypedVia() {
  return (
    <span className="ibx-via" data-testid="ibx-typed-via">
      <Ico icon={Terminal} sm />
      typed into terminal
    </span>
  );
}

/** "Replies go to …" for a message whose Reply-To (`to`, from {@link repliesViaKey}) is not its
 *  sender; nothing when `to` is null. */
export function RepliesVia({
  to,
  dir,
  team,
}: {
  to: string | null;
  dir: Directory;
  team?: ConversationMessage["team"];
}) {
  if (!to) return null;
  return (
    <span
      className="ibx-via"
      data-testid="ibx-replies-go-to"
      title="Replies go here, not to the sender"
    >
      <Ico icon={Reply} sm />
      replies go to{" "}
      {isRoleKey(to) ? <InboxRole role={to.slice(1)} team={team} /> : labelOf(dir, to)}
    </span>
  );
}

/** A role message's claim state (`badge`, from `claimBadge`): queued (violet) or claimed (green). */
export function ClaimVia({ badge }: { badge: ClaimBadge | null }) {
  if (!badge) return null;
  const cls = badge.kind === "claimed" ? "claimed" : badge.kind === "queued" ? "queued" : "";
  return (
    <span className={`ibx-via ${cls}`} data-testid="ibx-claim">
      <Ico icon={badge.kind === "claimed" ? Check : Reply} sm />
      {badge.label}
    </span>
  );
}

export const LENSES: { key: InboxLens; icon: LucideIcon; label: string }[] = [
  { key: "threads", icon: MessageSquare, label: "Threads" },
  { key: "spool", icon: List, label: "Mailspool" },
  { key: "wire", icon: ArrowRightLeft, label: "Wire" },
];

/** The segmented Threads | Mailspool | Wire switch. */
export function LensSwitch({
  lens,
  onChange,
  label,
}: {
  lens: InboxLens;
  onChange: (lens: InboxLens) => void;
  label: string;
}) {
  return (
    <div className="ibx-lens" role="group" aria-label={label}>
      {LENSES.map((l) => (
        <button
          key={l.key}
          type="button"
          aria-pressed={lens === l.key}
          title={l.label}
          onClick={() => onChange(l.key)}
        >
          <Ico icon={l.icon} sm />
          <span>{l.label}</span>
        </button>
      ))}
    </div>
  );
}

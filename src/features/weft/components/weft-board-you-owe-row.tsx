// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";

import {
  wvMarkApprovalSurfaced,
  type WvApprovalRow,
  type WvPendingAssertionDto,
} from "@/tauri/commands";
import { EndorsementPill } from "./endorsement-pill";
import { RecipientAckEcho } from "./recipient-ack-echo";
import { T2WindowBadge } from "./t2-window-badge";

/** SW-WV-F5: the truncation ceiling for a row's claim text (AC-1: ~140 chars). */
const CLAIM_MAX = 140;

/** SW-WV-F5 (AC-4): the surfacing session stamped when the operator's UI surfaces a T2
 *  window. The webview is a single operator context (not a worker's per-tab session), so a
 *  stable app-level token is the surfacing session — the E3 surfaced-item guard's live
 *  auto-close scan is owned by SW-WV-E3 (out of scope here); this records that the window
 *  WAS surfaced so a future scan can only lapse it in this same operator context. Passing a
 *  non-blank token also means the arm actually stamps (a blank session is an E3 refusal the
 *  command swallows as an idempotent no-op). */
const OPERATOR_UI_SESSION = "operator-ui";

function truncate(s: string, max: number): string {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

function relativeTime(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const deltaS = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (deltaS < 60) return `${deltaS}s ago`;
  const m = Math.floor(deltaS / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

/**
 * SW-WV-F5 — the unified shape a YOU-OWE endorsement row renders from. An assertion
 * (E1 `wv_assertions`) and an approval (E2/E3 `wv_approvals`) both normalize into this
 * one shape so the row renderer has a single contract (the shared `EndorsementPill`
 * plus the T2 badge / ack echo where they apply).
 */
export interface EndorsementRowModel {
  /** The pill/settle target. For an assertion this is the assertion id (`wvRatify`);
   *  for an approval this is the approval id (`wvSettleApproval`). */
  id: number;
  disposition: "pending" | "ratified" | "revoked";
  claim: string;
  channel: string | null;
  postedAt: string;
  /** Set for an approval row — routes the pill through `wvSettleApproval`. */
  approvalId?: number;
  tier: "T1" | "T2" | "T3" | null;
  windowState: "open" | "paused" | "expired" | null;
  ttlS: number | null;
  acquiredAt: number | null;
  pauseAccumulatedS: number;
  notSurfacedButExpired: boolean;
  /** Present on assertion rows only. */
  acks?: { seen: number; acknowledged: number; declined: number };
  /** The session an approval is surfaced-into (AC-4). */
  sessionId?: string | null;
}

/** SW-WV-F5: build a row model from an E1 pending assertion (no window / no acks by
 *  default — the F4 assertion DTO carries neither; acks pass separately if resolved). */
export function assertionRowModel(
  a: WvPendingAssertionDto,
  acks?: { seen: number; acknowledged: number; declined: number },
): EndorsementRowModel {
  return {
    id: a.id,
    disposition: (a.disposition as EndorsementRowModel["disposition"]) ?? "pending",
    claim: a.claim,
    channel: a.channel,
    postedAt: a.postedAt,
    approvalId: undefined,
    tier: null,
    windowState: null,
    ttlS: null,
    acquiredAt: null,
    pauseAccumulatedS: 0,
    notSurfacedButExpired: false,
    acks: acks ?? { seen: 0, acknowledged: 0, declined: 0 },
    sessionId: a.sessionId,
  };
}

/** SW-WV-F5: build a row model from a pending T2/T3 approval. */
export function approvalRowModel(a: WvApprovalRow): EndorsementRowModel {
  return {
    id: a.id,
    disposition: a.disposition,
    claim: a.claim,
    channel: a.source,
    postedAt: a.postedAt,
    approvalId: a.id,
    tier: a.tier,
    windowState: a.windowState,
    ttlS: a.ttlS,
    acquiredAt: a.acquiredAt,
    pauseAccumulatedS: a.pauseAccumulatedS,
    notSurfacedButExpired: a.notSurfacedButExpired,
    sessionId: a.surfacedSession,
  };
}

export interface WeftBoardYouOweRowProps {
  row: EndorsementRowModel;
  onRatified: (id: number) => void;
  onRevoked: (id: number) => void;
  /** Optional chevron target — opens the run's endorsement tab / fleet view. */
  onOpen?: (row: EndorsementRowModel) => void;
}

/**
 * SW-WV-F5 (AC-1, AC-3, AC-4, AC-5) — one YOU-OWE endorsement row: the truncated claim,
 * its channel + relative posted-at, the {@link EndorsementPill}, the {@link T2WindowBadge}
 * (T2 only), and the {@link RecipientAckEcho} (assertion rows only). For a T2 approval
 * row it also arms the surfaced-item guard on first viewport entry (AC-4) via an
 * `IntersectionObserver`, exactly once per mount (a local `surfaced` ref); Rust-side
 * idempotency handles cross-mount safety.
 */
export function WeftBoardYouOweRow({
  row,
  onRatified,
  onRevoked,
  onOpen,
}: WeftBoardYouOweRowProps): React.JSX.Element {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const surfacedRef = useRef(false);

  const isT2Approval = row.tier === "T2" && row.approvalId != null;

  useEffect(() => {
    if (!isT2Approval) return;
    const el = rootRef.current;
    if (el == null || typeof IntersectionObserver === "undefined") return;
    // The surfacing session is the row's own if it already carries one (a re-surface),
    // else the operator-UI context (the first surface, when surfaced_session is still
    // NULL) — never blank, so the arm actually stamps.
    const sessionId = row.sessionId ?? OPERATOR_UI_SESSION;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        // First viewport entry only — the local `surfaced` ref blocks a second
        // scroll-into-view on this mount (AC-4); a remount re-arms and Rust no-ops.
        if (entry.isIntersecting && !surfacedRef.current) {
          surfacedRef.current = true;
          void wvMarkApprovalSurfaced({
            id: row.approvalId!,
            sessionId,
          });
        }
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isT2Approval, row.approvalId]);

  return (
    <div
      ref={rootRef}
      data-testid="weft-you-owe-row"
      className="group/pill flex items-center gap-2 px-3 py-1.5 text-sm"
    >
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate" title={row.claim}>
          {truncate(row.claim, CLAIM_MAX)}
        </span>
        <span className="flex items-center gap-2 text-[10px] text-muted-foreground">
          {row.channel && <span className="truncate">{row.channel}</span>}
          <span>{relativeTime(row.postedAt)}</span>
          <T2WindowBadge
            tier={row.tier ?? "T1"}
            windowState={row.windowState}
            ttlS={row.ttlS}
            acquiredAt={row.acquiredAt}
            pauseAccumulatedS={row.pauseAccumulatedS}
            notSurfacedButExpired={row.notSurfacedButExpired}
          />
          {row.acks && <RecipientAckEcho acks={row.acks} />}
        </span>
      </div>
      <EndorsementPill
        id={row.id}
        disposition={row.disposition}
        requiresDialog={row.tier === "T3"}
        approvalId={row.approvalId}
        claim={row.claim}
        onRatified={onRatified}
        onRevoked={onRevoked}
      />
      {onOpen && (
        <button
          type="button"
          data-testid="weft-you-owe-open"
          aria-label="Open the resolving surface"
          onClick={() => onOpen(row)}
          className="shrink-0 rounded p-0.5 text-muted-foreground transition hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          &rsaquo;
        </button>
      )}
    </div>
  );
}

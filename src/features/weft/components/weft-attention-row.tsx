// SPDX-License-Identifier: MIT
import { useState } from "react";

import {
  Bot,
  ChevronRight,
  Navigation,
  RefreshCw,
  Reply,
  ScrollText,
  Send,
  ShieldX,
  ThumbsUp,
  UserPlus,
  X,
} from "lucide-react";

import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import { ParkedQuestionExpansion } from "@/features/viewer/components/parked-question-expansion";
import { InfoTip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { AgentStatusChip } from "@/shared/components/agent-status-chip";
import { useOldestPendingAsk } from "@/shared/hooks/use-oldest-pending-ask";
import { DECISION_VERBS, REACH_IT_VERB, type AgentStatus } from "@/shared/lib/agent-status-config";
import {
  findTabLocationBySessionId,
  selectRunningTerminalTabIndex,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import type { AttentionKind, AttentionRowDto } from "@/tauri/commands";

import { useSessionLiveness } from "../hooks/use-session-liveness";
import { WeftAnswerComposer } from "./weft-answer-composer";

/**
 * SW-WV-F1 (AC-3) — one Weft Board row: a status chip, the row's ~140-char standalone
 * summary, and a routing chevron. SW-AV-4: the resting redesign — the chip is derived
 * from the row's own `kind` (not the lane), the title is a two-line hierarchy (claim +
 * muted provenance) matching `WeftBoardYouOweRow`, and exactly ONE primary verb shows at
 * rest. The secondary controls (→ go, and the demoted dispatch-a-fresh-worker overflow)
 * are hover/focus-revealed only — calm at rest, action on approach. A click still routes
 * via the existing `revealPanelById` seam; a null/unknown `source` stays a no-op.
 */
export interface WeftAttentionRowProps {
  row: AttentionRowDto;
  /** SW-AV-4: the FALLBACK chip tone (from `SECTIONS`) — used only when the row carries
   *  no `kind` (an old-shaped record). When `row.kind` is present the chip derives from
   *  it (per-kind chip, design 4.3), never from the lane. */
  status: AgentStatus;
  /** SW-AV-4: the section's chip-label FALLBACK — used only when the row has no `kind`
   *  (e.g. PARKED's slate `done` tone that must read "Parked", not "Done"). A kind-bearing
   *  row draws its label from the kind vocabulary instead. */
  chipLabel?: string;
  /** SW-WV-F2: when provided, the row offers a dispatch-a-fresh-worker action. SW-AV-4:
   *  demoted to the hover/focus-revealed overflow — never the resting default verb. */
  onDispatch?: (row: AttentionRowDto) => void;
  /** SW-WV-F4 (AC-9): when provided, the row's primary verb reviews the run at altitude
   *  (the review-at-altitude opener). Passed only for the DONE-UNACKED section. */
  onReview?: (row: AttentionRowDto) => void;
  /** SW-WA-E5 (AC-6): when provided AND the row's live handle resolves, the primary verb
   *  becomes "Steer" — a one-click focus of the live worker terminal. Passed only for the
   *  ON-AGENT section; an unresolvable handle falls back to routing (never a dead-end). */
  onOpenTerminal?: (row: AttentionRowDto) => void;
  /** SW-AV-5: drill this row to ITS live agent through the shipped `useAgentDrill`
   *  (focus its tab / adopt a tabless-headless-minimized agent / honest no-op) — the SAME
   *  door the fleet + run monitor use. Passed to EVERY lane; whole-row click and the → go
   *  chevron both fire it whenever the row carries a resolvable `liveSessionId`. A row with
   *  no live agent keeps its route-to-where-it-resolves chevron (never a dead-end). */
  onDrill?: (row: AttentionRowDto) => void;
  /** SW-AV-8: recover from a DEAD / exited target — the "agent no longer running" state's
   *  relaunch / reassign actions. The board wires both to its dispatch surface (re-run the
   *  work / hand it to a different worker). When absent those two buttons are hidden; the
   *  Dismiss action (collapse the state) is always available. */
  onRecover?: (row: AttentionRowDto, action: "relaunch" | "reassign") => void;
  /** SW-AV-21: drive a human DECISION (Approve / Ratify) THROUGH the agent's own runtime
   *  and return the outcome the runtime reports. When it DECLINES the decision
   *  (permission-denied / rejected), the row shows a rejection signal ("Declined by agent"
   *  + reason) instead of silently reading as success. This SHOWS the agent-reported
   *  outcome — CodeSense never enforced the permission and runs no gate here. Passed for
   *  decision rows (approval / assertion); absent → the verb routes as before. */
  onDecision?: (row: AttentionRowDto) => Promise<DecisionOutcome>;
}

/** SW-AV-21: the outcome the agent's runtime reports for a human decision. `ok` — the
 *  runtime accepted it; otherwise it DECLINED (permission-denied / rejected) and `reason`
 *  carries the runtime's explanation when present. This is an OUTCOME the agent reports,
 *  never a CodeSense-side verdict. */
export type DecisionOutcome = { ok: true } | { ok: false; reason?: string | null };

/** SW-AV-4 — the per-kind chip + primary-verb vocabulary (design 4.3). Each attention
 *  kind maps to its OWN chip mark/label (from the shared status-mark vocabulary —
 *  no new color tokens) and its own unmistakable primary verb, so a lane of mixed kinds no
 *  longer shows the same repeated chip and the trailing action always matches the state. */
const KIND_CONFIG: Record<
  AttentionKind,
  { status: AgentStatus; chipLabel: string; verb: string; verbIcon: React.ElementType }
> = {
  // SW-AV-7: every verb is drawn from the ONE shared vocabulary — decision verbs from
  // `DECISION_VERBS`, and the pure "reach the agent" action from `REACH_IT_VERB` (killing
  // the old `parked → "Open"` synonym so the reach-it word is identical everywhere).
  assertion: {
    status: "paused",
    chipLabel: "Assertion",
    verb: DECISION_VERBS.ratify,
    verbIcon: ThumbsUp,
  },
  approval: {
    status: "paused",
    chipLabel: "Approval",
    verb: DECISION_VERBS.approve,
    verbIcon: ThumbsUp,
  },
  question: {
    status: "awaiting-answer",
    chipLabel: "Question",
    verb: DECISION_VERBS.answer,
    verbIcon: Reply,
  },
  blocked: {
    status: "dropped",
    chipLabel: "Blocked",
    verb: DECISION_VERBS.answer,
    verbIcon: Reply,
  },
  parked: { status: "idle", chipLabel: "Parked", verb: REACH_IT_VERB, verbIcon: ChevronRight },
  running: {
    status: "running",
    chipLabel: "Working",
    verb: DECISION_VERBS.steer,
    verbIcon: Navigation,
  },
  done: {
    status: "completed",
    chipLabel: "Done",
    verb: DECISION_VERBS.review,
    verbIcon: ScrollText,
  },
};

/**
 * Route a row to its resolving surface. `source` selects the seam; `ref` is its
 * target. Unknown/null values are a deliberate no-op (future-proof: F2/F4/F5/F6
 * sharpen the targets as they ship).
 */
export function routeAttentionRow(row: AttentionRowDto): void {
  const reveal = useViewerStore.getState().revealPanelById;
  switch (row.source) {
    case "panel":
      // Route to the named right-pane panel (chevron target = the panel id).
      if (row.ref) reveal(row.ref);
      return;
    case "run":
      // SW-AV-26: the Automations panel was retired — open the run's outcome + transcript directly
      // via the pill run-history panel (`openAgentResult`), the same run surface the Activity board
      // routes its DONE rows to. `ref` is the run id.
      if (row.ref) openAgentResult({ run_id: row.ref, story_id: null, defaultView: "findings" });
      return;
    default:
      // Null or unknown source — no-op (never a dead-end click, never a throw).
      return;
  }
}

/** Shared class for a trailing icon-button (chip-height, muted → foreground on hover). */
const ACTION_BTN_CLASS =
  "inline-flex shrink-0 items-center gap-1 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

export function WeftAttentionRow({
  row,
  status,
  chipLabel,
  onDispatch,
  onReview,
  onOpenTerminal,
  onDrill,
  onRecover,
  onDecision,
}: WeftAttentionRowProps): React.JSX.Element {
  const liveId = row.liveSessionId ?? null;
  // SW-WA-E5 (AC-6): the FE resolvability guard — the row's live handle resolves to a
  // terminal tab the panel can actually reach. The Rust read stamps `liveSessionId =
  // dispatch_handle` verbatim (a session id when live, a bare worker name otherwise); a
  // bare name (or a since-closed session) does NOT resolve. SW-AV-9 reuses this SAME
  // signal as the composer's send-enablement gate, so a gone target degrades honestly.
  // A Hidden tab still resolves — its session is running under the tray chip.
  const sessionResolves =
    !!liveId &&
    !!findTabLocationBySessionId(selectRunningTerminalTabIndex(useViewerStore.getState()), liveId);

  // SW-AV-8: the two OTHER reachability signals a bare tab lookup misses, so a row's
  // action never dead-ends into a non-tab session. `pendingAsk` = a LIVE HEADLESS agent is
  // blocking on an `ask_user` (no terminal, but answerable inline via the headless path);
  // `liveness` = the SW-AV-1 union roster + managed-state truth about whether this handle
  // is still running at all. Reconcile: tab-backed → the AV-9 submit_pty_line composer;
  // live-headless-parked → the headless answer surface; dead → the recovery state.
  const pendingAsk = useOldestPendingAsk(liveId ?? "").ask;
  const liveness = useSessionLiveness(liveId);
  const hasHeadlessAsk = !!liveId && !sessionResolves && !!pendingAsk;
  // Dead = a live handle that resolves to NOTHING live: no reachable tab, no pending ask,
  // and a SETTLED roster/managed read that says it isn't alive. Gated on `liveness.loaded`
  // so a pre-load flicker never falsely reads "no longer running".
  const isDead =
    !!liveId && !sessionResolves && !hasHeadlessAsk && liveness.loaded && !liveness.alive;

  const terminalResolves = !!onOpenTerminal && sessionResolves;

  // SW-AV-9: the inline composer's open state. A "waiting on your answer"
  // (question/blocked) row's Answer verb, or a running row's Steer verb, expands it.
  // SW-AV-8: for a live-headless-parked target it expands the headless answer surface
  // instead; for a dead target the same toggle drives the recovery state.
  const [composerOpen, setComposerOpen] = useState(false);
  const [recoveryOpen, setRecoveryOpen] = useState(false);

  // SW-AV-21: a human decision (Approve / Ratify) is driven THROUGH the agent's runtime;
  // when the runtime DECLINES it (permission-denied / rejected) the outcome lands here so
  // the row SHOWS the rejection instead of silently reading as success. `declined` holds
  // the runtime-reported reason (when present). Display of an agent-reported outcome — no
  // CodeSense permission gate runs. A row whose `kind` is a decision AND that carries an
  // `onDecision` driver has its primary verb settle the decision (else it routes, as before).
  const [deciding, setDeciding] = useState(false);
  const [declined, setDeclined] = useState<{ reason: string | null } | null>(null);
  const isDecisionKind = row.kind === "assertion" || row.kind === "approval";
  const runDecision = () => {
    if (!onDecision || deciding) return;
    setDeciding(true);
    setDeclined(null);
    void Promise.resolve(onDecision(row)).then(
      (outcome) => {
        setDeciding(false);
        // A DECLINED runtime reply is the agent's own outcome — surface it, never swallow
        // it into a phantom success. A successful decision leaves the row unchanged (the
        // durable ledger tombstones it).
        if (!outcome.ok) setDeclined({ reason: outcome.reason ?? null });
      },
      (e: unknown) => {
        // A rejected drive (transport failure or a thrown decline) is the same honest
        // negative outcome — shown, not hidden.
        setDeciding(false);
        setDeclined({ reason: e instanceof Error ? e.message : String(e) });
      },
    );
  };

  // SW-AV-8: recover from a DEAD / exited target. NEVER a silent no-op — route Go to the
  // agent's last output / run detail (reuse `openAgentResult`, the SW-OR-12/13 run panel)
  // when a run id resolves, else route to where the row otherwise resolves; and ALWAYS
  // reveal the inline "agent no longer running" recovery state (relaunch / reassign /
  // dismiss). The run id comes from the row itself (`source="run"` → `ref`) or, failing
  // that, the (possibly-retained) roster row's provenance stamp.
  const openDeadRecovery = () => {
    const runId = row.source === "run" && row.ref ? row.ref : liveness.runId;
    if (runId) {
      openAgentResult({
        run_id: runId,
        sessionId: liveId,
        ...(liveness.storyId ? { story_id: liveness.storyId } : {}),
      });
    } else {
      routeAttentionRow(row);
    }
    setRecoveryOpen(true);
  };

  // SW-AV-4: chip from the row's own kind (design 4.3); the lane tone is only a fallback
  // for an old-shaped record that predates `kind`.
  const kindCfg = row.kind ? KIND_CONFIG[row.kind] : undefined;
  const chipStatus = kindCfg?.status ?? status;
  const chipText = kindCfg?.chipLabel ?? chipLabel;

  // SW-AV-4: exactly ONE primary verb at rest, matched to the row's state. "Steer" wins
  // when a live terminal resolves (ON-AGENT); "Review" for a DONE-UNACKED row; otherwise
  // the kind's own verb (Ratify / Approve / Answer / Go) routing to where it resolves.
  // SW-AV-9: the "message the agent" kinds — a "waiting on your answer" row (question /
  // blocked) or a running row. Their primary verb (Answer / Steer) OPENS the inline
  // composer so the human has an explicit place to type, rather than routing/no-op.
  const opensComposer = row.kind === "question" || row.kind === "blocked" || row.kind === "running";

  let primaryVerb: string;
  let PrimaryIcon: React.ElementType;
  let primaryTestId: string;
  let onPrimary: () => void;
  let primaryHint: string;
  if (isDead) {
    // SW-AV-8: the target agent is no longer running. Keep the row's own verb LABEL (a
    // Ratify/Steer/Answer/Go still reads as itself) but re-point it: open the last output
    // and reveal the recovery state — never a swallowed no-op or a throw.
    primaryVerb = kindCfg?.verb ?? REACH_IT_VERB;
    PrimaryIcon = kindCfg?.verbIcon ?? ChevronRight;
    primaryTestId = "weft-row-primary-verb";
    onPrimary = openDeadRecovery;
    primaryHint = "This agent is no longer running — open its last output and recover";
  } else if (terminalResolves) {
    // SW-AV-9: Steer reuses SW-AV-5's drill (focus the live worker) AND opens the
    // composer, so the human lands on the agent and can type in one gesture.
    primaryVerb = DECISION_VERBS.steer;
    PrimaryIcon = Navigation;
    primaryTestId = "weft-row-open-terminal";
    onPrimary = () => {
      onOpenTerminal?.(row);
      setComposerOpen(true);
    };
    primaryHint = "Steer the live worker — open its terminal and message it";
  } else if (onReview) {
    primaryVerb = kindCfg?.verb ?? DECISION_VERBS.review;
    PrimaryIcon = ScrollText;
    primaryTestId = "weft-board-row-review";
    onPrimary = () => onReview(row);
    primaryHint = "Review the results at altitude";
  } else if (opensComposer) {
    // SW-AV-9: Answer (question/blocked) or Steer on a running row whose terminal isn't
    // directly focusable — open the composer in place and land focus on its input. A gone
    // target still opens the composer, which then explains it can't reach the agent.
    primaryVerb = kindCfg?.verb ?? DECISION_VERBS.answer;
    PrimaryIcon = kindCfg?.verbIcon ?? Reply;
    primaryTestId = "weft-row-primary-verb";
    onPrimary = () => setComposerOpen(true);
    primaryHint =
      row.kind === "running"
        ? "Steer the agent — type a message"
        : "Answer the agent — type your reply";
  } else if (onDecision && isDecisionKind) {
    // SW-AV-21: the decision drive verb (Approve / Ratify) settles THROUGH the agent's
    // runtime. A declined outcome flips the row to the rejection signal below; CodeSense
    // shows the outcome and enforces nothing.
    primaryVerb = kindCfg?.verb ?? DECISION_VERBS.approve;
    PrimaryIcon = kindCfg?.verbIcon ?? ThumbsUp;
    primaryTestId = "weft-row-primary-verb";
    onPrimary = runDecision;
    primaryHint =
      row.kind === "assertion"
        ? "Ratify — sent to the agent's runtime"
        : "Approve — sent to the agent's runtime";
  } else {
    // SW-AV-7: the pure "reach the agent / route to where it resolves" fallback uses the
    // ONE shared reach-it verb (was "Open" — a synonym that broke muscle memory).
    primaryVerb = kindCfg?.verb ?? REACH_IT_VERB;
    PrimaryIcon = kindCfg?.verbIcon ?? ChevronRight;
    primaryTestId = "weft-row-primary-verb";
    onPrimary = () => routeAttentionRow(row);
    primaryHint = "Go to where this resolves";
  }

  // SW-AV-5: a row is "drillable" when a drill handler is wired AND it carries a live agent
  // handle. When drillable, whole-row click and the → go chevron both open/focus that agent
  // through the shipped `useAgentDrill` — the SAME door the fleet uses. A row with no live
  // agent keeps its route-to-where-it-resolves behavior (never a dead-end). SW-AV-8: a DEAD
  // target is never drilled (the drill's honest "isn't running" is a dead-end for the
  // operator) — its whole-row click / → go opens the recovery state instead.
  const drillable = !!onDrill && !!liveId && !isDead;

  // The → go chevron is a distinct secondary when the primary verb is NOT already the route
  // (i.e. the primary is Steer/Review) OR when the row is drillable (so a drill-to-agent
  // affordance — the story's keyboard path — is always reachable, even on a lane like YOU-OWE
  // whose primary verb IS its route). When the primary already IS the (non-drill) route, the
  // chevron would just duplicate it, so it is omitted. SW-AV-8: a dead row always surfaces
  // the chevron so the keyboard path reaches the recovery state.
  // SW-AV-21: a decision row's primary settles (Approve/Ratify) rather than routes, so it
  // is no longer "the route" — surface the → go chevron so the resolving surface stays
  // reachable.
  const primaryIsDecision =
    !!onDecision && isDecisionKind && !isDead && !terminalResolves && !onReview;
  const primaryIsRoute = !isDead && !terminalResolves && !onReview && !primaryIsDecision;
  const showGo = isDead || drillable || !primaryIsRoute;
  const onGo = () =>
    isDead ? openDeadRecovery() : drillable ? onDrill?.(row) : routeAttentionRow(row);
  const goHint = isDead
    ? "This agent is no longer running — open its last output and recover"
    : drillable
      ? "Go to the agent behind this row"
      : "Go to where this resolves";

  return (
    <div data-testid="weft-attention-row-shell" className="flex flex-col">
      <div
        className={cn(
          "group flex items-start gap-2 px-3 py-1.5 text-sm",
          (drillable || isDead) && "cursor-pointer",
        )}
        // SW-AV-5: whole-row click drills to the agent (drillable rows only). Inner controls
        // stop propagation so a verb/overflow click never also drills. Keyboard users reach the
        // same drill via the → go chevron button below (a real, focusable control), so no
        // interactive role is grafted onto this container. SW-AV-8: a dead-target row's click
        // opens the recovery state (last output + relaunch/reassign/dismiss) instead.
        onClick={isDead ? openDeadRecovery : drillable ? () => onDrill?.(row) : undefined}
        data-testid="weft-attention-row"
        data-drillable={drillable || undefined}
        data-dead={isDead || undefined}
      >
        <AgentStatusChip status={chipStatus} label={chipText} className="mt-0.5" />
        {/* SW-AV-4: two-line hierarchy (design 4.3) — line 1 is the actionable claim; line 2
          is the muted provenance. SW-WA-E5 (AC-3): the 🤖 marker rides line 2 (an
          agent-owned row wears it; a human row does not). SW-AV-3: initiator is `"agent"`. */}
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="truncate leading-tight" title={row.summary}>
            {row.summary}
          </span>
          <span className="flex items-center gap-1.5 text-[10px] leading-none text-muted-foreground">
            {row.initiator === "agent" ? (
              <InfoTip label="Started by the agent (not you)" side="bottom">
                <span className="inline-flex items-center gap-1">
                  <Bot
                    size={11}
                    aria-label="Started by the agent"
                    data-testid="weft-row-agent-marker"
                    className="shrink-0"
                  />
                  Agent
                </span>
              </InfoTip>
            ) : (
              <span>You</span>
            )}
            {kindCfg && (
              <>
                <span aria-hidden>·</span>
                <span className="capitalize">{row.kind}</span>
              </>
            )}
          </span>
        </div>

        {/* SW-AV-4: the ONE primary verb, visible at rest. */}
        <InfoTip label={primaryHint} side="bottom">
          <button
            type="button"
            data-testid={primaryTestId}
            data-role="primary"
            aria-label={`${primaryVerb} — ${primaryHint}`}
            onClick={(e) => {
              // SW-AV-5: the verb owns the click — don't also fire the whole-row drill.
              e.stopPropagation();
              onPrimary();
            }}
            className={ACTION_BTN_CLASS}
          >
            <PrimaryIcon size={13} aria-hidden />
            <span className="text-xs font-medium">{primaryVerb}</span>
          </button>
        </InfoTip>

        {/* SW-AV-4: the secondaries — the → go chevron and the demoted dispatch overflow —
          are revealed ONLY on hover or keyboard focus (calm at rest). They stay in the DOM
          and in tab order (opacity/pointer-events, never `hidden`), so keyboard focus
          reveals them and reduced-motion users lose nothing but the fade. */}
        {(showGo || onDispatch) && (
          <div
            data-testid="weft-row-secondaries"
            data-role="secondary"
            className="flex shrink-0 items-center gap-1 opacity-0 transition-opacity pointer-events-none focus-within:pointer-events-auto focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100 motion-reduce:transition-none"
          >
            {showGo && (
              <InfoTip label={goHint} side="bottom">
                <button
                  type="button"
                  data-testid="weft-row-go"
                  aria-label={
                    drillable ? "Go to the agent behind this row" : "Go to the resolving surface"
                  }
                  onClick={(e) => {
                    // SW-AV-5: drill to the agent when the row has one; otherwise route.
                    e.stopPropagation();
                    onGo();
                  }}
                  className={ACTION_BTN_CLASS}
                >
                  <ChevronRight size={14} aria-hidden />
                </button>
              </InfoTip>
            )}
            {onDispatch && (
              <InfoTip label="Dispatch a worker to handle this" side="bottom">
                <button
                  type="button"
                  data-testid="weft-row-dispatch"
                  aria-label="Dispatch a worker for this"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDispatch(row);
                  }}
                  className={ACTION_BTN_CLASS}
                >
                  <Send size={13} aria-hidden />
                </button>
              </InfoTip>
            )}
          </div>
        )}
      </div>
      {/* SW-AV-8: a live HEADLESS agent that is blocking on a question has no terminal for
          the submit_pty_line composer to reach — but it CAN be answered inline through the
          shipped headless-answer surface (`ParkedQuestionExpansion` → `ManagedQuestionPanel`
          → `answerAskUser`), the SAME path the fleet's parked rows use. Route the Answer here
          so a headless ask is answerable from Activity, not a disabled dead-end. */}
      {composerOpen && hasHeadlessAsk && liveId ? (
        <ParkedQuestionExpansion sessionId={liveId} onCollapse={() => setComposerOpen(false)} />
      ) : composerOpen ? (
        /* SW-AV-9: the inline Answer / Steer composer — the human's explicit place to type
           the message. Opened by the Answer (question/blocked) or Steer (running) verb; it
           sends through the durable `sendTextToSession(..., { submit: true })` path and
           confirms on landing. `canSend` reuses the row's own reachability signal, so a
           non-tab (but not headless-parked) target opens disabled with an honest
           explanation (never a silent send into the void). */
        <WeftAnswerComposer
          sessionId={liveId}
          canSend={sessionResolves}
          disabledReason={
            sessionResolves
              ? undefined
              : "This agent has no terminal attached here — you'll be able to answer inline the moment it asks a question."
          }
          verb={primaryVerb}
          onClose={() => setComposerOpen(false)}
        />
      ) : null}
      {/* SW-AV-8: the "agent no longer running" recovery state — revealed when an action
          targeted a DEAD / exited session. Its primary already opened the last output; this
          offers the ways forward (relaunch / reassign / dismiss) so the row is never a
          dead-end. */}
      {recoveryOpen && (
        <WeftDeadAgentRecovery
          onRelaunch={onRecover ? () => onRecover(row, "relaunch") : undefined}
          onReassign={onRecover ? () => onRecover(row, "reassign") : undefined}
          onDismiss={() => setRecoveryOpen(false)}
        />
      )}
      {/* SW-AV-21: the agent's runtime declined this decision — SHOW the rejection so an
          Approve/Ratify the runtime rejects never reads as a silent success. */}
      {declined && (
        <WeftDecisionRejection reason={declined.reason} onDismiss={() => setDeclined(null)} />
      )}
    </div>
  );
}

/**
 * SW-AV-8 — the inline "agent no longer running" recovery card, expanded beneath a row
 * whose action targeted a dead / exited / crashed session. The row's primary already
 * routed to the agent's last output; this offers the ways forward so a Go / Ratify / Steer
 * / Answer into a gone agent is never a silent no-op:
 *
 * - **Relaunch** — re-dispatch a fresh worker for the same work (the board's dispatch surface).
 * - **Reassign** — hand the work to a different worker (same dispatch surface, seeded to reassign).
 * - **Dismiss** — collapse the state (the row itself is driven by the durable ledger).
 */
function WeftDeadAgentRecovery({
  onRelaunch,
  onReassign,
  onDismiss,
}: {
  onRelaunch?: () => void;
  onReassign?: () => void;
  onDismiss: () => void;
}): React.JSX.Element {
  return (
    <div
      data-testid="weft-dead-agent-recovery"
      // A recovery click is its own — never bubble into an ancestor row's drill/recovery.
      onClick={(e) => e.stopPropagation()}
      className="ml-1 mt-2 flex flex-col gap-1.5 border-l-2 border-amber-500/40 pl-2 pb-2 text-xs animate-in fade-in-0 slide-in-from-top-1 duration-150 motion-reduce:animate-none"
    >
      <p className="font-medium text-amber-600 dark:text-amber-400">
        This agent is no longer running.
      </p>
      <p className="text-muted-foreground">Its last output is open. Pick up where it left off:</p>
      <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
        {onRelaunch && (
          <button
            type="button"
            data-testid="weft-dead-recover-relaunch"
            onClick={onRelaunch}
            className="inline-flex items-center gap-1 rounded border border-border px-1.5 py-0.5 text-xs font-medium text-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <RefreshCw size={12} aria-hidden />
            Relaunch
          </button>
        )}
        {onReassign && (
          <button
            type="button"
            data-testid="weft-dead-recover-reassign"
            onClick={onReassign}
            className="inline-flex items-center gap-1 rounded border border-border px-1.5 py-0.5 text-xs font-medium text-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <UserPlus size={12} aria-hidden />
            Reassign
          </button>
        )}
        <button
          type="button"
          data-testid="weft-dead-recover-dismiss"
          onClick={onDismiss}
          className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <X size={12} aria-hidden />
          Dismiss
        </button>
      </div>
    </div>
  );
}

/**
 * SW-AV-21 — the "declined by the agent's runtime" outcome, expanded beneath a decision
 * row (Approve / Ratify) whose drive verb the agent's OWN runtime rejected
 * (permission-denied / rejected). The spine: permissions ride the agent's runtime;
 * CodeSense SHOWS the outcome and enforced nothing. So a declined decision reads as an
 * explicit rejection (with the runtime's reason when present) rather than a silent
 * success. Display-only — no gate, no auto-retry; Dismiss collapses the signal.
 */
function WeftDecisionRejection({
  reason,
  onDismiss,
}: {
  reason: string | null;
  onDismiss: () => void;
}): React.JSX.Element {
  return (
    <div
      data-testid="weft-decision-rejection"
      role="alert"
      // The rejection card owns its clicks — never bubble into an ancestor row's drill.
      onClick={(e) => e.stopPropagation()}
      className="ml-1 mt-2 flex flex-col gap-1 border-l-2 border-destructive/50 pl-2 pb-2 text-xs animate-in fade-in-0 slide-in-from-top-1 duration-150 motion-reduce:animate-none"
    >
      <p className="flex items-center gap-1 font-medium text-destructive">
        <ShieldX size={12} aria-hidden />
        Declined by agent
      </p>
      <p className="text-muted-foreground" data-testid="weft-decision-rejection-reason">
        {reason ?? "The agent's runtime rejected this decision."}
      </p>
      <div className="pt-0.5">
        <button
          type="button"
          data-testid="weft-decision-rejection-dismiss"
          onClick={onDismiss}
          className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <X size={12} aria-hidden />
          Dismiss
        </button>
      </div>
    </div>
  );
}

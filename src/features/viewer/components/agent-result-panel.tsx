// SPDX-License-Identifier: MIT
/**
 * A headless or interactive run's OUTCOME + persisted transcript, keyed by `run_id`. Framed as an
 * outcome view (decisive chip + self-attested summary on top), not a raw log dump — the transcript
 * is the fallback proof. Never a blank body or a hung spinner.
 *
 * Live runs get an in-flight tail with an in-place live→final swap, a durable terminal chip
 * and a liveness heartbeat; an agent CLI with structured capture gets the turn-by-turn body.
 *
 * Every caller opens a run through the ONE `openAgentResult(…)` opener, which shows it in the
 * bottom agent area (`AgentRunOutputHost`) — run output belongs with the agents, not in the right
 * pane, which is kept for the user's own surfaces.
 */
import { ArrowDownToLine } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import { AgentStatusChip } from "@/shared/components/agent-status-chip";
import { createPtyWriteBuffer } from "@/shared/terminal/pty-write-buffer";
import { isTerminalManagedState, type ManagedSessionState } from "@/shared/lib/managed-session";
import {
  validatedOutcomeChip,
  type RunOutcome,
  type RunOutcomeRead,
  type ValidatedRunOutcome,
} from "@/shared/lib/run-outcome";
import { formatLastOutput } from "@/shared/lib/format-updated-ago";
import type { SessionTranscript } from "@/shared/lib/session-transcript";
import type { RunWorkProduct } from "@/shared/lib/run-outcome";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import { useRovingTabs } from "@/shared/hooks/use-roving-tabs";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import {
  useAgentResultStore,
  type RunDetailView,
  type TerminalLayoutSnapshot,
} from "@/shared/stores/use-agent-result-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import {
  hasRawTerminalSnapshot,
  readRawTerminalSnapshot,
  readRunArtifact,
  readRunOutcome,
  readRunTranscript,
  readRunWorkProduct,
} from "@/tauri/commands";
import { TranscriptTurns } from "@/features/viewer/components/transcript-turns";
import { RunDetailViews } from "@/features/viewer/components/run-detail-views";
import { toast } from "sonner";

import { getActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** SW-OR-13 (AC-1b): the live-tail render cap, mirroring the Rust `tail_bounded` budget
 *  (`run_artifact.rs`) — keep the LAST ~1 MiB (the end holds the outcome), prefixed with a
 *  truncation marker, so a long stream neither grows unbounded nor thrashes the render path. */
const LIVE_TAIL_MAX = 1024 * 1024;

/** SW-OR-13 (AC-1): the completion-swap retry budget. A self-attesting run writes its durable
 *  outcome just AFTER the event that settles it (and suppresses the later exit event), so the first
 *  post-settle read can miss it. A handful of short retries covers that write-after-emit gap; the
 *  durable write lands within a few ms, so this is generous. */
const OUTCOME_RETRY_MAX = 5;
const OUTCOME_RETRY_MS = 250;

/** Keep only the tail of `text` within {@link LIVE_TAIL_MAX}, prefixed with a one-line marker when
 *  clipped — the FE mirror of Rust `tail_bounded` (chars, not bytes: a render cap, not a byte store). */
function boundTail(text: string): string {
  if (text.length <= LIVE_TAIL_MAX) return text;
  let start = text.length - LIVE_TAIL_MAX;
  // Don't begin the kept tail on a lone low surrogate (a split astral char / emoji) — nudge past it
  // so the truncation seam never renders a replacement glyph.
  const code = text.charCodeAt(start);
  if (code >= 0xdc00 && code <= 0xdfff) start += 1;
  return `…[${start} chars truncated]\n${text.slice(start)}`;
}

/** A short human duration from a millisecond span (`1.4s`, `2m 03s`). Works in whole TENTHS of a
 *  second so the chosen branch always matches what is displayed — neither branch can round up into a
 *  60-slot (`59.9s` then `1m 00s`, never `60.0s`; `2m 00s`, never `1m 60s`). */
function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "";
  const tenths = Math.round(ms / 100);
  if (tenths < 600) return `${(tenths / 10).toFixed(1)}s`;
  const totalSecs = Math.round(tenths / 10);
  const m = Math.floor(totalSecs / 60);
  const s = totalSecs % 60;
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

/** A `Date` from an RFC-3339 string, or `null` when absent/unparseable (never surface "Invalid Date"). */
function parseDate(iso: string | null | undefined): Date | null {
  if (!iso) return null;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d;
}

/** Seat 4: the quiet started / ended / duration line. Each part is omitted when its timestamp is
 *  absent or unparseable — a malformed value degrades to a missing part, never a literal "Invalid
 *  Date". Renders nothing when neither timestamp is present. */
function OutcomeTimestamps({ outcome }: { outcome: RunOutcome }) {
  const started = parseDate(outcome.started_at);
  const ended = parseDate(outcome.ended_at);
  const duration = started && ended ? formatDuration(ended.getTime() - started.getTime()) : null;
  if (!started && !ended) return null;
  return (
    <div
      className="text-[11px] text-muted-foreground flex flex-wrap gap-x-3 gap-y-0.5"
      data-testid="agent-result-timestamps"
    >
      {started && <span>Started {started.toLocaleString()}</span>}
      {ended && <span>Ended {ended.toLocaleString()}</span>}
      {duration && <span>Duration {duration}</span>}
    </div>
  );
}

function OutcomeProvenance({ outcome }: { outcome: RunOutcome }) {
  if (!outcome.workspace && !outcome.trigger_source && !outcome.scheduled_at && !outcome.fired_at)
    return null;
  return (
    <dl
      className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground"
      data-testid="agent-result-provenance"
    >
      {outcome.workspace && (
        <>
          <dt>Workspace</dt>
          <dd className="break-all">{outcome.workspace}</dd>
        </>
      )}
      {outcome.trigger_source && (
        <>
          <dt>Trigger</dt>
          <dd>{outcome.trigger_source}</dd>
        </>
      )}
      {outcome.scheduled_at && (
        <>
          <dt>Scheduled</dt>
          <dd>{outcome.scheduled_at}</dd>
        </>
      )}
      {outcome.fired_at && (
        <>
          <dt>Fired</dt>
          <dd>{outcome.fired_at}</dd>
        </>
      )}
    </dl>
  );
}

function WorkProductSeat({ runId, ready }: { runId: string; ready: boolean }) {
  const [result, setResult] = useState<{
    identity: string;
    value: RunWorkProduct | null;
    failed: boolean;
  } | null>(null);
  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    readRunWorkProduct(runId)
      .then((value) => {
        if (!cancelled) setResult({ identity: runId, value, failed: false });
      })
      .catch(() => {
        if (!cancelled) setResult({ identity: runId, value: null, failed: true });
      });
    return () => {
      cancelled = true;
    };
  }, [ready, runId]);
  const current = result?.identity === runId ? result : null;
  if (!ready) {
    return (
      <span className="text-muted-foreground">Linked change available when the run completes</span>
    );
  }
  if (!current) {
    return <span className="text-muted-foreground">Finding linked change…</span>;
  }
  if (current.failed) {
    return <span className="text-muted-foreground italic">No linked code change</span>;
  }
  if (!current.value)
    return <span className="text-muted-foreground italic">No linked code change</span>;
  return <ResolvedWorkProduct key={runId} value={current.value} />;
}

function ResolvedWorkProduct({ value }: { value: RunWorkProduct }) {
  const [openFailed, setOpenFailed] = useState(false);
  const mountedRef = useRef(true);
  const { copiedValue, handleCopy } = useCopyToClipboard();
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  const { branch, changeRequest } = value;
  const stat = changeRequest?.diffStat;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
      {changeRequest && (
        <button
          type="button"
          className="text-foreground underline underline-offset-2"
          aria-label={`Open ${changeRequest.noun} #${changeRequest.number} in browser`}
          onClick={() => {
            setOpenFailed(false);
            try {
              const url = new URL(changeRequest.url);
              if (url.protocol === "https:") {
                void openUrl(url.toString()).catch(() => {
                  if (mountedRef.current) setOpenFailed(true);
                });
              }
            } catch {
              // A malformed trust-boundary value degrades to a non-action.
            }
          }}
        >
          {changeRequest.noun} #{changeRequest.number} · {changeRequest.state}
        </button>
      )}
      <span className="text-muted-foreground">Working branch</span>
      <button
        type="button"
        className="max-w-full truncate rounded border border-border px-1.5 py-0.5 font-mono text-foreground"
        title={`Copy branch ${branch}`}
        aria-label={`Copy branch ${branch}`}
        onClick={() => handleCopy(branch)}
      >
        {branch}
      </button>
      <span className="sr-only" aria-live="polite">
        {copiedValue === branch ? "Branch copied" : ""}
      </span>
      <span className="text-muted-foreground" aria-live="polite">
        {openFailed ? "Could not open linked change" : ""}
      </span>
      {stat && (
        <span className="text-muted-foreground">
          {stat.changedFiles} files · +{stat.additions} · −{stat.deletions}
        </span>
      )}
    </div>
  );
}

/** SW-OR-13 (AC-1/AC-1b): the bounded transcript body with tail-pinned auto-scroll. New content
 *  sticks to the bottom while the user is following the tail; scrolling up detaches (to read
 *  history) and a "Jump to end" affordance re-pins. ONE element renders both the live tail and the
 *  persisted artifact — swapping the text keeps the same node (and its scroll), so the live→final
 *  swap has no remount / no flicker (AC-1). */
function TailPre({ text }: { text: string }) {
  const ref = useRef<HTMLPreElement>(null);
  const [pinned, setPinned] = useState(true);
  useEffect(() => {
    if (!pinned) return;
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text, pinned]);
  const onScroll = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    // A small slack absorbs sub-pixel rounding so "at the bottom" stays robust.
    setPinned(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
  }, []);
  return (
    <div className="relative min-h-0 flex-1">
      <pre
        ref={ref}
        onScroll={onScroll}
        className="text-xs font-mono whitespace-pre-wrap break-words text-foreground bg-muted/40 rounded p-2 overflow-auto max-h-[60vh] h-full"
        data-testid="agent-result-transcript"
      >
        {text}
      </pre>
      {!pinned && (
        <button
          type="button"
          onClick={() => setPinned(true)}
          className="absolute bottom-2 right-2 flex items-center gap-1 rounded border border-border bg-popover px-2 py-1 text-[11px] text-foreground shadow-sm hover:bg-accent"
          data-testid="agent-result-jump-to-end"
        >
          <ArrowDownToLine className="size-3" />
          Jump to end
        </button>
      )}
    </div>
  );
}

/** Seat 3: the transcript body, or one of the non-transcript first-class states. Rich structured
 *  turns win when present (SW-OR-16); otherwise the TEXT body — the persisted artifact once it lands,
 *  else the in-flight live tail — is the ONE swap OR-13 performs in place (AC-1). */
function AgentResultBody({
  outcome,
  body,
  transcript,
  liveTail,
  loading,
  sessionId,
}: {
  outcome: RunOutcome | null;
  body: string | null;
  transcript: SessionTranscript | null;
  liveTail: string;
  loading: boolean;
  sessionId: string | null;
}) {
  // SW-OR-16 (AC-3): RICH MODE — when the agent CLI has structured capture and the source parsed,
  // render the turn-by-turn body (collapsible tool calls + usage badges). A body rendering mode, not
  // a second viewer; falls through to the text body below when no turns are present.
  if (transcript && transcript.turns.length > 0) {
    return <TranscriptTurns transcript={transcript} />;
  }
  // SW-OR-13 (AC-1): the live/persisted TEXT body. The persisted artifact wins once it lands (a
  // failure's error lives in its kept tail); until then the live tail drives — ONE tail-pinned element
  // so the live→final swap is in place (no remount / no flicker), an on-disk log is never hidden, and
  // an in-flight run is never a blank gap. SW-OR-16 (AC-3): once COMPLETED (outcome present), caption a
  // plain (non-structured) transcript as a MODE — never over the still-streaming live tail.
  const text = body && body.length > 0 ? body : liveTail;
  if (text.length > 0) {
    return (
      <div className="flex flex-col gap-1 min-h-0 flex-1">
        {outcome && (
          <span
            className="text-[11px] italic text-muted-foreground"
            data-testid="agent-result-plain-caption"
          >
            Plain transcript — this agent doesn&apos;t provide structured turns.
          </span>
        )}
        {/* Keyed so the SAME node survives the caption appearing on completion — scroll preserved. */}
        <TailPre key="body" text={text} />
      </div>
    );
  }
  // Completed with no transcript. SW-OR-15 (AC-9): an interactive run writes no `.log` floor, so a
  // declared-but-lost structured capture (crash / torn / locked session log) lands HERE just like a
  // run that genuinely produced nothing — `structured_declared` is the attribution bit that keeps the
  // shortfall off the agent. Both stay the quiet, muted, NON-error styling (a benign shortfall).
  if (outcome) {
    if (outcome.structured_declared) {
      return (
        <div
          className="text-sm text-muted-foreground italic"
          data-testid="agent-result-structured-unavailable"
        >
          Structured transcript unavailable for this run — the outcome and summary above are the
          record.
        </div>
      );
    }
    // A clean side-effect-only run is a success WITH no transcript, styled neutral, never a fault.
    return (
      <div className="text-sm text-muted-foreground italic" data-testid="agent-result-no-output">
        No output captured
      </div>
    );
  }
  // In-flight, before the first chunk streams — an honest degrade, never a gap.
  if (sessionId) {
    return (
      <div className="text-sm text-muted-foreground" data-testid="agent-result-in-progress">
        Waiting for output…
      </div>
    );
  }
  if (loading) {
    return (
      <div className="text-sm text-muted-foreground" data-testid="agent-result-loading">
        Loading…
      </div>
    );
  }
  return (
    <div className="text-sm text-muted-foreground" data-testid="agent-result-no-outcome">
      No outcome recorded for this run yet.
    </div>
  );
}

/** The inner Output-content view switch, in visual order — shares the panel-level switch's roving a11y. */
const RESULT_VIEW_TABS = ["transcript", "raw"] as const;

function ResultViews({
  runId,
  rawAvailable,
  rawPresenceError,
  retryRawPresence,
  primary,
}: {
  runId: string;
  rawAvailable: boolean;
  rawPresenceError: boolean;
  retryRawPresence: () => void;
  primary: ReactNode;
}) {
  const [view, setView] = useState<"transcript" | "raw">("transcript");
  const [raw, setRaw] = useState<string | null>(null);
  const [rawError, setRawError] = useState(false);
  const rawGeneration = useRef(0);
  const rawReadInFlight = useRef(false);
  const id = useId();
  const transcriptTabId = `${id}-transcript-tab`;
  const rawTabId = `${id}-raw-tab`;
  const transcriptPanelId = `${id}-transcript-panel`;
  const rawPanelId = `${id}-raw-panel`;
  const selectRaw = () => {
    setView("raw");
    if (raw !== null || rawReadInFlight.current) return;
    rawReadInFlight.current = true;
    setRawError(false);
    const generation = ++rawGeneration.current;
    void readRawTerminalSnapshot(runId)
      .then((next) => {
        if (next === null) throw new Error("Raw terminal snapshot is no longer available");
        if (rawGeneration.current === generation) setRaw(next);
      })
      .catch(() => {
        if (rawGeneration.current === generation) setRawError(true);
      })
      .finally(() => {
        if (rawGeneration.current === generation) rawReadInFlight.current = false;
      });
  };
  const selectedView = rawAvailable ? view : "transcript";
  const selectView = (next: "transcript" | "raw") => {
    if (next === "raw") selectRaw();
    else setView("transcript");
  };
  const { tabProps } = useRovingTabs(RESULT_VIEW_TABS, selectedView, selectView);
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      {rawAvailable && (
        <div role="tablist" aria-label="Result views" className="flex gap-1">
          <button
            {...tabProps("transcript")}
            id={transcriptTabId}
            type="button"
            aria-controls={transcriptPanelId}
            onClick={() => selectView("transcript")}
            className="rounded px-2 py-1 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            Transcript
          </button>
          <button
            {...tabProps("raw")}
            id={rawTabId}
            type="button"
            aria-controls={rawPanelId}
            onClick={() => selectView("raw")}
            className="rounded px-2 py-1 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            Raw terminal
          </button>
        </div>
      )}
      {!rawAvailable && rawPresenceError && (
        <button
          type="button"
          onClick={retryRawPresence}
          className="w-fit text-xs text-muted-foreground underline underline-offset-2"
        >
          Retry raw terminal availability
        </button>
      )}
      <div
        id={transcriptPanelId}
        role={rawAvailable ? "tabpanel" : undefined}
        aria-labelledby={rawAvailable ? transcriptTabId : undefined}
        hidden={selectedView !== "transcript"}
        className="min-h-0 flex-1"
      >
        {primary}
      </div>
      <div
        id={rawPanelId}
        role={rawAvailable ? "tabpanel" : undefined}
        aria-labelledby={rawAvailable ? rawTabId : undefined}
        hidden={selectedView !== "raw"}
        className="min-h-0 flex-1"
      >
        {rawError ? (
          <button type="button" onClick={selectRaw}>
            Retry raw terminal snapshot
          </button>
        ) : raw === null ? (
          <span className="text-sm text-muted-foreground">Loading raw terminal…</span>
        ) : (
          <TailPre text={raw} />
        )}
      </div>
    </div>
  );
}

/**
 * The panel's presentational core — pure over its inputs (no host / store / command wiring), so the
 * first-class states are unit-testable directly. Lays out all four seats top→bottom: chip + summary
 * header, reserved work-product slot, transcript body, quiet timestamps.
 */
export function AgentResultContent({
  runId,
  outcome,
  validated = null,
  body,
  transcript = null,
  liveTail = "",
  loading,
  sessionId,
  liveSummary,
  liveStatus,
  heartbeat,
  rawAvailable = false,
  rawPresenceError = false,
  retryRawPresence = () => undefined,
}: {
  runId: string | null;
  outcome: RunOutcome | null;
  validated?: ValidatedRunOutcome | null;
  body: string | null;
  transcript?: SessionTranscript | null;
  liveTail?: string;
  loading: boolean;
  sessionId: string | null;
  liveSummary?: string;
  /** The in-flight run's live status, read the way the agents list reads it. */
  liveStatus?: AgentStatus;
  heartbeat?: string | null;
  rawAvailable?: boolean;
  rawPresenceError?: boolean;
  retryRawPresence?: () => void;
}) {
  if (!runId) {
    return (
      <div className="p-3 text-sm text-muted-foreground" data-testid="agent-result-empty">
        Select a run to view its output.
      </div>
    );
  }

  // Header chip: on completion the durable terminal state is PRIMARY (`validatedOutcomeChip`
  // — the SAME composed read AU-55's durable row uses, so they cannot diverge; completed ✓ /
  // failed / dropped). For an in-flight run (sessionId, no outcome yet) the live managed
  // status (`useAgentStatusOf`, the agents list's read) ENRICHES the chip; before it has landed
  // the live row still reads `running`, so a running row never shows a contradictory "No status".
  const chipStatus: AgentStatus | null = validated
    ? validatedOutcomeChip(validated)
    : sessionId
      ? (liveStatus ?? "running")
      : null;
  // Summary: durable outcome summary is primary; the retained live summary is enrichment only.
  const summary = outcome?.summary ?? liveSummary ?? null;

  return (
    <div
      className="flex flex-col flex-1 min-h-0 overflow-auto p-3 gap-3"
      data-testid="agent-result-panel"
    >
      {/* Seat 1: decisive chip + self-attested summary — chip-only when the summary is absent. The
          heartbeat rides alongside the chip while the run is in-flight (working vs wedged). */}
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          {chipStatus ? (
            <AgentStatusChip status={chipStatus} />
          ) : (
            <span className="text-xs text-muted-foreground">No status</span>
          )}
          {heartbeat && (
            <span
              className="text-[11px] text-muted-foreground"
              data-testid="agent-result-heartbeat"
            >
              {heartbeat}
            </span>
          )}
        </div>
        {summary && <p className="text-sm text-foreground break-words">{summary}</p>}
      </div>

      <div className="flex flex-col gap-1 text-xs border-t border-border pt-2 sm:flex-row sm:items-center sm:gap-2">
        <span className="text-muted-foreground">Work product</span>
        <div data-testid="agent-result-work-product">
          <WorkProductSeat runId={runId} ready={outcome !== null} />
        </div>
      </div>

      {/* Seat 3: the bounded transcript body — rich turns when present, else the live/persisted text
          floor (or its first-class non-transcript state). */}
      <ResultViews
        key={runId}
        runId={runId}
        rawAvailable={rawAvailable}
        rawPresenceError={rawPresenceError}
        retryRawPresence={retryRawPresence}
        primary={
          <AgentResultBody
            outcome={outcome}
            body={body}
            transcript={transcript}
            liveTail={liveTail}
            loading={loading}
            sessionId={sessionId}
          />
        }
      />

      {/* Seat 4: durable execution identity and quiet timing. */}
      {outcome && <OutcomeProvenance outcome={outcome} />}
      {outcome && <OutcomeTimestamps outcome={outcome} />}
    </div>
  );
}

/** Read a run's durable outcome (+ its display state), persisted transcript body, and
 *  structured turns by `run_id`. Fetches on open / `run_id` change AND — the OR-13 keystone (AC-1) —
 *  RE-fetches whenever the run's TERMINAL managed state changes (driven by the always-mounted
 *  managed-session store, so a completion that fired before this panel subscribed can't be missed).
 *  A self-attesting run writes its durable
 *  outcome just AFTER the settling event, so an early post-settle read can be null — the bounded retry
 *  covers that gap. All reads run off the UI thread; a stale in-flight fetch is discarded on re-target. */
export function useAgentResult(
  runId: string | null,
  terminalState: ManagedSessionState | null,
): {
  outcome: RunOutcome | null;
  validated: ValidatedRunOutcome | null;
  body: string | null;
  transcript: SessionTranscript | null;
  loading: boolean;
} {
  const identity = runId ?? "";
  // Keep one keyed snapshot so outcome, validation, body, and transcript can never
  // tear or leak across a run switch.
  const [result, setResult] = useState<{
    identity: string;
    read: RunOutcomeRead | null;
    body: string | null;
    transcript: SessionTranscript | null;
  } | null>(null);

  useEffect(() => {
    if (!runId) return;
    let cancelled = false;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let attempts = 0;
    // SW-OR-16: fetch the structured turns alongside the outcome + text body, all off the UI thread.
    // Turns are the rich body when present; the text body stays the single-sourced fallback.
    const load = () => {
      return Promise.all([readRunOutcome(runId), readRunArtifact(runId), readRunTranscript(runId)])
        .then(([nextRead, nextBody, nextTranscript]) => {
          if (cancelled) return;
          setResult({ identity, read: nextRead, body: nextBody, transcript: nextTranscript });
          if (terminalState !== null && !nextRead && attempts < OUTCOME_RETRY_MAX) {
            attempts += 1;
            retry = setTimeout(load, OUTCOME_RETRY_MS);
          }
        })
        .catch(() => {
          if (cancelled) return;
          setResult({ identity, read: null, body: null, transcript: null });
        });
    };
    void load();
    return () => {
      cancelled = true;
      if (retry) clearTimeout(retry);
    };
  }, [identity, runId, terminalState]);

  const current = result?.identity === identity ? result : null;

  return {
    outcome: current?.read?.outcome ?? null,
    validated: current?.read?.validated ?? null,
    body: current?.body ?? null,
    transcript: current?.transcript ?? null,
    loading: Boolean(runId) && current === null,
  };
}

/** Discover the optional sibling after the primary result read has its own paint path. */
export function useRawTerminalPresence(
  runId: string | null,
  outcomeMaterialized = false,
): { available: boolean; error: boolean; retry: () => void } {
  const identity = runId ?? "";
  const [attempt, setAttempt] = useState(0);
  const [presence, setPresence] = useState<{
    identity: string;
    status: "available" | "absent" | "error";
  } | null>(null);
  useEffect(() => {
    if (!runId) return;
    let cancelled = false;
    void hasRawTerminalSnapshot(runId)
      .then((available) => {
        if (!cancelled) setPresence({ identity, status: available ? "available" : "absent" });
      })
      .catch(() => {
        if (!cancelled) setPresence({ identity, status: "error" });
      });
    return () => {
      cancelled = true;
    };
  }, [attempt, identity, outcomeMaterialized, runId]);
  const status = presence?.identity === identity ? presence.status : null;
  return {
    available: status === "available",
    error: status === "error",
    retry: () => setAttempt((current) => current + 1),
  };
}

/** SW-OR-13 (AC-1/AC-1b): subscribe to a run's live output event (`mcp:agent-output-{run_id}`) while
 *  it is in-flight, coalescing chunks per animation frame (reuse `createPtyWriteBuffer`) into a
 *  tail-bounded buffer — so a 1 MiB stream neither thrashes the render path nor grows unbounded.
 *  Returns the live text plus the epoch-ms of the last chunk (the heartbeat source). Yields empty
 *  once the run is terminal (its persisted artifact then drives the body) or with no live session. */
function useLiveTail(
  runId: string | null,
  sessionId: string | null,
  terminal: boolean,
): { tail: string; lastChunkAt: number | null } {
  const identity = !runId || !sessionId || terminal ? null : `${runId}\0${sessionId}`;
  const [live, setLive] = useState<{
    identity: string;
    tail: string;
    lastChunkAt: number;
  } | null>(null);

  useEffect(() => {
    if (!runId || !sessionId || !identity) return;
    let buffer = "";
    const buf = createPtyWriteBuffer((coalesced) => {
      buffer = boundTail(buffer + coalesced);
      setLive({ identity, tail: buffer, lastChunkAt: Date.now() });
    });
    const group = createListenerGroup();
    group.track(
      listen<{ stream: string; chunk: string }>(`mcp:agent-output-${runId}`, (e) => {
        if (e.payload?.chunk) buf.append(e.payload.chunk);
      }),
    );
    return () => {
      group.dispose();
      buf.flush();
    };
  }, [identity, runId, sessionId]);

  const current = live?.identity === identity ? live : null;
  return { tail: current?.tail ?? "", lastChunkAt: current?.lastChunkAt ?? null };
}

/** The run output body — wires the target store + data fetch + live enrichment to the pure content
 *  and the run-detail tabs. Host-agnostic: the host supplies its chrome through `header`, which
 *  receives the durable outcome so the chrome can name the run without a second read. */
export function AgentRunOutputBody({
  teamKey,
  repoPath,
  header,
  visible = true,
}: {
  teamKey: string;
  repoPath: string;
  visible?: boolean;
  header?: (outcome: RunOutcome | null) => ReactNode;
}): React.JSX.Element {
  const runId = useAgentResultStore((s) => s.runId);
  const sessionId = useAgentResultStore((s) => s.sessionId);
  // The story key (Evidence needs `(story_id, run_id)`) and which tab the run opens on.
  const storyId = useAgentResultStore((s) => s.storyId);
  const defaultView = useAgentResultStore((s) => s.defaultView);
  // Live enrichment ONLY when a sessionId is present (in-flight) — BLANK once the run completes.
  const liveSummary = useManagedSessionStore((s) =>
    sessionId ? s.summaries[sessionId] : undefined,
  );
  const liveState = useManagedSessionStore((s) => (sessionId ? s.states[sessionId] : undefined));
  // The run's TERMINAL managed state (or null while in-flight), per the always-mounted
  // managed-session bridge — the race-free completion-refetch trigger (see `useAgentResult`). It is
  // the state VALUE, not a boolean, so a later terminal-state change also refetches.
  const terminalState = liveState != null && isTerminalManagedState(liveState) ? liveState : null;
  const liveRows = useAgentListProjectionStore((s) => s.sessions) ?? [];
  const statusOf = useAgentStatusOf(liveRows);
  const liveRow = liveRows.find((row) => row.sessionId === sessionId);
  const liveStatus = liveRow ? statusOf(liveRow) : undefined;
  const { outcome, validated, body, transcript, loading } = useAgentResult(runId, terminalState);
  const rawPresence = useRawTerminalPresence(runId, outcome !== null);
  // Terminal once the durable outcome has landed — that ends the live stream and swaps the body to
  // the persisted artifact.
  const terminal = outcome !== null;
  const { tail: liveTail, lastChunkAt } = useLiveTail(runId, sessionId, terminal);
  // Tick only while in-flight so the "Ns ago" heartbeat advances; frozen once terminal. Shown only
  // once the first chunk has arrived — before that the body's "Waiting for output…" carries the
  // state, so the heartbeat doesn't duplicate it.
  const streaming = !terminal && sessionId !== null;
  const now = useNowClock(1000, streaming);
  // The shared "last output Ns ago" label — identical cadence to the fleet-row heartbeat (both call
  // `formatLastOutput`), so the view and the row never contradict.
  const heartbeat = streaming && lastChunkAt !== null ? formatLastOutput(lastChunkAt, now) : null;

  // Open a Findings-row file in the viewer, where the workspace-tile diff overlay surfaces the
  // worktree diff — no second diff surface.
  const onOpenDiff = useCallback(
    (filePath: string) => useViewerStore.getState().openFile(teamKey, repoPath, filePath),
    [teamKey, repoPath],
  );

  const output = (
    <AgentResultContent
      runId={runId}
      outcome={outcome}
      validated={validated}
      body={body}
      transcript={transcript}
      liveTail={liveTail}
      loading={loading}
      sessionId={sessionId}
      liveSummary={liveSummary}
      liveStatus={liveStatus}
      heartbeat={heartbeat}
      rawAvailable={rawPresence.available}
      rawPresenceError={rawPresence.error}
      retryRawPresence={rawPresence.retry}
    />
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {header?.(outcome)}
      {runId ? (
        <RunDetailViews
          // Key by run AND requested view so a directed re-open (e.g. opening the same run on
          // Evidence) remounts and applies `defaultView`; a re-open on the same view keeps the
          // user's current tab.
          key={`${runId}:${defaultView}`}
          runId={runId}
          storyId={storyId}
          defaultView={defaultView}
          output={output}
          onOpenDiff={onOpenDiff}
          teamKey={teamKey}
          visible={visible}
        />
      ) : (
        output
      )}
    </div>
  );
}

/** The live watch on the revealed terminal column, while a restore is pending. */
let stopWatchingLayout: (() => void) | null = null;

function stopLayoutWatch(): void {
  stopWatchingLayout?.();
  stopWatchingLayout = null;
}

function terminalEntries(): TerminalLayoutSnapshot {
  const viewer = useViewerStore.getState();
  return { mode: viewer.layoutMode, visible: viewer.terminalVisible };
}

/** Put the terminal column back exactly as it was before the output revealed it. */
function restoreTerminalLayout(prior: TerminalLayoutSnapshot): void {
  stopLayoutWatch();
  useViewerStore.setState({ layoutMode: prior.mode, terminalVisible: prior.visible });
  useViewerStore.getState().persistState();
}

/** Any change to the terminal column after the reveal is the user's, so it forfeits the
 *  restore: closing the output must never undo a layout the user chose. */
function watchRevealedLayout(): void {
  stopLayoutWatch();
  const revealed = terminalEntries();
  stopWatchingLayout = useViewerStore.subscribe((s) => {
    if (s.layoutMode === revealed.mode && s.terminalVisible === revealed.visible) return;
    stopLayoutWatch();
    useAgentResultStore.getState().forfeitRestore();
  });
}

/**
 * Open a run's output in the bottom agent area of the active team. `run_id` is the durable key
 * (always present); `sessionId` is optional live enrichment while the run is in-flight. `story_id`
 * threads the Evidence key (`read_run_evidence` refuses `run_id` alone), and `defaultView` lets a
 * caller open directly on another tab. The ONE opener every lens calls, so no lens ever shows a
 * disconnected duplicate.
 */
export function openAgentResult(target: {
  run_id: string;
  sessionId?: string | null;
  story_id?: string | null;
  defaultView?: RunDetailView;
  preserveFocus?: boolean;
}): void {
  const teamKey = getActiveTeamKey();
  // With no team there is no agent area to show the output in.
  if (!teamKeyHasTeam(teamKey)) {
    toast("Open a team to view this run's output.");
    return;
  }
  const current = useAgentResultStore.getState();
  const sameTeam = current.runId !== null && current.teamKey === teamKey;
  const viewer = useViewerStore.getState();
  const hidden = viewer.layoutMode === "viewer" || !viewer.terminalVisible;
  // The column is one for every team (D39), so the output keeps the first open's pending restore
  // when it switches runs or follows the active team: closing returns the column to how it was
  // before any output opened.
  const pending = current.teamKey ? current.restoreTerminal : null;
  const restore = sameTeam || pending ? pending : hidden ? terminalEntries() : null;
  current.setTarget(
    target.run_id,
    target.sessionId ?? null,
    target.story_id ?? null,
    target.defaultView,
    teamKey,
    restore,
    target.preserveFocus,
  );
  // Show the terminal column without rewriting the user's saved terminal mode: the output takes the
  // place of the CLI tiles there.
  viewer.revealTerminalInPlace();
  if (restore && !sameTeam) watchRevealedLayout();
}

/** Close the open run output and hand the agent area back as it was: a terminal column the output
 *  had to reveal is hidden again, unless the user has changed the column since. */
export function closeAgentResult(): void {
  const { teamKey, restoreTerminal, close } = useAgentResultStore.getState();
  close();
  if (teamKey && restoreTerminal) restoreTerminalLayout(restoreTerminal);
  else stopLayoutWatch();
}

/** Close the run output and keep the terminal column as it is now — for a jump into a terminal. */
export function dismissAgentResult(): void {
  stopLayoutWatch();
  useAgentResultStore.getState().close();
}

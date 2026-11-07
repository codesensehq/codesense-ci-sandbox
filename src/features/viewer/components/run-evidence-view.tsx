// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";

import type { SessionTranscript } from "@/shared/lib/session-transcript";
import type {
  ArtifactKind,
  Attempt,
  FootprintChange,
  FootprintEntry,
  PerNodeResult,
  RunAuditHeader,
  RunEvidence,
  StepArtifact,
  StepExecution,
  StepOutput,
  TriggerFact,
} from "@/shared/lib/run-evidence";
import {
  hasRedactionMarker,
  hasReportedResult,
  isExternalUrl,
  isResolvableReference,
  joinStepsWithResults,
  TRUNCATION_MARKER,
} from "@/shared/lib/run-evidence-results";
import { readRunArtifact, readRunTranscript } from "@/tauri/commands";
import { TranscriptTurns } from "@/features/viewer/components/transcript-turns";
import type { RunEvidenceState } from "@/features/viewer/hooks/use-run-evidence";

/**
 * SW-AU-82A — the RunEvidence view (AC-8/9/10/11 FE). Renders one run's immutable audit header, its
 * per-node `StepExecution`s, and per-`Attempt` evidence, deep-linked by stable id
 * (`run`/`node`/`step_execution`/`attempt`/`event`). Every fetch state renders honestly; per-evidence
 * completeness is shown (never a silent blank); redaction is visible as a data property; a run whose
 * trigger event was never captured renders an explicit pre-envelope step, never a fabricated one.
 */
export function RunEvidenceView({
  state,
  onReload,
}: {
  state: RunEvidenceState;
  onReload?: () => void;
}): React.JSX.Element {
  switch (state.kind) {
    case "loading":
      return <StatusPanel testid="run-evidence-loading" title="Loading run evidence…" busy />;
    case "error":
      return (
        <StatusPanel
          testid="run-evidence-error"
          title="Evidence source unreadable"
          detail={state.message}
          onReload={onReload}
        />
      );
    case "unavailable":
      return (
        <StatusPanel
          testid="run-evidence-unavailable"
          title="No evidence for this run"
          detail="This run left no recorded outcome yet, or is not in the current store."
        />
      );
    case "permissionGated":
      return (
        <StatusPanel
          testid="run-evidence-gated"
          title="Evidence unavailable here"
          detail={state.reason}
        />
      );
    case "ready":
      return <ReadyEvidence evidence={state.evidence} stale={state.stale} onReload={onReload} />;
  }
}

function StatusPanel({
  testid,
  title,
  detail,
  busy,
  onReload,
}: {
  testid: string;
  title: string;
  detail?: string;
  busy?: boolean;
  onReload?: () => void;
}): React.JSX.Element {
  return (
    <div
      role="status"
      aria-busy={busy ? true : undefined}
      data-testid={testid}
      className="flex flex-col gap-1 p-3 text-sm text-muted-foreground"
    >
      <span className="font-medium text-foreground">{title}</span>
      {detail && <span className="text-xs">{detail}</span>}
      {onReload && (
        <button
          type="button"
          onClick={onReload}
          className="mt-1 self-start rounded border px-2 py-0.5 text-xs hover:bg-accent"
        >
          Retry
        </button>
      )}
    </div>
  );
}

function ReadyEvidence({
  evidence,
  stale,
  onReload,
}: {
  evidence: RunEvidence;
  stale: boolean;
  onReload?: () => void;
}): React.JSX.Element {
  const { header, run, authority, perNodeResults } = evidence;
  const rows = joinStepsWithResults(run.steps, perNodeResults);
  return (
    <section
      role="region"
      aria-label={`Evidence for run ${header.runId}`}
      data-testid="run-evidence"
      data-run-id={run.runId}
      data-authority={authority}
      data-event={run.correlationId !== run.runId ? run.correlationId : undefined}
      className="flex flex-col gap-3 p-3"
    >
      {stale && (
        <div
          role="status"
          data-testid="run-evidence-stale"
          className="rounded bg-muted/60 px-2 py-1 text-xs italic text-muted-foreground"
        >
          Showing last known evidence · refreshing…
          {onReload && (
            <button type="button" onClick={onReload} className="ml-2 underline">
              Refresh now
            </button>
          )}
        </div>
      )}
      {authority === "currentStoreFallback" && (
        <div data-testid="run-evidence-authority-cue" className="text-[11px] text-muted-foreground">
          durable authority: unavailable — showing current-store record
        </div>
      )}
      <AuditHeaderView header={header} />
      {run.parentRunId && <StartedByLane parentRunId={run.parentRunId} />}
      <ul className="flex flex-col gap-2" data-testid="run-evidence-steps">
        {rows.length === 0 && (
          <li className="text-xs text-muted-foreground" data-testid="run-evidence-no-steps">
            No step executions recorded for this run.
          </li>
        )}
        {rows.map((row) => (
          <StepExecutionRow
            key={row.step?.stepExecutionId ?? `orphan:${row.result?.nodeId}`}
            step={row.step}
            result={row.result}
          />
        ))}
      </ul>
    </section>
  );
}

/** The inspectable trigger step (AC-11) — honest: an unattributed/pre-envelope run is labelled as such. */
function TriggerStep({ trigger }: { trigger: TriggerFact }): React.JSX.Element {
  if (trigger.kind === "attributed") {
    return (
      <div data-testid="trigger-attributed" className="text-xs">
        <span className="text-muted-foreground">Triggered by</span>{" "}
        <span className="font-mono">{trigger.source}</span>
      </div>
    );
  }
  return (
    <div data-testid="trigger-pre-envelope" className="text-xs text-muted-foreground">
      Trigger not captured — pre-envelope run (no normalized event recorded)
    </div>
  );
}

function AuditHeaderView({ header }: { header: RunAuditHeader }): React.JSX.Element {
  return (
    <header
      data-testid="run-evidence-header"
      className="flex flex-col gap-1 rounded border bg-muted/30 p-2"
    >
      <div className="flex items-center gap-2">
        <span
          data-testid="run-evidence-status"
          className="rounded bg-muted px-1.5 py-0.5 text-[11px] font-medium uppercase tracking-wide"
        >
          {/* A live durable Run has no terminal status yet — render it honestly (AC-6/AC-12). */}
          {header.status ?? "in progress"}
        </span>
        <span className="truncate text-sm font-medium" title={header.runId}>
          {header.storyId}
        </span>
      </div>
      <TriggerStep trigger={header.trigger} />
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
        {header.automationId && (
          <>
            <dt>Automation</dt>
            <dd className="font-mono">{header.automationId}</dd>
          </>
        )}
        {header.firedAt && (
          <>
            <dt>Fired</dt>
            <dd>{header.firedAt}</dd>
          </>
        )}
        {header.endedAt && (
          <>
            <dt>Ended</dt>
            <dd>{header.endedAt}</dd>
          </>
        )}
      </dl>
      {header.summary && (
        <p data-testid="run-evidence-summary" className="text-xs">
          {header.summary}
          {hasRedactionMarker(header.summary) && <RedactedBadge scope="summary" />}
        </p>
      )}
    </header>
  );
}

/** Redaction is a data property — make it visible, never a silent deletion (AC-6/AC-8). `scope`
 *  keeps the test id unique when both the summary and an attempt render a badge. */
function RedactedBadge({ scope }: { scope: string }): React.JSX.Element {
  return (
    <span
      data-testid={`redacted-badge-${scope}`}
      title="Secrets were scrubbed from this evidence"
      className="ml-1 rounded bg-amber-500/15 px-1 text-[10px] font-medium text-amber-700 dark:text-amber-400"
    >
      contains redacted content
    </span>
  );
}

/** The "Started by" causation lane (SW-AU-82B, AC-14). This is causation, NOT assignment
 *  (ARCH-INV-20): it renders under its own labeled heading, distinct from any subject/assignment
 *  region, and links the parent Run by stable `parent_run_id` with a full accessible name (visible
 *  affordance, not hover-only). Deep-link navigation resolution is owned by SW-AU-90. */
function StartedByLane({ parentRunId }: { parentRunId: string }): React.JSX.Element {
  return (
    <section
      data-testid="run-evidence-started-by"
      aria-labelledby="run-evidence-started-by-heading"
      className="flex flex-col gap-0.5"
    >
      <h3
        id="run-evidence-started-by-heading"
        className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground"
      >
        Started by
      </h3>
      <a
        data-testid="started-by-link"
        data-parent-run-id={parentRunId}
        href={`#run/${parentRunId}`}
        aria-label={`Parent Automation Run ${parentRunId}`}
        title={parentRunId}
        className="truncate font-mono text-xs underline"
      >
        {parentRunId}
      </a>
    </section>
  );
}

/**
 * One row of a run's result surface: a node's reported result (SW-AU-97) plus its attempt evidence.
 * `step` is absent for an orphan reported result whose `nodeId` joined no
 * step (never dropped); `result` is absent for a step that reported no structured result. When the node
 * reported any structured result, the raw transcript moves one click down into a lazily-mounted
 * disclosure (AC-2/AC-8c); with nothing reported, the transcript stays inline (it is the evidence).
 */
export function StepExecutionRow({
  step,
  result,
}: {
  step?: StepExecution;
  result?: PerNodeResult;
}): React.JSX.Element {
  const attempts = step?.attempts ?? [];
  // Default to the final (latest) attempt; the older attempts are superseded but retained (AC-3).
  const [selected, setSelected] = useState(Math.max(0, attempts.length - 1));
  // Clamp defensively: if this instance is reused for a step with fewer attempts, a stale index must
  // not resolve to `undefined` (the graph inspector also keys the row by node to force a remount).
  const active = attempts.length > 0 ? Math.min(selected, attempts.length - 1) : 0;
  const attempt = attempts[active];
  const nodeId = step?.nodeId ?? result?.nodeId ?? "unknown";
  const reported = hasReportedResult(result);
  return (
    <li
      data-testid="step-execution"
      data-node-id={nodeId}
      data-step-execution-id={step?.stepExecutionId}
      data-orphan-result={step ? undefined : "true"}
      className="rounded border p-2"
    >
      <div className="flex items-center gap-2">
        <span className="font-mono text-xs font-medium">{nodeId}</span>
        {attempts.length > 1 && (
          <AttemptSelector count={attempts.length} selected={active} onSelect={setSelected} />
        )}
      </div>
      <PerNodeResultView result={result} />
      {step &&
        (attempt ? (
          reported ? (
            <TranscriptDisclosure attempt={attempt} superseded={active < attempts.length - 1} />
          ) : (
            <AttemptEvidence attempt={attempt} superseded={active < attempts.length - 1} />
          )
        ) : (
          <span
            data-testid="step-no-attempts"
            className="mt-1 block text-[11px] text-muted-foreground"
          >
            No attempt evidence recorded.
          </span>
        ))}
    </li>
  );
}

/** The raw transcript, one click down (AC-2): a collapsed disclosure that mounts — and therefore
 *  fetches — `AttemptEvidence` only once opened, so the summary-first view costs no transcript I/O
 *  on render or node-select (AC-8c). */
function TranscriptDisclosure({
  attempt,
  superseded,
}: {
  attempt: Attempt;
  superseded: boolean;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="mt-1 rounded border border-border/60"
      data-testid="transcript-disclosure"
      onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}
    >
      <summary className="cursor-pointer select-none px-2 py-0.5 text-[11px] text-muted-foreground">
        Raw transcript
      </summary>
      <div className="p-1">
        {open && <AttemptEvidence attempt={attempt} superseded={superseded} />}
      </div>
    </details>
  );
}

/** Cap on rendered rows per result channel (outputs / artifacts / footprint) — bounds the DOM even when
 *  a step reports an extreme number of entries (AC-8a). Distinct from the transcript's turn cap. */
const MAX_RENDERED_RESULT_ROWS = 50;

/** Above this many characters a typed output value renders behind a collapsed disclosure (AC-8b). */
const LARGE_VALUE_CHARS = 200;

/**
 * The per-node reported result (SW-AU-97 AC-1/2/3/4): summary first (the default view), then typed
 * outputs, produced artifacts, and touched footprint. Each list channel is bounded and each large body
 * is a collapsed disclosure, so an unopened result costs no DOM and no I/O. A node that reported nothing
 * renders one honest line — never a blank scaffold (AC-1/AC-7).
 */
function PerNodeResultView({ result }: { result?: PerNodeResult }): React.JSX.Element {
  if (!hasReportedResult(result)) {
    return (
      <p data-testid="node-result-none" className="mt-1 text-[11px] text-muted-foreground">
        No structured results reported for this step.
      </p>
    );
  }
  // hasReportedResult guarantees a defined result with at least one channel.
  const reported = result as PerNodeResult;
  return (
    <div data-testid="node-result" className="mt-1 flex flex-col gap-2">
      {reported.summary?.trim() && <ResultSummary summary={reported.summary} />}
      {reported.outputs && reported.outputs.length > 0 && (
        <ResultOutputs outputs={reported.outputs} />
      )}
      {reported.artifacts && reported.artifacts.length > 0 && (
        <ResultArtifacts artifacts={reported.artifacts} />
      )}
      {reported.footprint && reported.footprint.length > 0 && (
        <ResultFootprint footprint={reported.footprint} />
      )}
    </div>
  );
}

/** A small uppercase channel heading, shared by every result channel. */
function ChannelLabel({ children }: { children: React.ReactNode }): React.JSX.Element {
  return (
    <div className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
      {children}
    </div>
  );
}

/** Window a channel list to the shared ceiling; the caller surfaces the hidden count (AC-8a). */
function windowRows<T>(items: T[]): { shown: T[]; hidden: number } {
  const hidden = Math.max(0, items.length - MAX_RENDERED_RESULT_ROWS);
  return { shown: hidden > 0 ? items.slice(0, MAX_RENDERED_RESULT_ROWS) : items, hidden };
}

/** The visible "N more…" notice — never a silent truncation (AC-8a). Pluralizes `entry → entries`. */
function MoreNotice({ hidden, noun }: { hidden: number; noun: string }): React.JSX.Element | null {
  if (hidden <= 0) return null;
  const plural = noun.endsWith("y") ? `${noun.slice(0, -1)}ies` : `${noun}s`;
  return (
    <div data-testid="result-more-notice" className="text-[11px] italic text-muted-foreground">
      {hidden} more {hidden === 1 ? noun : plural} not shown.
    </div>
  );
}

function ResultSummary({ summary }: { summary: string }): React.JSX.Element {
  return (
    <div data-testid="node-result-summary" className="text-xs">
      <ChannelLabel>Summary</ChannelLabel>
      <p className="whitespace-pre-wrap break-words">
        {summary}
        {hasRedactionMarker(summary) && <RedactedBadge scope="result-summary" />}
      </p>
    </div>
  );
}

function stringifyValue(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** A large value body behind a collapsed native `<details>` disclosure, opened on demand (AC-8b) —
 *  the same collapse + bounded-scroll idiom the transcript tool-calls use. */
function JsonDisclosure({ value, testid }: { value: unknown; testid: string }): React.JSX.Element {
  return (
    <details className="rounded border border-border bg-muted/30" data-testid={testid}>
      <summary className="cursor-pointer select-none px-2 py-0.5 text-[11px] text-muted-foreground">
        Show value
      </summary>
      <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded bg-background/60 p-1.5 font-mono text-[11px]">
        {stringifyValue(value)}
      </pre>
    </details>
  );
}

function TruncatedNote(): React.JSX.Element {
  return (
    <span data-testid="result-truncated" className="ml-1 text-[10px] italic text-muted-foreground">
      truncated
    </span>
  );
}

/** One typed output's value: inline for a small body, a collapsed disclosure for a large one (AC-8b).
 *  Redaction is surfaced once at the channel level (see `ResultOutputs`) so a windowed-out or JSON-mode
 *  value is covered too; this only surfaces the per-value truncation marker. */
function OutputValue({ value }: { value: unknown }): React.JSX.Element {
  const text = stringifyValue(value);
  if (text.length > LARGE_VALUE_CHARS) {
    return <JsonDisclosure value={value} testid="output-value-json" />;
  }
  return (
    <span className="whitespace-pre-wrap break-words font-mono">
      {text}
      {text.includes(TRUNCATION_MARKER) && <TruncatedNote />}
    </span>
  );
}

/** Whether any output value carries the redaction sentinel — computed over ALL outputs, not just the
 *  windowed slice, so AC-5's "say so" holds even for a redacted value that scrolled past the row cap. */
function outputsHaveRedaction(outputs: StepOutput[]): boolean {
  return outputs.some((o) => hasRedactionMarker(stringifyValue(o.value)));
}

/** The JSON view of the shown outputs — the raw `StepOutput[]` in one bounded-scroll body (matches the
 *  transcript text idiom). Rendered as the array (not folded to an object) so duplicate keys are never
 *  silently dropped from what the table shows. */
function JsonOutputsView({ outputs }: { outputs: StepOutput[] }): React.JSX.Element {
  return (
    <pre
      data-testid="outputs-json"
      className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded bg-muted/40 p-2 font-mono text-[11px]"
    >
      {stringifyValue(outputs)}
    </pre>
  );
}

/** Typed outputs as a Table/JSON toggle (AC-4), windowed to the shared ceiling (AC-8a). */
function ResultOutputs({ outputs }: { outputs: StepOutput[] }): React.JSX.Element {
  const [mode, setMode] = useState<"table" | "json">("table");
  const { shown, hidden } = windowRows(outputs);
  return (
    <section data-testid="node-result-outputs" className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <ChannelLabel>Outputs</ChannelLabel>
        {outputsHaveRedaction(outputs) && <RedactedBadge scope="outputs" />}
        <span
          role="group"
          aria-label="Output view"
          className="ml-auto flex items-center gap-1 text-[10px]"
        >
          <button
            type="button"
            aria-pressed={mode === "table"}
            onClick={() => setMode("table")}
            className={`rounded border px-1 ${mode === "table" ? "bg-accent" : ""}`}
          >
            Table
          </button>
          <button
            type="button"
            aria-pressed={mode === "json"}
            onClick={() => setMode("json")}
            className={`rounded border px-1 ${mode === "json" ? "bg-accent" : ""}`}
          >
            JSON
          </button>
        </span>
      </div>
      {mode === "table" ? (
        <table className="w-full text-[11px]" data-testid="outputs-table">
          <tbody>
            {shown.map((output, i) => (
              <tr key={i} className="border-t border-border/60 align-top">
                <td className="py-0.5 pr-2 font-mono font-medium">{output.key}</td>
                <td className="py-0.5">
                  <OutputValue value={output.value} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <JsonOutputsView outputs={shown} />
      )}
      <MoreNotice hidden={hidden} noun="output" />
    </section>
  );
}

/**
 * The kind-driven affordance shared by artifacts and footprint (AC-3/AC-4): a resolvable `pr`/`entity`
 * reference links; a `file` path and a non-resolvable `entity` (a work-product branch) render as a
 * labeled, copyable reference with NO I/O. A `file` always shows its repo-relative path.
 */
function ReferenceAffordance({
  kind,
  reference,
  label,
}: {
  kind: ArtifactKind;
  reference: string;
  label?: string;
}): React.JSX.Element {
  const text = kind === "file" ? reference : (label ?? reference);
  if (kind !== "file" && isResolvableReference(reference)) {
    const external = isExternalUrl(reference);
    return (
      <a
        data-testid="reference-link"
        data-kind={kind}
        href={reference}
        title={reference}
        className="break-all font-mono underline"
        {...(external ? { target: "_blank", rel: "noreferrer noopener" } : {})}
      >
        {text}
      </a>
    );
  }
  return (
    <span
      data-testid="reference-label"
      data-kind={kind}
      title={reference}
      className="break-all font-mono text-muted-foreground"
    >
      {text}
      {kind === "file" && (
        <span className="ml-1 text-[10px] uppercase text-muted-foreground/70">file</span>
      )}
    </span>
  );
}

/** Produced artifacts as a named, kind-driven list (AC-3), windowed to the shared ceiling (AC-8a). */
function ResultArtifacts({ artifacts }: { artifacts: StepArtifact[] }): React.JSX.Element {
  const { shown, hidden } = windowRows(artifacts);
  return (
    <section data-testid="node-result-artifacts" className="flex flex-col gap-1">
      <ChannelLabel>Artifacts</ChannelLabel>
      <ul className="flex flex-col gap-0.5">
        {shown.map((artifact, i) => (
          <li key={i} data-testid="artifact-row" data-kind={artifact.kind} className="text-[11px]">
            <ReferenceAffordance
              kind={artifact.kind}
              reference={artifact.reference}
              label={artifact.label}
            />
          </li>
        ))}
      </ul>
      <MoreNotice hidden={hidden} noun="artifact" />
    </section>
  );
}

function ChangeBadge({ change }: { change: FootprintChange }): React.JSX.Element {
  return (
    <span
      data-testid="footprint-change"
      className="rounded bg-muted px-1 text-[9px] uppercase text-muted-foreground"
    >
      {change}
    </span>
  );
}

/** Touched footprint as a kind-driven list (AC-4): `file` → repo-relative path, `pr`/`entity` →
 *  deep-link when resolvable. Windowed to the shared ceiling (AC-8a). */
function ResultFootprint({ footprint }: { footprint: FootprintEntry[] }): React.JSX.Element {
  const { shown, hidden } = windowRows(footprint);
  return (
    <section data-testid="node-result-footprint" className="flex flex-col gap-1">
      <ChannelLabel>Footprint</ChannelLabel>
      <ul className="flex flex-col gap-0.5">
        {shown.map((entry, i) => (
          <li
            key={i}
            data-testid="footprint-row"
            data-kind={entry.kind}
            data-change={entry.change}
            className="flex items-center gap-1.5 text-[11px]"
          >
            <ChangeBadge change={entry.change} />
            <ReferenceAffordance kind={entry.kind} reference={entry.reference} />
          </li>
        ))}
      </ul>
      <MoreNotice hidden={hidden} noun="entry" />
    </section>
  );
}

function AttemptSelector({
  count,
  selected,
  onSelect,
}: {
  count: number;
  selected: number;
  onSelect: (n: number) => void;
}): React.JSX.Element {
  return (
    <span
      role="group"
      aria-label="Select attempt"
      data-testid="attempt-selector"
      className="ml-auto flex items-center gap-1 text-[11px]"
    >
      <button
        type="button"
        aria-label="Previous attempt"
        disabled={selected === 0}
        onClick={() => onSelect(selected - 1)}
        className="rounded border px-1 disabled:opacity-40"
      >
        ‹
      </button>
      <span aria-live="polite" data-testid="attempt-position">
        Attempt {selected + 1} of {count}
      </span>
      <button
        type="button"
        aria-label="Next attempt"
        disabled={selected === count - 1}
        onClick={() => onSelect(selected + 1)}
        className="rounded border px-1 disabled:opacity-40"
      >
        ›
      </button>
    </span>
  );
}

/** Per-attempt evidence body + honest completeness state (AC-8), fetched by the attempt's compat
 *  `run_id` through the EXISTING per-run readers (one evidence-read path, DRY). */
function AttemptEvidence({
  attempt,
  superseded,
}: {
  attempt: Attempt;
  superseded: boolean;
}): React.JSX.Element {
  const body = useAttemptBody(attempt.runId);
  const redacted = containsRedaction(body);
  return (
    <div
      data-testid="attempt-evidence"
      data-attempt-id={attempt.attemptId}
      className="mt-1 flex flex-col gap-1"
    >
      <div className="flex items-center gap-2 text-[11px] text-muted-foreground">
        {/* aria-live so a bounded-truncated / redacted completeness is announced to AT (AC-8/AC-10). */}
        <span data-testid="attempt-completeness" role="status" aria-live="polite">
          {completenessLabel(body)}
          {redacted ? " · redacted" : ""}
        </span>
        {superseded && (
          <span
            data-testid="attempt-superseded"
            className="rounded bg-muted px-1 text-[10px] uppercase"
          >
            superseded
          </span>
        )}
        {redacted && <RedactedBadge scope="attempt" />}
      </div>
      {body.kind === "loading" && (
        <span className="text-[11px] text-muted-foreground">Loading…</span>
      )}
      {body.kind === "turns" && <TranscriptTurns transcript={body.transcript} />}
      {body.kind === "text" && (
        <pre className="max-h-64 overflow-auto rounded bg-muted/40 p-2 text-[11px] whitespace-pre-wrap">
          {body.text}
        </pre>
      )}
    </div>
  );
}

type AttemptBody =
  | { kind: "loading" }
  | { kind: "turns"; transcript: SessionTranscript }
  | { kind: "text"; text: string }
  | { kind: "notCaptured" };

function useAttemptBody(runId: string): AttemptBody {
  const [body, setBody] = useState<AttemptBody>({ kind: "loading" });
  const generation = useRef(0);
  useEffect(() => {
    const request = ++generation.current;
    // Defer the loading reset off the effect body (no synchronous cascading setState); the
    // fetch's own callbacks then settle the terminal state.
    queueMicrotask(() => {
      if (request === generation.current) setBody({ kind: "loading" });
    });
    void Promise.all([readRunTranscript(runId), readRunArtifact(runId)])
      .then(([transcript, text]) => {
        if (request !== generation.current) return;
        if (transcript) setBody({ kind: "turns", transcript });
        else if (text) setBody({ kind: "text", text });
        else setBody({ kind: "notCaptured" });
      })
      .catch(() => {
        if (request !== generation.current) return;
        setBody({ kind: "notCaptured" });
      });
    return () => {
      generation.current += 1;
    };
  }, [runId]);
  return body;
}

function completenessLabel(body: AttemptBody): string {
  switch (body.kind) {
    case "loading":
      return "Loading evidence…";
    case "turns":
      return "Complete · structured transcript";
    case "text":
      return body.text.includes("bytes truncated") ? "Bounded — truncated" : "Complete · text";
    case "notCaptured":
      return "Not captured";
  }
}

function containsRedaction(body: AttemptBody): boolean {
  if (body.kind === "text") return hasRedactionMarker(body.text);
  if (body.kind === "turns")
    return body.transcript.turns.some(
      (t) =>
        hasRedactionMarker(t.text) ||
        (t.tool_calls?.some((c) => hasRedactionMarker(c.input) || hasRedactionMarker(c.result)) ??
          false),
    );
  return false;
}

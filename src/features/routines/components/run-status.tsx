// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Copy, X } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { AppButton as Button } from "@/shared/components/app-button";
import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import { selectTabBySessionId, useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentsStore } from "@/features/agents/stores/agents-store";
import type { RoutineRun, RoutineRunPhase } from "@/tauri/commands";

import {
  runLine,
  runStatus,
  unknownCause,
  type RunContext,
  type RunStatusView,
} from "../lib/run-status";
import { ArgvChips } from "./routine-command-fields";
import { commandRunLine } from "../lib/routine-display";

import { StatusMark } from "@/shared/components/status-mark";

/** A run's status mark. Always beside its word, so it is hidden from assistive tech. */
export function RunStatusMark({ view, className }: { view: RunStatusView; className?: string }) {
  return (
    <StatusMark
      mark={view.mark}
      size={8}
      data-testid="run-status-dot"
      data-status={view.status}
      className={className}
    />
  );
}

/** Mark plus word: how every run row names its status. */
export function RunStatusWord({
  run,
  className,
}: {
  run: { phase: RoutineRunPhase; delivered_at?: number };
  className?: string;
}) {
  const view = runStatus(run);
  return (
    <span className={cn("inline-flex items-center gap-1.5", className)}>
      <RunStatusMark view={view} />
      <span className="text-foreground">{view.word}</span>
    </span>
  );
}

/** A fire time with its weekday ("Mon 8:30"). */
export function fireTime(at: number): string {
  return new Intl.DateTimeFormat(undefined, {
    weekday: "short",
    hour: "numeric",
    minute: "2-digit",
  }).format(at);
}

/**
 * The Details popover of an undelivered or unknown run (design §4.7): what happened in
 * plain words, where it went, and the one thing to do next. An unknown run's only
 * action is to look at the tab: never Retry, never Run now (R5).
 */
export function RunDetails({
  run,
  ctx,
  label = "Details",
  onOpen,
}: {
  run: RoutineRun;
  ctx: RunContext;
  label?: string;
  /** Told when the popover opens (the Schedule counts the run as seen). */
  onOpen?: (runId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const tabName = useViewerStore((s) =>
    run.session_id ? selectTabBySessionId(s, run.session_id)?.name : undefined,
  );
  const view = runStatus(run);
  const titleId = `run-details-${run.run_id}`;
  const at = run.ended_at ?? run.started_at ?? run.created_at;
  const result =
    run.phase === "unknown" ? unknownCause(run) : (runLine(run, ctx) ?? view.word) + ".";
  const show = () => {
    setOpen(false);
    openAgentResult({ run_id: run.run_id, sessionId: run.session_id });
  };
  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) {
          // Opened: it no longer counts toward the Schedule's "need attention".
          useAgentsStore.getState().markRunsSeen([run.run_id]);
          onOpen?.(run.run_id);
        }
      }}
    >
      <PopoverTrigger asChild>
        <button
          type="button"
          data-testid="run-details-trigger"
          className="text-xs text-foreground underline decoration-dotted underline-offset-2 hover:decoration-solid"
        >
          {label}
        </button>
      </PopoverTrigger>
      <PopoverContent
        role="dialog"
        aria-labelledby={titleId}
        align="start"
        data-testid="run-details"
        data-status={view.status}
        className="w-80 max-w-[calc(100vw-16px)] gap-2 p-3 shadow-none"
      >
        <div className="flex items-center gap-2">
          <RunStatusMark view={view} />
          <h3 id={titleId} className="text-sm font-semibold">
            {view.word}
          </h3>
          <button
            type="button"
            aria-label="Close"
            onClick={() => setOpen(false)}
            className="ml-auto inline-flex size-6 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <X aria-hidden="true" className="size-3.5" />
          </button>
        </div>
        <p className="text-sm text-foreground">{runLine(run, ctx)}</p>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1 text-xs">
          <dt className="text-muted-foreground">Fired</dt>
          <dd className="tabular-nums">{fireTime(at)}</dd>
          <dt className="text-muted-foreground">Into</dt>
          <dd data-testid="run-details-into">
            {ctx.agent} · {ctx.machine}
            {tabName ? ` · tab “${tabName}”` : ""}
          </dd>
          <dt className="text-muted-foreground">Result</dt>
          <dd>{result}</dd>
          <dt className="text-muted-foreground">Run</dt>
          <dd className="flex items-center gap-1">
            <span className="font-mono text-[11px]">{run.run_id}</span>
            <button
              type="button"
              aria-label="Copy run id"
              onClick={() => void navigator.clipboard?.writeText(run.run_id)}
              className="inline-flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
            >
              <Copy aria-hidden="true" className="size-3" />
            </button>
          </dd>
        </dl>
        <div>
          {run.session_id ? (
            <Button size="sm" variant="outline" onClick={show}>
              Show {ctx.agent}&apos;s tab
            </Button>
          ) : (
            <p className="text-xs text-muted-foreground">
              The session has ended. Its scrollback is in Run history.
            </p>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

export function CommandRunDetails({ run, now }: { run: RoutineRun; now: number }) {
  const shellQuote = (arg: string) => "'" + arg.replace(/'/g, "'\\''") + "'";
  return (
    <>
      <dl className="grid min-w-0 grid-cols-[max-content_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
        <dt className="text-muted-foreground">Ran</dt>
        <dd className="flex min-w-0 items-start gap-1">
          <ArgvChips argv={run.argv ?? []} />
          <button
            type="button"
            aria-label="Copy command"
            onClick={() =>
              void navigator.clipboard?.writeText((run.argv ?? []).map(shellQuote).join(" "))
            }
          >
            <Copy className="size-3" aria-hidden="true" />
          </button>
        </dd>
        <dt className="text-muted-foreground">In</dt>
        <dd className="break-all">
          {run.cwd ?? "Team workspace"} · {run.launch_machine ?? "This Mac"}
        </dd>
        <dt className="text-muted-foreground">{run.started_at === null ? "Reason" : "Exit"}</dt>
        <dd>{commandRunLine(run, now)}</dd>
        <dt className="text-muted-foreground">Run</dt>
        <dd className="flex min-w-0 items-start gap-1">
          <code className="break-all">{run.run_id}</code>
          <button
            type="button"
            aria-label="Copy run id"
            onClick={() => void navigator.clipboard?.writeText(run.run_id)}
          >
            <Copy className="size-3" aria-hidden="true" />
          </button>
        </dd>
      </dl>
      <span className="text-xs font-medium">Output</span>
      <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">
        {run.started_at === null ? "No output." : run.result_text || "No output."}
      </pre>
      <p className="text-xs text-muted-foreground">Last 4 KB, secrets hidden.</p>
      {run.stderr_tail ? (
        <details open={run.phase === "failed" || run.phase === "timed_out"}>
          <summary className="cursor-pointer text-xs">Errors (stderr)</summary>
          <pre className="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">
            {run.stderr_tail}
          </pre>
        </details>
      ) : (
        <p className="text-xs text-muted-foreground">No error output.</p>
      )}
    </>
  );
}

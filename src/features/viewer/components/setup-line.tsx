// SPDX-License-Identifier: MIT
import { useEffect, useId, useState, type ReactNode } from "react";
import { AlertTriangle, Check, X } from "lucide-react";

import {
  MachineActionCancelled,
  updateMachineTerminalServer,
} from "@/features/machines/lib/machine-actions";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { getErrorMessage } from "@/shared/lib/error-message";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

import { agentLaunchRetry } from "../lib/agent-launch-retry";
import {
  describeSetup,
  displayedLine,
  needsServerUpdate,
  nextLineChangeAt,
  STEP1_FAILED_WHILE,
  type SetupSlots,
  type SetupStep1Phase,
  type SetupView,
  type WorkspaceSetupState,
} from "../lib/workspace-setup-state";

/**
 * SW-XS-10 (SPEC §8.1, §8.6): the workspace setup line — one line, the phase on
 * the left and a determinate count on the right in tabular numerals, with an
 * `--accent` ring. An indeterminate phase shows a static arc and a stepping
 * ellipsis, never a spinner. A failure reads `Couldn't prepare {agent} —
 * {reason}` with Retry and Details as real buttons, and is an alert.
 *
 * Phase changes are announced by the workspace's one status region
 * ({@link WorkspaceAnnouncer}), once per phase, never per count.
 */

/** A workspace's setup slots, as the store keeps them. */
export function slotsOf(ws: Pick<Workspace, "setupState" | "agentRuns">): SetupSlots {
  return { base: ws.setupState, runs: ws.agentRuns };
}

/** The line as shown now: holds a young Step 2 back (no flash), lets a Ready
 *  go quiet and a wait expire — with one timer, never a ticking clock. The
 *  clock is read only in the effect: a state newer than the last tick reads as
 *  "just now" until the timer moves the clock past its boundary. */
export function useDisplayedLine(slots: SetupSlots): WorkspaceSetupState | undefined {
  const [now, setNow] = useState(() => Date.now());
  const { base, runs } = slots;
  useEffect(() => {
    const t = Date.now();
    const at = nextLineChangeAt({ base, runs }, t);
    if (at === null) return;
    const timer = setTimeout(() => setNow(Date.now()), Math.max(0, at - t) + 1);
    return () => clearTimeout(timer);
  }, [base, runs, now]);
  return displayedLine(slots, now);
}

/** The leading mark: an `--accent` ring filled to `fraction` when the phase is
 *  determinate, a static arc when it is not, a check on Ready, a warning on
 *  failure. */
export function SetupMark({ view, size = 12 }: { view: SetupView; size?: number }) {
  if (view.tone === "ready") {
    return (
      <Check
        size={size}
        strokeWidth={2.5}
        className="shrink-0 text-[var(--cs-status-done)]"
        aria-hidden
      />
    );
  }
  if (view.tone === "failed") {
    return (
      <AlertTriangle size={size} className="shrink-0 text-[var(--cs-status-failed)]" aria-hidden />
    );
  }
  const r = 5;
  const c = 2 * Math.PI * r;
  const determinate = view.fraction !== undefined;
  // Indeterminate: a fixed quarter arc, no track — it says "working", it
  // doesn't pretend to measure.
  const fill = determinate ? Math.min(1, view.fraction ?? 0) : 0.25;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 14 14"
      className="shrink-0 -rotate-90"
      aria-hidden
      data-testid="setup-ring"
      data-fraction={determinate ? (view.fraction ?? 0).toFixed(3) : ""}
    >
      {determinate && (
        <circle
          cx="7"
          cy="7"
          r={r}
          fill="none"
          strokeWidth="2"
          style={{ stroke: "color-mix(in srgb, var(--accent) 25%, transparent)" }}
        />
      )}
      {fill > 0 && (
        <circle
          cx="7"
          cy="7"
          r={r}
          fill="none"
          strokeWidth="2"
          strokeLinecap="round"
          strokeDasharray={`${(c * fill).toFixed(2)} ${c.toFixed(2)}`}
          className="transition-[stroke-dasharray] duration-200 ease-out motion-reduce:transition-none"
          style={{ stroke: "var(--accent)" }}
        />
      )}
    </svg>
  );
}

/** The stepping ellipsis an indeterminate phase carries (static under reduced
 *  motion), set apart from the words so it never reads as truncation. */
function Ellipsis() {
  return (
    <span className="cs-setup-ellipsis ml-0.5 tracking-[0.08em]" aria-hidden>
      <span>.</span>
      <span>.</span>
      <span>.</span>
    </span>
  );
}

export interface SetupLineViewProps {
  view: SetupView;
  /** `overlay`: centered in the creating cover; `corner`: a framed chip over
   *  the live terminal; `pane`: the failure placeholder's body line. */
  variant?: "overlay" | "corner" | "pane";
  /** Failure actions (real buttons). */
  actions?: ReactNode;
  /** The failure's details, shown inline below the line when Details is on. */
  details?: ReactNode;
  /** Closes a failure line; drawn last, after Details. */
  onDismiss?: () => void;
  testId?: string;
}

export const PRIMARY_BUTTON =
  "inline-flex min-h-6 items-center rounded border border-[var(--accent)] px-2 text-[11px] font-medium text-[var(--foreground)] transition-colors hover:bg-[color-mix(in_srgb,var(--accent)_14%,transparent)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-[var(--accent)] disabled:cursor-not-allowed disabled:opacity-50";
export const SECONDARY_BUTTON =
  "inline-flex min-h-6 items-center rounded border border-[var(--border)] px-2 text-[11px] text-[var(--muted-foreground)] transition-colors hover:text-[var(--foreground)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-[var(--accent)] disabled:cursor-not-allowed disabled:opacity-50";
export const ICON_BUTTON =
  "inline-flex size-6 shrink-0 items-center justify-center rounded text-[var(--muted-foreground)] hover:text-[var(--foreground)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--accent)]";

/** The line itself, presentational (the harness and tests drive it directly). */
export function SetupLineView({
  view,
  variant = "overlay",
  actions,
  details,
  onDismiss,
  testId,
}: SetupLineViewProps) {
  const [open, setOpen] = useState(false);
  const detailsId = useId();
  const failed = view.tone === "failed";
  const surface =
    variant === "pane"
      ? ""
      : `${
          variant === "corner"
            ? " rounded border border-[var(--border)] bg-[var(--popover)] px-2 py-1"
            : ""
        }`;
  return (
    <div
      data-surface={variant === "pane" ? undefined : "terminal"}
      className={`flex min-w-0 flex-col gap-1 text-xs ${surface} ${
        variant === "overlay" ? "w-full max-w-sm" : ""
      }`}
      data-testid={testId ?? "setup-line"}
      data-tone={view.tone}
    >
      <div role={failed ? "alert" : undefined} className="flex min-w-0 items-center gap-2">
        <SetupMark view={view} />
        <span
          className={`min-w-0 flex-1 text-left font-medium text-[var(--foreground)] ${
            failed ? "line-clamp-2" : "truncate"
          }`}
          data-testid="setup-line-text"
          title={view.text}
        >
          {view.text}
          {view.indeterminate && <Ellipsis />}
        </span>
        {view.count && (
          <span
            className="shrink-0 whitespace-nowrap tabular-nums text-[var(--muted-foreground)]"
            data-testid="setup-line-count"
          >
            {view.count}
          </span>
        )}
      </div>
      {failed && (actions || details || onDismiss) && (
        <div className="flex flex-wrap items-center gap-1.5 pl-5">
          {actions}
          {details && (
            <button
              type="button"
              aria-expanded={open}
              aria-controls={detailsId}
              onClick={() => setOpen((o) => !o)}
              className={SECONDARY_BUTTON}
            >
              Details
            </button>
          )}
          {onDismiss && (
            <button type="button" aria-label="Dismiss" className={ICON_BUTTON} onClick={onDismiss}>
              <X size={12} />
            </button>
          )}
        </div>
      )}
      {failed && details && open && (
        <section
          id={detailsId}
          aria-label="Setup failure details"
          className="ml-5 max-h-48 overflow-auto rounded border border-[var(--border)] bg-[var(--popover)] p-2 text-left text-[var(--foreground)]"
          data-testid="setup-line-details"
        >
          {details}
        </section>
      )}
    </div>
  );
}

/** What a failed step's Details lists. */
export function SetupFailureDetails({
  state,
}: {
  state: Extract<WorkspaceSetupState, { kind: "failed" }>;
}) {
  if (state.step === 1) {
    return (
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
        <dt className="text-[var(--muted-foreground)]">Stopped while</dt>
        <dd>{STEP1_FAILED_WHILE[state.phase as SetupStep1Phase] ?? state.phase}</dd>
        <dt className="text-[var(--muted-foreground)]">Reason</dt>
        <dd className="break-words select-text">{state.reason}</dd>
        <dt className="text-[var(--muted-foreground)]">Retry</dt>
        <dd>Creates the worktree again; a machine reuses the copy it already has.</dd>
      </dl>
    );
  }
  return (
    <div className="space-y-2">
      {state.failed && state.failed.length > 0 ? (
        <ul className="space-y-0.5" aria-label="Files that could not be written">
          {state.failed.map(([path, why]) => (
            <li key={path} className="break-words select-text">
              <span className="font-mono">{path}</span>
              <span className="text-[var(--muted-foreground)]"> — {why}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="break-words select-text">{state.reason}</p>
      )}
      {state.note && <p className="text-[var(--muted-foreground)] select-text">{state.note}</p>}
      <p className="text-[var(--muted-foreground)]">
        Retry prepares {state.agent ?? "the agent"} again; the worktree is kept as it is.
      </p>
    </div>
  );
}

/** "Update terminal server", with its own busy and done states. */
function UpdateServerButton({
  machineId,
  onUpdated,
}: {
  machineId: string;
  onUpdated: () => void;
}) {
  const [step, setStep] = useState<"idle" | "updating" | "updated" | "failed">("idle");
  const [why, setWhy] = useState<string | null>(null);
  if (step === "updated")
    return (
      <span role="status" className="text-[11px] text-[var(--muted-foreground)]">
        Terminal server updated
      </span>
    );
  return (
    <>
      <button
        type="button"
        className={PRIMARY_BUTTON}
        data-testid="setup-line-update-server"
        disabled={step === "updating"}
        onClick={() => {
          setStep("updating");
          // SW-AGR-19: "updated" only once the update finished — never when
          // the confirmation merely opened; closing it changes nothing.
          updateMachineTerminalServer(machineId)
            .then(() => {
              setStep("updated");
              onUpdated();
            })
            .catch((e: unknown) => {
              if (e instanceof MachineActionCancelled) {
                setStep("idle");
                return;
              }
              setWhy(getErrorMessage(e));
              setStep("failed");
            });
        }}
      >
        {step === "updating" ? "Updating…" : "Update terminal server"}
      </button>
      {step === "failed" && why && (
        <span role="alert" className="text-[11px] text-[var(--foreground)]">
          Couldn&apos;t update: {why}
        </span>
      )}
    </>
  );
}

/** The failure line's buttons for one workspace: Retry (Step 1 again, or that
 *  agent's launch again — Step 2, then the spawn), "Launch <agent> again" when
 *  no launch of it was recorded here, and Update terminal server when the
 *  machine's is too old (then Retry). */
export function SetupFailureActions({
  ws,
  teamKey,
  state,
}: {
  ws: Workspace;
  teamKey: string;
  state: Extract<WorkspaceSetupState, { kind: "failed" }>;
}) {
  const [busy, setBusy] = useState(false);
  const [serverUpdated, setServerUpdated] = useState(false);
  const store = useViewerStore;
  const machineId = isRemoteWorkspace(ws) ? ws.location.machineId : undefined;
  const update = !!machineId && needsServerUpdate(state) && !serverUpdated;
  const recordedRetry =
    state.step === 2 ? agentLaunchRetry(teamKey, ws.id, state.agent) : undefined;
  const relaunch =
    recordedRetry ??
    (ws.kind === "context"
      ? async () => {
          openLaunchSheet({
            teamKey,
            contextWorktreeId: ws.contextWorktreeId ?? ws.id,
            where: "team-workspace",
          });
        }
      : undefined);
  const agent = state.agent ?? "the agent";

  const retry = () => {
    if (state.step === 1) {
      setBusy(true);
      void store
        .getState()
        .retryWorkspaceCreate(teamKey, ws.id)
        .finally(() => setBusy(false));
      return;
    }
    if (!relaunch) return;
    setBusy(true);
    // Only this agent's run goes; another agent's failure or progress stays.
    store.getState().clearAgentRun(teamKey, ws.id, agent);
    relaunch()
      .catch((error: unknown) => {
        const current = store.getState().workspaces[teamKey]?.find((w) => w.id === ws.id);
        // The launch's own failed event already said why; otherwise say it here.
        if (current?.agentRuns?.[agent]?.kind !== "failed")
          store
            .getState()
            .failAgentRun(
              teamKey,
              ws.id,
              agent,
              getErrorMessage(error),
              (error as { code?: string }).code,
            );
      })
      .finally(() => setBusy(false));
  };

  return (
    <>
      {machineId && needsServerUpdate(state) && (
        <UpdateServerButton machineId={machineId} onUpdated={() => setServerUpdated(true)} />
      )}
      {update ? null : state.step === 1 || relaunch ? (
        <button
          type="button"
          className={PRIMARY_BUTTON}
          disabled={busy}
          onClick={retry}
          data-testid="setup-line-retry"
        >
          {busy ? "Retrying…" : "Retry"}
        </button>
      ) : (
        <button
          type="button"
          className={PRIMARY_BUTTON}
          data-testid="setup-line-launch"
          onClick={() => {
            store.getState().clearAgentRun(teamKey, ws.id, agent);
            openLaunchSheet({
              teamKey,
              ...(ws.storyId ? { storyId: ws.storyId } : {}),
              ...(ws.kind === "context"
                ? { contextWorktreeId: ws.contextWorktreeId ?? ws.id }
                : {}),
            });
          }}
        >
          Launch {agent} again
        </button>
      )}
    </>
  );
}

/** The connected setup line for one workspace (`overlay` inside the creating
 *  cover; `corner` over a live terminal for its agents' Step 2). Renders
 *  nothing while there is nothing to say. */
export function WorkspaceSetupLine({
  ws,
  teamKey,
  variant,
}: {
  ws: Workspace;
  teamKey: string;
  variant: "overlay" | "corner";
}) {
  const shown = useDisplayedLine(slotsOf(ws));
  const view = describeSetup(shown);
  if (!view || !shown) return null;
  return (
    <SetupLineView
      view={view}
      variant={variant}
      testId={`workspace-setup-line-${ws.id}`}
      {...(shown.kind === "failed"
        ? {
            actions: <SetupFailureActions ws={ws} teamKey={teamKey} state={shown} />,
            details: <SetupFailureDetails state={shown} />,
            ...(shown.step === 2
              ? {
                  onDismiss: () =>
                    useViewerStore
                      .getState()
                      .clearAgentRun(teamKey, ws.id, shown.agent ?? "the agent"),
                }
              : {}),
          }
        : {})}
    />
  );
}

/**
 * SW-XS-10 (SPEC §8.6): the workspace's one status region, always mounted, so
 * a screen reader hears each change exactly once: the setup line's phase
 * (never its counts) and the operation chip's state — a running operation by
 * its verb, a finished or failed one by its line, politely and once (the text
 * holds while the state does, so a sticky failure is not repeated). A setup
 * failure is the visible line's own alert, not repeated here.
 */
export function WorkspaceAnnouncer({
  ws,
  operation,
}: {
  ws: Workspace;
  /** What the operation chip says now, if anything (its verb while running). */
  operation?: string;
}) {
  const shown = useDisplayedLine(slotsOf(ws));
  const view = describeSetup(shown);
  const line = view && view.tone !== "failed" ? view.announce : "";
  // Say only what just changed: a new phase, or the operation's new state.
  // A line turning into a failure (its own alert) or going quiet changes
  // nothing here, so the operation's verb is never said twice.
  const [heard, setHeard] = useState({ line, operation, text: line || operation || "" });
  if (heard.line !== line || heard.operation !== operation) {
    const text =
      line && line !== heard.line
        ? line
        : operation && operation !== heard.operation
          ? operation
          : heard.text;
    setHeard({ line, operation, text });
  }
  const text = heard.text;
  return (
    <span
      role="status"
      aria-live="polite"
      className="sr-only"
      data-testid={`workspace-announcer-${ws.id}`}
    >
      {text}
    </span>
  );
}

// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { ChevronRight } from "lucide-react";

import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { useOrgStore } from "@/shared/stores/org-store";
import { RuntimeBadge } from "@/features/agents/components/agent-bits";
import {
  routinePreviewSchedule,
  type AgentRow,
  type RoutineSchedulePreview,
} from "@/tauri/commands";

import { findAgent, type RoutineSpace } from "../lib/routine-agent-ref";
import { scheduleLabel } from "../lib/routine-display";
import {
  canWake,
  defaultMode,
  limitsProblem,
  limitsSummary,
  machineProblem,
  machineProblemCopy,
  shownMode,
  type SentenceDraft,
} from "../lib/routine-sentence";
import { machineLabel, machineTeamOf, useRoutineMachines } from "../lib/use-routine-machines";
import { useRoutinesStore } from "../stores/routines-store";
import { RoutineMetadataTriggers } from "./routine-metadata-triggers";
import { FIELD, SELECT, PREVIEW_DEBOUNCE_MS } from "../lib/routine-form-controls";
import { RoutineAgentPicker, RoutineAgentSummary, type SavedAgent } from "./routine-agent-picker";

import {
  RoutineCommandFields,
  RoutineCommandReadback,
  RoutineCommandHelp,
  useCommandPreview,
} from "./routine-command-fields";

const PERSONAL: RoutineSpace = { scope: "personal" };

/**
 * The routine as one editable sentence — *Every [when] [wake ▾] [agent] to [task] in
 * [where] on [machine]* (SW-AGR-5, design §4.6) — with its name, its mode hint, and its
 * options folded under one summary line. The same fields serve New routine and a
 * routine's Overview. Under an agent, the agent slot is fixed: a routine belongs to the
 * agent it runs.
 */
export function RoutineSentenceFields({
  draft,
  onChange,
  whenLocked = null,
  routine = PERSONAL,
  savedAgent,
  fixedAgent,
  migrated = false,
  routineKey,
  armedMachine,
  onCommandBlocked,
  saveError,
}: {
  saveError?: string | null;
  onCommandBlocked?: (blocked: boolean) => void;
  routineKey?: string;
  armedMachine?: string | null;
  draft: SentenceDraft;
  onChange: (next: SentenceDraft) => void;
  /** Why the schedule slot cannot be edited here, when it cannot. */
  whenLocked?: string | null;
  /** Where the routine lives, which decides the agents it is offered. A team routine
   *  also chooses how it runs: once for the team, or on each teammate's Mac that turns
   *  it on. */
  routine?: RoutineSpace;
  /** The agent the routine was saved with, kept pickable even when archived. */
  savedAgent?: SavedAgent;
  /** The agent the routine belongs to: shown, never picked here. */
  fixedAgent?: AgentRow;
  /** The routine moved under its agent, set to start a new session then, and has not
   *  been saved since (§9.2): the hint says why. */
  migrated?: boolean;
}) {
  const team = routine.scope === "team";
  const command = draft.action === "command";
  const commandUnavailable = !team
    ? "Commands run in a team’s workspace for now. Pick a team under Who can use it."
    : armedMachine
      ? "Command routines can’t be armed on a machine yet."
      : null;
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const personalOrg =
    routineKey?.startsWith("_personal/") && routineKey.includes("/_scratch/")
      ? routineKey.split("/").slice(2, -2).join("/")
      : null;
  const observationOrg =
    routine.scope === "team"
      ? routine.space.slice(0, routine.space.lastIndexOf("/"))
      : personalOrg || fixedAgent?.org || activeOrg || "";
  const set = <K extends keyof SentenceDraft>(field: K, value: SentenceDraft[K]) =>
    onChange({ ...draft, [field]: value });
  const preview = useCommandPreview({
    draft,
    routine,
    routineKey,
    onBlocked: onCommandBlocked,
    enabled: command,
  });
  const repo = draft.where.startsWith("repo:");
  const agents = useRoutinesStore((s) => s.agents);
  const agentRow = fixedAgent ?? findAgent(agents ?? [], draft.agent, routine);
  const mode = shownMode(draft, agentRow);
  const wakeAllowed = canWake(agentRow, draft.agent);
  const {
    options: machines,
    labels,
    known: knownIds,
  } = useRoutineMachines(machineTeamOf(agentRow), agentRow?.scope === "personal");
  const recipe = team && draft.runsOn === "each_mac";
  const machine = draft.machine || "local";
  const name = agentRow?.name ?? (draft.agent.trim() || "the agent");
  // A fresh run starts on This Mac for now (SW-AGR-2b lifts it); a recipe each Mac
  // runs wakes only that Mac's agent; a machine no longer shared can't be named.
  const problem = machineProblem(draft, mode, knownIds, team);
  const known = problem !== "unshared";
  return (
    <div className="flex flex-col gap-3 text-sm">
      <label className="flex max-w-96 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Name</span>
        <Input
          aria-label="Name"
          className={FIELD}
          value={draft.name}
          placeholder="Morning triage"
          onChange={(e) => set("name", e.target.value)}
        />
      </label>
      <div
        data-testid="routine-sentence-form"
        className="flex flex-wrap items-center gap-x-2 gap-y-2 leading-8"
      >
        <span className={!draft.when.trim() ? "text-muted-foreground" : undefined}>Every</span>
        <Input
          aria-label="When"
          className={cn("w-40", FIELD)}
          value={draft.when}
          disabled={whenLocked !== null}
          title={whenLocked ?? undefined}
          placeholder={
            (draft.events?.length ?? 0) > 0 || draft.after ? "optional" : "5m, day 09:00, mon 17:00"
          }
          onChange={(e) => set("when", e.target.value)}
        />
        <select
          aria-label="Mode"
          data-testid="routine-mode"
          className={cn(SELECT, "font-medium")}
          value={command ? "command" : mode}
          aria-describedby={
            commandUnavailable ? "routine-command-unavailable" : "routine-mode-hint"
          }
          onChange={(e) => {
            if (e.target.value === "command")
              onChange({
                ...draft,
                action: "command",
                machine: "local",
                ...(!routineKey
                  ? {
                      overlap: "queue",
                      maxRuntime: draft.maxRuntimeTouched ? draft.maxRuntime : "5m",
                    }
                  : {}),
              });
            else
              onChange({
                ...draft,
                action: "agent",
                mode: e.target.value as SentenceDraft["mode"],
              });
          }}
        >
          <option value="wake" disabled={!wakeAllowed}>
            wake
          </option>
          <option value="fresh">start a new</option>
          {!fixedAgent && (
            <option value="command" disabled={!!commandUnavailable}>
              run the command
              {!team ? " (team routines only)" : armedMachine ? " (not on armed machines yet)" : ""}
            </option>
          )}
        </select>
        {!command && (
          <>
            {fixedAgent ? (
              <span
                data-testid="routine-agent-fixed"
                title={`Routines belong to ${fixedAgent.name}. Use Move to another agent… to change it.`}
                className="inline-flex items-center gap-1.5 rounded bg-muted px-1.5 py-0.5 leading-6 font-medium"
              >
                <RuntimeBadge runtime={fixedAgent.runtime} />
                {fixedAgent.name}
              </span>
            ) : (
              <RoutineAgentPicker
                value={draft.agent}
                onChange={(ref) => set("agent", ref)}
                routine={routine}
                saved={savedAgent}
              />
            )}
            <span>to</span>
            <textarea
              aria-label="Task"
              className={cn(
                "min-h-16 w-full rounded-lg border border-input px-2.5 py-1.5 text-sm outline-none focus-visible:border-ring disabled:opacity-60",
                FIELD,
              )}
              value={draft.task}
              placeholder={`What ${name} should do on each run`}
              onChange={(e) => set("task", e.target.value)}
            />
          </>
        )}
        {command && <RoutineCommandFields draft={draft} onChange={onChange} preview={preview} />}
        <span>in</span>
        <select
          aria-label="Where"
          className={SELECT}
          value={repo ? "repo" : "team_workspace"}
          onChange={(e) => set("where", e.target.value === "repo" ? "repo:" : "team_workspace")}
        >
          <option value="team_workspace">the team workspace</option>
          <option value="repo">a repo</option>
        </select>
        {repo && (
          <Input
            aria-label="Repo"
            className={cn("w-48", FIELD)}
            value={draft.where.slice("repo:".length)}
            placeholder="owner/repo"
            onChange={(e) => set("where", `repo:${e.target.value}`)}
          />
        )}
        <span>on</span>
        {recipe ? (
          <span data-testid="routine-machine" className="whitespace-nowrap">
            each teammate&apos;s Mac
          </span>
        ) : (
          <select
            aria-label="Machine"
            data-testid="routine-machine"
            className={cn(SELECT, "whitespace-nowrap")}
            value={machine}
            aria-invalid={problem !== null || undefined}
            aria-describedby={problem ? "routine-machine-error" : undefined}
            onChange={(e) => set("machine", e.target.value)}
          >
            {machines
              .filter((m) => !command || m.id === "local")
              .map((m) => (
                <option key={m.id} value={m.id}>
                  {m.offline ? `${m.label} (offline)` : m.label}
                </option>
              ))}
            {!known && <option value={machine}>{machine}</option>}
            {!command && (mode === "wake" || machine === "any") && (
              <option value="any">any connected machine</option>
            )}
          </select>
        )}
      </div>
      {problem && (
        <p id="routine-machine-error" role="alert" className="text-xs text-destructive">
          {machineProblemCopy(problem, machineLabel(machine, labels), name)}
        </p>
      )}
      {commandUnavailable && !fixedAgent && (
        <p id="routine-command-unavailable" className="text-xs text-muted-foreground">
          {commandUnavailable}
        </p>
      )}
      {command && <RoutineCommandReadback preview={preview} />}
      {!command && !fixedAgent && <RoutineAgentSummary value={draft.agent} routine={routine} />}
      {command ? (
        <p id="routine-mode-hint" className="text-xs text-muted-foreground">
          Runs the command in{" "}
          {draft.where === "team_workspace"
            ? "the team workspace"
            : draft.where.replace("repo:", "")}{" "}
          on {recipe ? "each teammate’s Mac" : "This Mac"}. No agent and no model.
        </p>
      ) : (
        <ModeHint
          mode={mode}
          name={name}
          machine={machineLabel(machine, labels)}
          any={machine === "any"}
          wakeAllowed={wakeAllowed || !draft.agent.trim()}
          isDefault={!draft.mode && mode === defaultMode(agentRow, draft.visible)}
          migrated={migrated && mode === "fresh"}
          codex={agentRow?.runtime === "codex"}
        />
      )}
      {command ? <RoutineCommandHelp draft={draft} /> : <TaskHelp mode={mode} name={name} />}
      {mode === "wake" && !!draft.events?.length && (
        <p className="text-xs text-muted-foreground">
          When a trigger fires, {name} can call <code>routine_context</code> to see which document
          changed and how.
        </p>
      )}
      {whenLocked && (
        <p data-testid="routine-when-locked" className="text-xs text-muted-foreground">
          {whenLocked}
        </p>
      )}
      {!draft.when.trim() && (!!draft.events?.length || draft.after) ? (
        <p className="text-xs text-muted-foreground">
          No schedule: runs only on its triggers. Type one, like 5m or day 09:00, to also run on a
          clock.
        </p>
      ) : (
        <SchedulePreview when={draft.when} tz={draft.tz} />
      )}
      <RoutineMetadataTriggers
        draft={draft}
        onChange={onChange}
        routine={routine}
        org={observationOrg}
        routineKey={routineKey}
        chainError={
          saveError?.includes("loop") || saveError?.includes("no longer exists") ? saveError : null
        }
        armedMachine={armedMachine ? machineLabel(armedMachine, labels) : undefined}
      />
      {!draft.when.trim() && !draft.events?.length && !draft.after && (
        <p className="text-xs text-muted-foreground">
          Type a schedule, or add a trigger: the routine needs one to run.
        </p>
      )}
      <Options draft={draft} set={set} onChange={onChange} mode={mode} team={team} />
      {!command && <Limits draft={draft} set={set} mode={mode} />}
    </div>
  );
}

const OVERLAP_WORDS: Record<SentenceDraft["overlap"], string> = {
  coalesce: "merges into a running one",
  skip: "skips a fire while one runs",
  queue: "runs after the one going",
};

/** The policy, folded under one summary line so Save stays above the fold (design
 *  §4.6). Rows that do not apply to the mode are left out, not disabled. */
function Options({
  draft,
  set,
  onChange,
  mode,
  team,
}: {
  draft: SentenceDraft;
  set: <K extends keyof SentenceDraft>(field: K, value: SentenceDraft[K]) => void;
  onChange: (next: SentenceDraft) => void;
  mode: SentenceDraft["mode"];
  team: boolean;
}) {
  const wake = mode === "wake";
  const summary = [
    wake ? `waits up to ${draft.maxWait || "30m"} for idle` : OVERLAP_WORDS[draft.overlap],
    draft.missed === "skip" ? "skips missed runs" : "runs a missed one on return",
    `${draft.maxRuntime.trim() || "30m"} max runtime`,
  ].join(" · ");
  return (
    <details data-testid="routine-options" className="group text-xs">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 text-muted-foreground select-none hover:text-foreground [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden="true"
          className="size-3 shrink-0 transition-transform duration-150 group-open:rotate-90 motion-reduce:transition-none"
        />
        <span>
          <span className="font-medium text-foreground">Options</span> · {summary}
        </span>
      </summary>
      <fieldset
        aria-label="Options"
        className="mt-2.5 grid min-w-0 grid-cols-[max-content_minmax(0,1fr)] items-center gap-x-3 gap-y-2"
      >
        {team && (
          <>
            <span className="text-muted-foreground">Runs</span>
            <select
              aria-label="Runs"
              className={cn(SELECT, "h-7 w-64 max-w-full text-xs")}
              value={draft.runsOn}
              onChange={(e) => {
                const runsOn = e.target.value as SentenceDraft["runsOn"];
                // Each teammate's Mac wakes its own agent: the machine is This Mac (§9.9).
                onChange({
                  ...draft,
                  runsOn,
                  ...(runsOn === "each_mac" ? { machine: "local" } : {}),
                });
              }}
            >
              <option value="one_mac">once for the team, on one Mac</option>
              <option
                value="each_mac"
                disabled={!!draft.after || draft.events?.some((e) => e.kind === "metadata_change")}
              >
                on each teammate's Mac
                {draft.after || draft.events?.some((e) => e.kind === "metadata_change")
                  ? " (schedules only)"
                  : " that turns it on"}
              </option>
            </select>
          </>
        )}
        <span className="text-muted-foreground">Time zone</span>
        <Input
          aria-label="Time zone"
          className={cn("h-7 w-48 max-w-full", FIELD)}
          value={draft.tz}
          placeholder="this Mac's zone"
          onChange={(e) => set("tz", e.target.value)}
        />
        {wake ? (
          <>
            <span className="text-muted-foreground">Wait for idle up to</span>
            <span data-testid="routine-max-wait" className="tabular-nums">
              {draft.maxWait || "30m"}
              <span className="text-muted-foreground"> · can&apos;t be changed here yet</span>
            </span>
          </>
        ) : (
          <>
            <span className="text-muted-foreground">If still running</span>
            <select
              aria-label="Overlap"
              className={cn(SELECT, "h-7 w-48 max-w-full text-xs")}
              value={draft.overlap}
              onChange={(e) => set("overlap", e.target.value as SentenceDraft["overlap"])}
            >
              <option value="coalesce">merge into the running one</option>
              <option value="skip">skip the new fire</option>
              <option value="queue">run after it</option>
            </select>
          </>
        )}
        <span className="text-muted-foreground">Missed while closed</span>
        <select
          aria-label="Missed"
          className={cn(SELECT, "h-7 w-48 max-w-full text-xs")}
          value={draft.missed}
          onChange={(e) => set("missed", e.target.value as SentenceDraft["missed"])}
        >
          <option value="skip">skip</option>
          <option value="run_once">run once on return</option>
        </select>
        <span className="text-muted-foreground">Max runtime</span>
        <Input
          aria-label="Max runtime"
          className={cn("h-7 w-48 max-w-full", FIELD)}
          value={draft.maxRuntime}
          placeholder="30m"
          onChange={(e) =>
            onChange({ ...draft, maxRuntime: e.target.value, maxRuntimeTouched: true })
          }
        />
        {!wake && draft.action !== "command" && (
          <>
            <span className="text-muted-foreground">Visible</span>
            <label className="inline-flex items-center gap-2">
              <input
                type="checkbox"
                aria-label="Visible"
                checked={draft.visible}
                onChange={(e) => set("visible", e.target.checked)}
              />
              <span>run in a terminal tab instead of headless</span>
            </label>
          </>
        )}
      </fieldset>
      {draft.action === "command" &&
        draft.overlap !== "queue" &&
        draft.command.some((a) => /\{\{(?!date\}|timestamp\}|last_run_)/.test(a)) && (
          <p className="mt-2 text-warning-ink">
            Fires that arrive while one runs are dropped, along with their{" "}
            <code>{"{{itemId}}"}</code>.
          </p>
        )}
    </details>
  );
}

/** SW-AGR-13: the routine's own wake limits, folded like Options and closed until
 *  opened. All optional: a light check can wake as often as it fires; a wake that
 *  starts heavy work gets a gap and a budget. */
function Limits({
  draft,
  set,
  mode,
}: {
  draft: SentenceDraft;
  set: <K extends keyof SentenceDraft>(field: K, value: SentenceDraft[K]) => void;
  mode: SentenceDraft["mode"];
}) {
  const gapSet = (draft.minGap ?? "").trim() !== "";
  const fresh = mode === "fresh";
  const problem = limitsProblem(draft);
  return (
    <details data-testid="routine-limits" className="group text-xs">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 text-muted-foreground select-none hover:text-foreground [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden="true"
          className="size-3 shrink-0 transition-transform duration-150 group-open:rotate-90 motion-reduce:transition-none"
        />
        <span>
          <span className="font-medium text-foreground">Limits</span> · {limitsSummary(draft)}
        </span>
      </summary>
      <p data-testid="routine-limits-hint" className="mt-2 max-w-[60ch] text-muted-foreground">
        {fresh
          ? "These count wakes into a running agent. This routine starts a new agent each time, so they don't apply; “If still running” under Options decides overlaps."
          : "Optional, and only for waking a running agent. A quick check can wake it every time it fires; set these when a wake starts heavy work that should not repeat or overlap. Each Mac and machine keeps its own count."}
      </p>
      <fieldset
        aria-label="Limits"
        className="mt-2.5 grid min-w-0 grid-cols-[max-content_minmax(0,1fr)] items-center gap-x-3 gap-y-2"
      >
        <span className="text-muted-foreground">Max wakes</span>
        <span className="flex min-w-0 items-center gap-2">
          <Input
            aria-label="Max wakes"
            inputMode="numeric"
            className={cn("h-7 w-24 tabular-nums", FIELD)}
            value={draft.maxWakes ?? ""}
            placeholder="no limit"
            onChange={(e) => set("maxWakes", e.target.value)}
          />
          <span className="text-muted-foreground">in all, then it pauses itself</span>
        </span>
        <span className="text-muted-foreground">Max wakes a day</span>
        <span className="flex min-w-0 items-center gap-2">
          <Input
            aria-label="Max wakes a day"
            inputMode="numeric"
            className={cn("h-7 w-24 tabular-nums", FIELD)}
            value={draft.maxWakesPerDay ?? ""}
            placeholder="no limit"
            onChange={(e) => set("maxWakesPerDay", e.target.value)}
          />
          <span className="text-muted-foreground">in any 24 hours</span>
        </span>
        <span className="text-muted-foreground">Min gap</span>
        <span className="flex min-w-0 items-center gap-2">
          <Input
            aria-label="Min gap"
            className={cn("h-7 w-24", FIELD)}
            value={draft.minGap ?? ""}
            placeholder="none"
            onChange={(e) => set("minGap", e.target.value)}
          />
          <span className="text-muted-foreground">between wakes, like 30m, 2h or 1d</span>
        </span>
        {gapSet && (
          <>
            <span className="text-muted-foreground">Inside the gap</span>
            <select
              aria-label="Inside the gap"
              className={cn(SELECT, "h-7 w-48 max-w-full text-xs")}
              value={draft.overlap}
              onChange={(e) => set("overlap", e.target.value as SentenceDraft["overlap"])}
            >
              <option value="coalesce">fold into the last wake</option>
              <option value="skip">skip the fire</option>
              {draft.overlap === "queue" && (
                <option value="queue">fold into the last wake (overlap: queue)</option>
              )}
            </select>
          </>
        )}
      </fieldset>
      {problem && (
        <p role="alert" className="mt-2 text-destructive">
          {problem}
        </p>
      )}
    </details>
  );
}

/** What the mode word does, in the product's words (design §5.2). */
function ModeHint({
  mode,
  name,
  machine,
  any,
  wakeAllowed,
  isDefault,
  migrated,
  codex,
}: {
  mode: SentenceDraft["mode"];
  name: string;
  machine: string;
  any: boolean;
  wakeAllowed: boolean;
  isDefault: boolean;
  migrated: boolean;
  codex: boolean;
}) {
  const suffix = isDefault ? ` (default for ${name})` : "";
  if (migrated && mode === "fresh") {
    return (
      <p
        id="routine-mode-hint"
        data-testid="routine-mode-hint"
        aria-live="polite"
        className="text-xs text-muted-foreground"
      >
        <span>
          Set to start a new {name} each run when this routine moved under {name}, so it behaves as
          before. Choose wake to reuse {name}&apos;s running session instead.
        </span>
      </p>
    );
  }
  const text = !wakeAllowed
    ? "Wake needs a saved agent. Use template to make one from Claude."
    : mode === "wake"
      ? any
        ? `Goes into ${name}'s session on whichever connected machine has one, preferring an idle one. If none is running, a new session starts on This Mac.${suffix}`
        : `If ${name} is running on ${machine}, this goes into that session when it's idle. If not, a new session starts with it.${suffix}`
      : `Starts a new ${name} session every time, even if one is already running.${suffix}`;
  return (
    <>
      <p
        id="routine-mode-hint"
        data-testid="routine-mode-hint"
        aria-live="polite"
        className="text-xs text-muted-foreground"
      >
        {text}
      </p>
      {codex && mode === "wake" && (
        <p data-testid="routine-mode-warning" className="text-xs text-warning-ink">
          CodeSense can&apos;t yet tell when Codex is idle, so wakes for {name} may come back
          undelivered.
        </p>
      )}
    </>
  );
}

/** The run contract the task carries: a wake pastes it once into the live session; a
 *  fresh run makes it the whole prompt. */
function TaskHelp({ mode, name }: { mode: SentenceDraft["mode"]; name: string }) {
  if (mode === "wake") {
    return (
      <p data-testid="routine-task-help" className="text-xs text-muted-foreground">
        CodeSense pastes this into {name}&apos;s session once, when it is idle, after a line naming
        this routine. Only <code>{"{{date}}"}</code> and <code>{"{{timestamp}}"}</code> are filled
        in. For a result, tell {name} to call <code>routine_done</code>.
      </p>
    );
  }
  return (
    <p data-testid="routine-task-help" className="text-xs text-muted-foreground">
      The task is the agent's whole prompt; CodeSense adds nothing to it. A run ends when the agent
      exits, calls <code>signal_done</code>, or hits its max runtime. For a result or a question on
      the run, tell the agent to call <code>signal_done</code>; otherwise the run simply ends. A
      visible agent stays open in its tab, so without <code>signal_done</code> its run lasts until
      you close the tab or it hits its max runtime. It can call <code>routine_context</code> for
      details. Routines never deliver results: say in the task where to post or file them.
    </p>
  );
}

/** The schedule in words and its next five fires, read live as the slot changes. */
function SchedulePreview({ when, tz }: { when: string; tz: string }) {
  const [read, setRead] = useState<{
    input: string;
    preview: RoutineSchedulePreview | null;
    error: string | null;
  } | null>(null);
  const seq = useRef(0);
  const input = `${when.trim()}|${tz.trim()}`;
  useEffect(() => {
    const id = ++seq.current;
    if (!when.trim()) return;
    const timer = setTimeout(() => {
      routinePreviewSchedule(when.trim(), tz.trim() || undefined).then(
        (preview) => {
          if (seq.current === id) setRead({ input, preview, error: null });
        },
        (e: unknown) => {
          if (seq.current === id) {
            setRead({ input, preview: null, error: e instanceof Error ? e.message : String(e) });
          }
        },
      );
    }, PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [when, tz, input]);

  // Only a read of what the slot holds now is shown; an older one is stale.
  const current = when.trim() && read?.input === input ? read : null;
  const preview = current?.preview ?? null;
  const error = current?.error ?? null;

  if (error) {
    return (
      <p data-testid="routine-preview-error" className="text-xs text-destructive">
        {error}
      </p>
    );
  }
  if (!preview) {
    return (
      <p className="text-xs text-muted-foreground">
        Type a schedule: 5m, 2h, hour, day 09:00, weekday 09:00, mon 17:00, or a cron.
      </p>
    );
  }
  return (
    <div data-testid="routine-next-fires" className="text-xs text-muted-foreground">
      <span className="text-foreground">
        {scheduleLabel({ schedule_in_words: preview.in_words })}
      </span>
      {" · next: "}
      {preview.next_fires.map((at) => formatFire(at, preview.tz)).join("; ")} ({preview.tz})
    </div>
  );
}

/** A fire time as the routine reads it: in its own zone, with the date, so
 *  weekly fires stay apart. */
function formatFire(at: number, tz: string): string {
  const options: Intl.DateTimeFormatOptions = {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  };
  try {
    return new Date(at).toLocaleString(undefined, { ...options, timeZone: tz });
  } catch {
    return new Date(at).toLocaleString(undefined, options);
  }
}

// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { Bot, Loader2 } from "lucide-react";

import { InlineAlert } from "@/components/ui/inline-alert";
import { openAgents } from "@/features/agents/agents-virtual-tab";
import { Switch } from "@/components/ui/switch";
import { AppButton as Button } from "@/shared/components/app-button";
import { ONLY_ME, WhoCanUseItField, resolveAudience } from "@/shared/components/who-can-use-it";
import { useOrgStore } from "@/shared/stores/org-store";
import { cn } from "@/lib/utils";
import { cancelSessionTimer, type AgentRow, type RoutineRow } from "@/tauri/commands";
import { toast } from "sonner";

import { agentKeyFor, findAgent, routineSpace, type RoutineSpace } from "../lib/routine-agent-ref";
import {
  createForm,
  shownMode,
  draftFromDef,
  editForm,
  emptyDraft,
  metadataProblem,
  limitsProblem,
  machineProblem,
  migratedUnsaved,
  scheduleCount,
  type SentenceDraft,
} from "../lib/routine-sentence";
import { useRoutinesStore, type RoutineCreateSeed } from "../stores/routines-store";
import { RoutineSentenceFields } from "./routine-sentence-form";
import { machineTeamOf, useRoutineMachines } from "../lib/use-routine-machines";

/** Whether the draft has a machine, limit or trigger problem that blocks saving. */
function useMachineProblem(
  draft: SentenceDraft,
  team: boolean,
  fixedAgent: AgentRow | undefined,
  space: RoutineSpace,
): boolean {
  const rows = useRoutinesStore((s) => s.rows);
  const agents = useRoutinesStore((s) => s.agents);
  const agent = fixedAgent ?? findAgent(agents ?? [], draft.agent, space);
  const { known } = useRoutineMachines(machineTeamOf(agent), agent?.scope === "personal");
  return (
    machineProblem(draft, shownMode(draft, agent), known, team) !== null ||
    (draft.action !== "command" && limitsProblem(draft) !== null) ||
    metadataProblem(draft) !== null ||
    (draft.action === "command" && (!team || !draft.command[0]?.trim())) ||
    (!!draft.after &&
      (!draft.after.routine ||
        (rows !== null &&
          !rows.some(
            (r) =>
              r.def.id === draft.after?.routine &&
              (space.scope !== "team" || r.key.startsWith(space.space + "/")),
          ))))
  );
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const EMPTY_DRAFT = emptyDraft();

/** A New routine saved before an agent was picked. */
const NO_AGENT = "pick an agent";

/** Report whether this form holds an unsaved edit, so leaving it asks first;
 *  an unmounted form holds none. */
function useReportDirty(dirty: boolean) {
  const setFormDirty = useRoutinesStore((s) => s.setFormDirty);
  useEffect(() => setFormDirty(dirty), [dirty, setFormDirty]);
  useEffect(() => () => setFormDirty(false), [setFormDirty]);
}

/**
 * A routine's Overview: its sentence, editable by anyone who can see it. A save
 * is a local edit to the context repo, shared by a git push. It is made
 * against the revision the form was loaded from; if someone (or an agent)
 * changed it since, the save writes nothing, the edit stays in the form, and
 * Reload brings the newer routine in.
 */
export function RoutineOverview({ row, fixedAgent }: { row: RoutineRow; fixedAgent?: AgentRow }) {
  const saveRoutine = useRoutinesStore((s) => s.saveRoutine);
  const warnings = useRoutinesStore((s) => s.saveWarnings[row.key]);
  const loadList = useRoutinesStore((s) => s.loadList);
  const [loaded, setLoaded] = useState(() => ({
    revision: row.def.revision,
    base: draftFromDef(row.def),
  }));
  const [draft, setDraft] = useState<SentenceDraft>(loaded.base);
  const [conflict, setConflict] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [commandBlocked, setCommandBlocked] = useState(false);
  const changes = editForm(loaded.base, draft);
  const triggerRemoved =
    !!(loaded.base.when || loaded.base.events?.length || loaded.base.after) &&
    !(draft.when.trim() || draft.events?.length || draft.after);
  const machineBlocked = useMachineProblem(
    draft,
    row.scope === "team",
    fixedAgent,
    routineSpace(row.scope, row.key),
  );

  const blocked =
    machineBlocked || triggerRemoved || (draft.action === "command" && commandBlocked);

  // Tell the tab an edit is unsaved, so leaving this routine asks first.
  useReportDirty(changes !== null);

  // A newer revision arrived while nothing was being edited: show it. With an
  // edit in progress the form keeps it; Save then reports the conflict.
  const unchanged = changes === null;
  useEffect(() => {
    if (row.def.revision === loaded.revision || !unchanged) return;
    const base = draftFromDef(row.def);
    setLoaded({ revision: row.def.revision, base });
    setDraft(base);
    // Re-checked when the routine moves on and when an edit is undone back to
    // nothing; the draft itself is not a dependency, or typing would reset it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [row.def.revision, unchanged]);

  const reload = () => {
    const base = draftFromDef(row.def);
    setLoaded({ revision: row.def.revision, base });
    setDraft(base);
    setConflict(null);
    setError(null);
    void loadList();
  };

  const save = async () => {
    if (!changes || saving || blocked) return;
    setSaving(true);
    setError(null);
    try {
      const reply = await saveRoutine(changes, {
        key: row.key,
        baseRevision: loaded.revision,
      });
      if (reply.outcome === "conflict") {
        setConflict(reply.current_revision);
      } else {
        setConflict(null);
        // The save re-read the list: start the next edit from what was written.
        const fresh = useRoutinesStore.getState().rows?.find((r) => r.key === row.key)?.def;
        const base = fresh?.revision === reply.revision ? draftFromDef(fresh) : draft;
        setLoaded({ revision: reply.revision, base });
        setDraft(base);
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section aria-label="Overview" data-testid="routine-overview" className="flex flex-col gap-3">
      <RoutineSentenceFields
        saveError={error}
        onCommandBlocked={setCommandBlocked}
        draft={draft}
        onChange={setDraft}
        whenLocked={whenLocked(row)}
        routine={routineSpace(row.scope, row.key)}
        savedAgent={{ ref: loaded.base.agent, display: row.agent_display }}
        fixedAgent={fixedAgent}
        migrated={migratedUnsaved(row.def)}
        routineKey={row.key}
        armedMachine={row.def.armed_machine}
      />
      {draft.action !== "command" && warnings && warnings.length > 0 && (
        <div
          data-testid="routine-save-warnings"
          className="flex flex-col gap-0.5 text-xs text-warning-ink"
        >
          <ul className="flex flex-col gap-0.5">
            {warnings.map((w) => (
              <li key={`${w.code}:${w.message}`}>{w.message}</li>
            ))}
          </ul>
          {row.agent_display.resolvable && (
            <button
              type="button"
              data-testid="routine-warning-open-agent"
              className="self-start underline decoration-dotted underline-offset-2 hover:text-foreground"
              onClick={() =>
                openAgents(agentKeyFor(row.def.agent ?? "", row.key), row.agent_display.archived)
              }
            >
              Open the agent
            </button>
          )}
        </div>
      )}
      {conflict !== null && (
        <InlineAlert
          data-testid="routine-conflict"
          severity="warning"
          title="Changed elsewhere — reload"
          action={{ label: "Reload", onClick: reload }}
        >
          This routine is now at revision {conflict}, so nothing was saved. Your edit is still here;
          Reload replaces it with the newer version.
        </InlineAlert>
      )}
      {error && (
        <p data-testid="routine-save-error" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex items-center gap-2">
        <Button size="sm" disabled={!changes || saving || blocked} onClick={() => void save()}>
          {saving && <Loader2 className="animate-spin" />}
          Save changes
        </Button>
        {changes && (
          <Button size="sm" variant="ghost" disabled={saving} onClick={reload}>
            Discard
          </Button>
        )}
        <span className="text-xs text-muted-foreground">Revision {loaded.revision}</span>
      </div>
    </section>
  );
}

/** With several schedules the slot shows only the first, and a new one would
 *  replace them all: those are changed by asking an agent. */
function whenLocked(row: RoutineRow): string | null {
  const count = scheduleCount(row.def);
  if (count < 2) return null;
  return `This routine has ${count} schedules; the first is shown. Ask an agent to change them, so none is dropped.`;
}

/** The routine kept from a timer is saved: the timer still runs on its tab, so
 *  ask whether to cancel it or keep both (SW-AGR-6 "Keep this…"). */
function offerToCancelTimer(name: string, timer: NonNullable<RoutineCreateSeed["timer"]>) {
  toast(`${name || timer.name} is now a routine. Cancel the timer?`, {
    duration: 15_000,
    action: {
      label: "Cancel timer",
      onClick: () => {
        const machine =
          timer.machineId && timer.machineId !== "local" ? timer.machineId : undefined;
        cancelSessionTimer(timer.sessionId, timer.timerId, machine).catch((e: unknown) =>
          toast.error(`Couldn't cancel the timer: ${errorText(e)}`),
        );
      },
    },
    cancel: { label: "Keep both", onClick: () => undefined },
  });
}

/** The New routine form: who can use it (the open team by default, or only you), the
 *  same sentence, an enabled toggle (off saves a draft), and a way to describe the
 *  routine to an agent instead. */
export function RoutineCreateForm({
  fixedAgent,
  onCancel,
}: {
  /** The agent the new routine belongs to (SW-AGR-5): fixed, and its space is where the
   *  routine lives. A starter has no space of its own, so who can use it is still asked. */
  fixedAgent?: AgentRow;
  onCancel?: () => void;
} = {}) {
  const saveRoutine = useRoutinesStore((s) => s.saveRoutine);
  const cancelCreate = useRoutinesStore((s) => s.cancelCreate);
  const cancel = onCancel ?? cancelCreate;
  const describe = useRoutinesStore((s) => s.describeToAgent);
  const describing = useRoutinesStore((s) => s.pending.describe === true);
  // "Keep this…" on a timer opens the form filled in from it (read once: a
  // refill remounts the form), its mode and machine in their slots.
  const [seed] = useState(() => useRoutinesStore.getState().createSeed);
  // An agent that launches in a tab runs its routines visibly too, so its default
  // mode is wake (design §4.6).
  const fixedBase = fixedAgent
    ? { ...emptyDraft(), agent: fixedAgent.key, visible: fixedAgent.launch === "foreground" }
    : null;
  const [draft, setDraft] = useState<SentenceDraft>(() => ({
    ...(fixedBase ?? emptyDraft()),
    ...seed?.draft,
    ...(seed?.mode ? { mode: seed.mode } : {}),
    ...(seed?.machineId ? { machine: seed.machineId } : {}),
  }));
  const [enabled, setEnabled] = useState(true);
  const orgSlug = useOrgStore((s) => s.activeOrgSlug);
  const teamName = useOrgStore((s) => s.activeTeamName);
  const teams = orgSlug && teamName ? [{ value: `${orgSlug}/${teamName}`, label: teamName }] : [];
  const [choice, setChoice] = useState<string | null>(
    seed?.audience === "personal" ? ONLY_ME : null,
  );
  const who = resolveAudience(choice, teams);
  // A saved agent decides where its routine lives: its team, or your own space.
  const ownSpace = fixedAgent && fixedAgent.scope !== "builtin";
  const forTeam = ownSpace
    ? fixedAgent.scope === "team"
      ? { org: fixedAgent.org, team: fixedAgent.team }
      : null
    : who !== ONLY_ME && orgSlug && teamName
      ? { org: orgSlug, team: teamName }
      : null;
  const space: RoutineSpace = forTeam
    ? { scope: "team", space: `${forTeam.org}/${forTeam.team}` }
    : { scope: "personal" };
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const agents = useRoutinesStore((s) => s.agents);
  // The mode the form shows is the mode saved: the pick, else the agent's default.
  const mode = shownMode(draft, fixedAgent ?? findAgent(agents ?? [], draft.agent, space));
  const blocked = useMachineProblem(draft, forTeam !== null, fixedAgent, space);
  const [commandBlocked, setCommandBlocked] = useState(false);
  const ready =
    draft.name.trim() !== "" &&
    (draft.action === "command"
      ? !!draft.command[0]?.trim() && !commandBlocked
      : draft.task.trim() !== "") &&
    (draft.when.trim() !== "" || !!draft.events?.length || !!draft.after) &&
    !blocked;
  useReportDirty(editForm(fixedBase ?? EMPTY_DRAFT, draft) !== null || !enabled || choice !== null);

  const save = async () => {
    if (!ready || saving) return;
    if (draft.action !== "command" && !draft.agent.trim()) {
      setError(NO_AGENT);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const base = forTeam
        ? { ...createForm(draft, mode), runsOn: draft.runsOn }
        : createForm(draft, mode);
      // "Keep this…" put the timer's mode and machine in their slots: the form
      // sends what it shows.
      const form = base;
      const reply = await saveRoutine(form, undefined, enabled, forTeam ?? undefined);
      // A draft never fires, so the timer is still the thing that runs.
      if (reply.outcome === "saved" && seed?.timer && enabled)
        offerToCancelTimer(form.name ?? "", seed.timer);
    } catch (e) {
      setError(errorText(e));
      setSaving(false);
    }
  };

  return (
    <section
      aria-label="New routine"
      data-testid="routine-create"
      className={cn("flex flex-col gap-4", !fixedAgent && "p-4")}
    >
      {!fixedAgent && (
        <header className="flex items-center gap-2">
          <h2 className="text-base font-semibold">New routine</h2>
          <Button
            className="ml-auto"
            size="sm"
            variant="outline"
            disabled={describing}
            onClick={() => void describe()}
          >
            {describing ? <Loader2 className="animate-spin" /> : <Bot />}
            Describe it to an agent
          </Button>
        </header>
      )}
      {!ownSpace && (
        <WhoCanUseItField
          value={who}
          onChange={(next) => {
            setChoice(next);
            // The agent picker offers the new space's agents; a pick from the old one may
            // not apply. A fixed agent stays.
            setDraft((d) => ({ ...d, agent: fixedAgent?.key ?? "" }));
          }}
          teams={teams}
        />
      )}
      <RoutineSentenceFields
        saveError={error}
        onCommandBlocked={setCommandBlocked}
        draft={draft}
        routine={space}
        fixedAgent={fixedAgent}
        onChange={(next) => {
          setDraft(next);
          if (next.agent.trim()) setError((e) => (e === NO_AGENT ? null : e));
        }}
      />
      {seed?.mode === "wake" && mode === "wake" && (
        <p data-testid="routine-seed-mode" className="text-xs text-muted-foreground">
          Kept from the timer “{seed.timer?.name ?? draft.name}”: each run{" "}
          <span className="font-medium text-foreground">wakes</span> the agent in its running
          session on {seed.machineLabel ?? "This Mac"} when it's idle.
        </p>
      )}
      {error && (
        <p data-testid="routine-save-error" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex items-center gap-3">
        <label className="inline-flex items-center gap-2 text-sm">
          <Switch size="sm" aria-label="Enabled" checked={enabled} onCheckedChange={setEnabled} />
          {enabled ? "Enabled" : "Draft — saved, never fires until enabled"}
        </label>
        <Button
          size="sm"
          className="ml-auto"
          disabled={!ready || saving}
          onClick={() => void save()}
        >
          {saving && <Loader2 className="animate-spin" />}
          {fixedAgent ? "Save routine" : "Create routine"}
        </Button>
        <Button
          size="sm"
          variant={fixedAgent ? "outline" : "ghost"}
          disabled={saving}
          onClick={cancel}
        >
          Cancel
        </Button>
      </div>
    </section>
  );
}

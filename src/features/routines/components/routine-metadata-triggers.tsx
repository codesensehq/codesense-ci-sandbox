// SPDX-License-Identifier: MIT
import { useEffect, useId, useState } from "react";
import { Plus, X } from "lucide-react";
import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import { useOrgStore } from "@/shared/stores/org-store";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import type {
  ContextKind,
  ContextQueryResult,
  ContextWhere,
} from "@/shared/lib/context-query/types";
import { queryContext, type RoutineMetadataCondition } from "@/tauri/commands";
import type { RoutineSpace } from "../lib/routine-agent-ref";
import { FIELD, SELECT, PREVIEW_DEBOUNCE_MS } from "../lib/routine-form-controls";
import { eventTriggerWords, metadataProblem, type SentenceDraft } from "../lib/routine-sentence";

import { useRoutinesStore } from "../stores/routines-store";
import { afterWords } from "../lib/routine-sentence";

const KINDS: { value: ContextKind | ""; label: string }[] = [
  { value: "story", label: "a story" },
  { value: "bug", label: "a bug" },
  { value: "spike", label: "a spike" },
  { value: "epic", label: "an epic" },
  { value: "doc", label: "a doc" },
  { value: "dashboard", label: "a dashboard" },
  { value: "skill", label: "a skill" },
  { value: "", label: "any document" },
];

export function RoutineMetadataTriggers({
  draft,
  onChange,
  routine,
  org,
  armedMachine,
  routineKey,
  chainError,
}: {
  draft: SentenceDraft;
  onChange: (draft: SentenceDraft) => void;
  routine: RoutineSpace;
  org: string;
  armedMachine?: string | null;
  routineKey?: string;
  chainError?: string | null;
}) {
  const rows = useRoutinesStore((s) => s.rows);
  const selectRoutine = useRoutinesStore((s) => s.select);
  const choices = (rows ?? []).filter((r) =>
    routine.scope === "team"
      ? r.key.startsWith(routine.space + "/")
      : r.scope === "personal" && r.key.includes(`/${org}/`),
  );
  const names = new Map(choices.map((r) => [r.def.id, r.def.name]));
  const after = draft.after;
  const missing = rows !== null && !!after?.routine && !names.has(after.routine);
  const loops = (id: string) => {
    const seen = new Set<string>();
    let candidate = choices.find((r) => r.def.id === id);
    while (candidate && !seen.has(candidate.def.id)) {
      if (candidate.key === routineKey) return true;
      seen.add(candidate.def.id);
      candidate = choices.find((r) => r.def.id === candidate?.def.after?.routine);
    }
    return false;
  };
  const [focus, setFocus] = useState<number | null>(null);
  const events = draft.events ?? [];
  const team =
    routine.scope === "team" ? routine.space.slice(routine.space.lastIndexOf("/") + 1) : null;
  const orgs = useOrgStore((s) => s.orgs);
  const contextOrg = orgs.find((o) => o.slug === org);
  const pageTeam = team ?? contextOrg?.teams[0]?.name;
  const page =
    contextOrg && pageTeam ? pathToTeamContentDirSync(contextOrg.org_root, pageTeam) : "";
  const disabled = armedMachine
    ? `Routines armed on ${armedMachine} run on schedules only.`
    : draft.runsOn === "each_mac"
      ? "Routines on each teammate's Mac support schedules only for these triggers."
      : null;
  const set = (index: number, condition: RoutineMetadataCondition) =>
    onChange({
      ...draft,
      events: events.map((event, i) => (i === index ? { ...event, metadata: condition } : event)),
    });
  return (
    <section aria-label="Triggers" className="flex min-w-0 flex-col gap-2">
      <h3 className="text-xs font-medium">Triggers</h3>
      {events.map((event, i) =>
        event.kind === "metadata_change" && event.metadata ? (
          <MetadataRow
            key={i}
            condition={event.metadata}
            index={i}
            page={page}
            scope={{
              orgs: [org],
              layers: [team ? "team" : "private"],
              ...(team ? { teams: [team] } : {}),
            }}
            autoFocus={focus === i}
            onChange={(next) => set(i, next)}
            duplicate={events
              .slice(0, i)
              .findIndex(
                (e) =>
                  e.kind === event.kind &&
                  JSON.stringify(e.metadata) === JSON.stringify(event.metadata),
              )}
            onRemove={() => {
              setFocus(null);
              onChange({ ...draft, events: events.filter((_, n) => n !== i) });
            }}
          />
        ) : (
          <p key={i} data-testid="routine-event-triggers" className="text-xs text-muted-foreground">
            {eventTriggerWords(event)} · ask an agent to change this
          </p>
        ),
      )}
      {after && (
        <div className="flex flex-col gap-1">
          <div className="flex flex-wrap items-center gap-2 text-xs">
            <span>after</span>
            <select
              aria-label="After routine"
              className={SELECT}
              value={after.routine}
              aria-invalid={missing || !!chainError || undefined}
              aria-describedby={
                missing || chainError ? "routine-after-error" : "routine-after-words"
              }
              onChange={(e) => onChange({ ...draft, after: { ...after, routine: e.target.value } })}
            >
              <option value="" disabled>
                Pick a routine
              </option>
              {missing && after.routine && (
                <option value={after.routine}>{after.routine} (gone)</option>
              )}
              {choices.map((r) => (
                <option
                  key={r.key}
                  value={r.def.id}
                  disabled={loops(r.def.id) || !!r.def.armed_machine?.trim()}
                >
                  {r.def.name}
                  {r.def.armed_machine?.trim()
                    ? ` (armed on ${r.def.armed_machine})`
                    : r.key === routineKey
                      ? " (this routine)"
                      : loops(r.def.id)
                        ? " (runs after this one)"
                        : ""}
                </option>
              ))}
            </select>
            <select
              aria-label="After ending"
              className={SELECT}
              value={after.on}
              onChange={(e) =>
                onChange({
                  ...draft,
                  after: { ...after, on: e.target.value as "succeeded" | "finished" },
                })
              }
            >
              <option value="succeeded">succeeds</option>
              <option value="finished">finishes</option>
            </select>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              aria-label="Remove after trigger"
              onClick={() => onChange({ ...draft, after: undefined })}
            >
              <X />
            </Button>
          </div>
          {(missing || chainError) && (
            <p id="routine-after-error" role="alert" className="text-xs text-destructive">
              {chainError ||
                `${after.routine || "The upstream routine"} no longer exists, so this never fires. Pick another routine or remove this trigger.`}
              {chainError?.includes("loop") && choices.find((r) => r.def.id === after.routine) && (
                <button
                  type="button"
                  className="ml-1 underline"
                  onClick={() =>
                    void selectRoutine(choices.find((r) => r.def.id === after.routine)!.key)
                  }
                >
                  {names.get(after.routine)}
                </button>
              )}
            </p>
          )}
          {after.routine && (
            <p
              id="routine-after-words"
              aria-live="polite"
              className="text-xs text-muted-foreground"
            >
              {after.on === "succeeded"
                ? `${afterWords(after, names).replace(/^after/, "After")}.`
                : `After ${names.get(after.routine) ?? after.routine} finishes: succeeds, errors, times out, needs you, or ends. Not when it is cancelled or skipped.`}
            </p>
          )}
        </div>
      )}
      <div className="flex flex-wrap gap-1">
        {!after && (
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="self-start"
            disabled={!!disabled}
            title={disabled ?? undefined}
            onClick={() => onChange({ ...draft, after: { routine: "", on: "succeeded" } })}
          >
            <Plus />
            After a routine
          </Button>
        )}
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="self-start"
          disabled={!!disabled}
          aria-describedby={disabled ? "metadata-add-disabled" : undefined}
          onClick={() => {
            setFocus(events.length);
            onChange({
              ...draft,
              events: [
                ...events,
                {
                  kind: "metadata_change",
                  metadata: {
                    selection: { where: { kinds: ["story"] } },
                    target: { key: "", op: "eq", value: "" },
                  },
                },
              ],
            });
          }}
        >
          <Plus />
          Add a metadata trigger
        </Button>
      </div>
      {disabled && (
        <p id="metadata-add-disabled" className="text-xs text-muted-foreground">
          {disabled}
        </p>
      )}
      {events.length > 0 && (
        <p className="text-xs text-muted-foreground">
          {team
            ? `Watches the ${team} team's synced files, whatever team is on screen.`
            : "Watches your personal files, whatever team is on screen."}
        </p>
      )}
    </section>
  );
}

function MetadataRow({
  condition,
  index,
  scope,
  page,
  autoFocus,
  duplicate,
  onChange,
  onRemove,
}: {
  condition: RoutineMetadataCondition;
  index: number;
  scope: ContextWhere;
  page: string;
  autoFocus: boolean;
  duplicate: number;
  onChange: (condition: RoutineMetadataCondition) => void;
  onRemove: () => void;
}) {
  const id = useId();
  const field = condition.target.key;
  const target = String(condition.target.value ?? "");
  const previous = String(condition.previous?.value ?? "");
  const folder = condition.selection.where?.pathPrefix ?? "";
  const kind = condition.selection.where?.kinds?.[0] ?? "";
  const noun = kind || "document";
  const plural = noun === "story" ? "stories" : `${noun}s`;
  const [read, setRead] = useState<{
    input: string;
    sample: ContextQueryResult | null;
    values: ContextQueryResult | null;
    count: number | null;
  } | null>(null);
  const input = JSON.stringify({ condition, scope, page });
  useEffect(() => {
    let active = true;
    const timer = setTimeout(async () => {
      // A missing page cannot establish private visibility, so use honest fallback copy.
      if (!scope.orgs || (scope.layers?.[0] === "private" && !page)) return;
      const query = {
        ...condition.selection,
        where: { ...condition.selection.where, ...scope },
        cursor: undefined,
        limit: 200,
      };
      const results = await Promise.allSettled([
        queryContext({ ...query, select: undefined, groupBy: undefined }, page),
        field
          ? queryContext({ ...query, select: [field], groupBy: field }, page)
          : Promise.resolve(null),
        field &&
        target &&
        !condition.selection.filters?.length &&
        !Object.keys(condition.selection.match ?? {}).length
          ? queryContext(
              { ...query, filters: [condition.target], limit: 0, select: [], groupBy: undefined },
              page,
            )
          : Promise.resolve(null),
      ]);
      if (active)
        setRead({
          input,
          sample:
            results[0].status === "fulfilled" && !results[0].value?.indexing
              ? results[0].value
              : null,
          values:
            results[1].status === "fulfilled" && !results[1].value?.indexing
              ? results[1].value
              : null,
          count:
            results[2].status === "fulfilled" && !results[2].value?.indexing
              ? (results[2].value?.total ?? null)
              : null,
        });
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [condition, field, input, page, scope, target]);
  const current = read?.input === input ? read : null;
  const list =
    condition.list ||
    current?.values?.rows.some((r) => Object.values(r.fields).some(Array.isArray)) ||
    false;
  useEffect(() => {
    if (list && (!condition.list || condition.previous))
      onChange({ ...condition, list: true, previous: undefined });
  }, [list, condition, onChange]);
  const keys = new Map<string, number>();
  const folders = new Set<string>();
  for (const row of current?.sample?.rows ?? []) {
    for (const key of Object.keys(row.fields)) keys.set(key, (keys.get(key) ?? 0) + 1);
    const parts = row.relPath.split("/");
    parts.pop();
    while (parts.length) {
      folders.add(`${parts.join("/")}/`);
      parts.pop();
    }
  }
  const problem = metadataProblem({
    events: [{ kind: "metadata_change", metadata: condition }],
    runsOn: "one_mac",
  });
  const invalid = !!problem && !!field && !!target;
  const words = eventTriggerWords({ kind: "metadata_change", metadata: { ...condition, list } });
  const warning =
    duplicate >= 0
      ? `Same as trigger ${duplicate + 1}.`
      : folder && current?.sample?.total === 0
        ? `Nothing under ${folder} yet. Check the folder, or keep it for files to come.`
        : field && target && current?.values?.groups?.every((g) => g.value === null)
          ? `No ${noun} here has ${field} yet. This fires once one is set to ${target}.`
          : null;
  const baseline =
    current?.count === null || current?.count === undefined
      ? `${plural.charAt(0).toUpperCase() + plural.slice(1)} that already have ${field} ${target} don't count; the next change to ${target} does.`
      : current.count === 0
        ? `No ${noun} has ${field} ${target} now.`
        : `${current.count} ${current.count === 1 ? noun : plural} already ${current.count === 1 ? "has" : "have"} ${field} ${target}. They count only after ${list ? "it's removed and added again" : `${field} leaves ${target} and comes back`}.`;
  const describe = `${id}-message`;
  const inputProps = {
    "aria-describedby": describe,
    "aria-invalid": invalid || undefined,
  } as const;
  return (
    <div
      role="group"
      aria-label={`Trigger ${index + 1}: ${words}`}
      className="flex min-w-0 flex-col gap-1"
    >
      <div className="flex items-start gap-1">
        <div className="flex min-w-0 flex-1 flex-col gap-1.5">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span>when</span>
            <select
              aria-label="Kind"
              className={SELECT}
              value={kind}
              {...inputProps}
              onChange={(e) =>
                onChange({
                  ...condition,
                  selection: {
                    ...condition.selection,
                    where: {
                      ...condition.selection.where,
                      kinds: e.target.value ? [e.target.value as ContextKind] : undefined,
                    },
                  },
                })
              }
            >
              {KINDS.map((k) => (
                <option key={k.value} value={k.value}>
                  {k.label}
                </option>
              ))}
            </select>
            <span>under</span>
            <Input
              aria-label="Folder"
              className={cn("w-44 max-w-full", FIELD)}
              placeholder="any folder"
              list={`${id}-folders`}
              value={folder}
              {...inputProps}
              onChange={(e) =>
                onChange({
                  ...condition,
                  selection: {
                    ...condition.selection,
                    where: {
                      ...condition.selection.where,
                      pathPrefix: e.target.value || undefined,
                    },
                  },
                })
              }
            />
          </div>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span>changes</span>
            <Input
              autoFocus={autoFocus}
              aria-label="Field"
              className={cn("w-36 max-w-full", FIELD)}
              placeholder="field"
              list={`${id}-keys`}
              value={field}
              {...inputProps}
              onChange={(e) =>
                onChange({
                  ...condition,
                  list: undefined,
                  target: { ...condition.target, key: e.target.value },
                  previous: condition.previous
                    ? { ...condition.previous, key: e.target.value }
                    : undefined,
                })
              }
            />
            <span>from</span>
            <Input
              aria-label="From value"
              className={cn("w-32 max-w-full", FIELD)}
              placeholder="any value"
              list={`${id}-values`}
              value={list ? "" : previous}
              disabled={list}
              {...inputProps}
              onChange={(e) =>
                onChange({
                  ...condition,
                  previous: e.target.value
                    ? { key: field, op: "eq", value: e.target.value }
                    : undefined,
                })
              }
            />
            <span>to</span>
            <Input
              aria-label="To value"
              className={cn("w-32 max-w-full", FIELD)}
              placeholder="value"
              list={`${id}-values`}
              value={target}
              {...inputProps}
              onChange={(e) =>
                onChange({ ...condition, target: { ...condition.target, value: e.target.value } })
              }
            />
          </div>
        </div>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          aria-label={`Remove trigger ${index + 1}`}
          onClick={onRemove}
        >
          <X />
        </Button>
      </div>
      <div id={describe} className="text-xs text-muted-foreground">
        {problem ? (
          <p
            role={invalid ? "alert" : undefined}
            className={invalid ? "text-destructive" : undefined}
          >
            {problem}
          </p>
        ) : (
          <>
            <p aria-live="polite" className="text-foreground">
              {words.charAt(0).toUpperCase() + words.slice(1)}.
            </p>
            <p>{baseline}</p>
            {list && <p>From doesn&apos;t apply to lists.</p>}
          </>
        )}
        {warning && <p className="text-warning-ink">{warning}</p>}
      </div>
      <datalist id={`${id}-keys`}>
        {[...keys]
          .sort((a, b) => b[1] - a[1])
          .map(([key]) => (
            <option key={key} value={key} />
          ))}
      </datalist>
      <datalist id={`${id}-folders`}>
        {[...folders].sort().map((path) => (
          <option key={path} value={path} />
        ))}
      </datalist>
      <datalist id={`${id}-values`}>
        {(current?.values?.groups ?? [])
          .filter((g) => g.value !== null)
          .map((g) => (
            <option key={g.value} value={g.value!} label={String(g.count)} />
          ))}
      </datalist>
    </div>
  );
}

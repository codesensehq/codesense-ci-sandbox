// SPDX-License-Identifier: MIT
import { showAgent } from "./agents-tab-nav";
import { createContext, useContext, useId, useMemo, useState, type ReactNode } from "react";
import {
  Archive,
  ArchiveRestore,
  Copy,
  Ellipsis,
  Rocket,
  Trash2,
  TriangleAlert,
  User,
  Users,
  Wrench,
} from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { InlineAlert } from "@/components/ui/inline-alert";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { AppButton as Button } from "@/shared/components/app-button";
import { EmptyState } from "@/shared/components/empty-state";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { StateWord } from "./agent-state-word";
import { StatusMark } from "@/shared/components/status-mark";
import { PreviewBadge, PreviewNote, previewNoteRef } from "@/shared/components/preview-badge";
import { isPreviewRuntime } from "@/shared/lib/agent-runtimes";
import { AgentRowFace } from "@/features/notes/components/agent-row-face";
import { cn } from "@/lib/utils";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import type { AgentRow, RoutineRow } from "@/tauri/commands";

import { groupAgents, isBuiltin, type AgentTeamGroup } from "../lib/agent-display";
import {
  agentRowLineParts,
  agentRowStatus,
  STATE_MARK,
  type AgentRowLineParts,
} from "../lib/agent-row-state";
import type { RunningInstance } from "../lib/agent-running";
import { INK, NEEDS_WASH } from "../lib/agent-tones";
import { useAgentInstanceIndex } from "../lib/use-agent-instances";
import { chipOfRow, nextRunChip, routinesOf } from "../lib/agent-routines";
import { useActiveTeamKey } from "@/shared/lib/team-key";

import { openLaunchSheet } from "../lib/launch-sheet";
import { useAgentsStore } from "../stores/agents-store";

/** How often the next-run chips' times re-render. Display only: no backend read. */
const CLOCK_MS = 30_000;

/**
 * Every user-owned agent you can see: Mine (only you, kept in your scratch space) and one
 * group per synced team, each in a fixed order by name. The starters CodeSense ships are
 * templates, not agents — they live behind the New agent dialog, not here. Every row is
 * fully usable: anyone can launch, edit, archive, duplicate or delete any agent.
 */
export function AgentList({ onDelete }: { onDelete: (row: AgentRow) => void }) {
  const rows = useAgentsStore((s) => s.rows);
  const routineRows = useRoutinesStore((s) => s.rows);
  const now = useNowClock(CLOCK_MS);
  const instances = useAgentInstanceIndex();
  const byAgent = useMemo(() => {
    if (!routineRows) return null;
    const keys = new Set(routineRows.flatMap((r) => (r.agent_key ? [r.agent_key] : [])));
    return new Map([...keys].map((k) => [k, routinesOf(routineRows, k)] as const));
  }, [routineRows]);
  const routinesContext = useMemo(() => ({ byAgent, now, instances }), [byAgent, now, instances]);
  const listError = useAgentsStore((s) => s.listError);
  const includeArchived = useAgentsStore((s) => s.includeArchived);
  const setIncludeArchived = useAgentsStore((s) => s.setIncludeArchived);
  if (rows === null) {
    return listError ? (
      <InlineAlert
        severity="error"
        title="Couldn't load agents"
        error={listError}
        className="m-3"
        data-testid="agent-list-error"
      />
    ) : (
      <AgentListSkeleton />
    );
  }
  const { mine, teams } = groupAgents(rows);
  const empty = mine.length === 0 && teams.length === 0;
  const count = mine.length + teams.reduce((n, g) => n + g.rows.length, 0);
  return (
    <RoutinesByAgent.Provider value={routinesContext}>
      <div data-testid="agent-list" className="flex min-h-0 flex-1 flex-col">
        <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border/70 pr-3 pl-3.5 text-xs text-muted-foreground">
          <span className="tabular-nums">{count === 1 ? "1 agent" : `${count} agents`}</span>
          <label className="ml-auto inline-flex items-center gap-2 text-[13px] text-foreground">
            Archived
            <Switch
              size="sm"
              aria-label="Show archived agents"
              checked={includeArchived}
              onCheckedChange={setIncludeArchived}
            />
          </label>
        </div>
        {listError && (
          <InlineAlert
            severity="error"
            title="Couldn't refresh agents"
            error={listError}
            className="m-3 mb-0"
            data-testid="agent-list-error"
          />
        )}
        <div className="min-h-0 flex-1 overflow-y-auto pb-3">
          {empty ? (
            <EmptyState
              testId="agent-list-empty"
              icon={<Users />}
              title="No agents yet"
              body="Pick a template or start blank from the New agent button above."
            />
          ) : (
            <>
              <AgentGroup
                title="Mine"
                icon={<User />}
                rows={mine}
                empty="No agents just for you yet."
                onDelete={onDelete}
              />
              {teams.map((group) => (
                <TeamGroup key={group.id} group={group} onDelete={onDelete} />
              ))}
            </>
          )}
        </div>
      </div>
    </RoutinesByAgent.Provider>
  );
}

/** Rows' height while the list loads, so nothing jumps when it arrives. */
function AgentListSkeleton() {
  return (
    <div
      data-testid="agent-list-loading"
      aria-busy="true"
      aria-label="Loading agents"
      className="p-2"
    >
      {[0, 1, 2, 3, 4].map((i) => (
        <div key={i} className="flex h-11 items-center gap-3 px-2.5">
          <Skeleton round className="size-7" />
          <div className="grid flex-1 gap-1.5">
            <Skeleton className="h-3 w-28" />
            <Skeleton className="h-2.5 w-44" />
          </div>
        </div>
      ))}
    </div>
  );
}

function GroupHead({ icon, title, count }: { icon: ReactNode; title: string; count: number }) {
  return (
    <div className="sticky top-0 z-[1] mt-1.5 flex h-[30px] items-center gap-1.5 bg-background px-3.5 text-xs font-semibold text-muted-foreground [&_svg]:size-3">
      {icon}
      <h2 className="font-semibold text-foreground">{title}</h2>
      <span className="ml-auto font-medium tabular-nums">{count}</span>
    </div>
  );
}

function AgentGroup({
  title,
  icon,
  rows,
  empty,
  onDelete,
}: {
  title: string;
  icon: ReactNode;
  rows: AgentRow[];
  empty: string;
  onDelete: (row: AgentRow) => void;
}) {
  return (
    <section aria-label={title}>
      <GroupHead icon={icon} title={title} count={rows.length} />
      {rows.length === 0 ? (
        <p className="px-3.5 py-1 text-[13px] text-muted-foreground">{empty}</p>
      ) : (
        <AgentRows rows={rows} onDelete={onDelete} />
      )}
    </section>
  );
}

function TeamGroup({
  group,
  onDelete,
}: {
  group: AgentTeamGroup;
  onDelete: (row: AgentRow) => void;
}) {
  return (
    <section aria-label={group.label} data-testid="agent-team-group" data-team={group.id}>
      <GroupHead icon={<Users />} title={group.label} count={group.rows.length} />
      <AgentRows rows={group.rows} onDelete={onDelete} />
    </section>
  );
}

function AgentRows({ rows, onDelete }: { rows: AgentRow[]; onDelete: (row: AgentRow) => void }) {
  const { byAgent, now, instances } = useContext(RoutinesByAgent);
  return (
    <ul className="flex flex-col">
      {rows.map((row) => (
        <AgentListRow
          key={row.key}
          row={row}
          onDelete={onDelete}
          own={byAgent ? (byAgent.get(row.key) ?? []) : null}
          now={now}
          instances={instances.get(row.key) ?? NO_INSTANCES}
        />
      ))}
    </ul>
  );
}

/** Every agent's routines, grouped once per read, and the one clock the chips share. */
const RoutinesByAgent = createContext<{
  byAgent: ReadonlyMap<string, RoutineRow[]> | null;
  now: number;
  instances: ReadonlyMap<string, readonly RunningInstance[]>;
}>({ byAgent: null, now: 0, instances: new Map() });

const NO_INSTANCES: readonly RunningInstance[] = [];

function AgentListRow({
  row,
  onDelete,
  own,
  now,
  instances,
}: {
  row: AgentRow;
  onDelete: (row: AgentRow) => void;
  /** Its routines, once the list is read (grouped once for every row). */
  own: RoutineRow[] | null;
  now: number;
  instances: readonly RunningInstance[];
}) {
  const selected = useAgentsStore((s) => s.selectedKey === row.key);
  const inUse = useAgentsStore((s) => s.inUse[row.key]);
  const archiving = useAgentsStore(
    (s) => s.pending[`archive:${row.key}`] === true || s.pending[`unarchive:${row.key}`] === true,
  );
  const copying = useAgentsStore((s) => s.pending[`copy:${row.key}`] === true);
  const restoring = useAgentsStore((s) => s.pending[`restore-routines:${row.key}`] === true);
  const { archive, unarchive, copyToMine, restoreRoutines } = useAgentsStore.getState();
  // Archiving an agent archives its routines too: say which, and ask first.
  const [confirmArchive, setConfirmArchive] = useState(false);
  const [offerDismissed, setOfferDismissed] = useState(false);
  const ownRoutines = row.routines ?? [];
  // The routines its archive archived, offered back for as long as they stay archived.
  const restoreOffer = row.archived ? [] : (row.archived_routines ?? []);
  const ownKeys = new Set(ownRoutines.map((r) => r.key));
  const ownInUse = inUse?.filter((k) => ownKeys.has(k)) ?? [];
  const waitingInUse = inUse?.filter((k) => !ownKeys.has(k)) ?? [];
  const teamKey = useActiveTeamKey();
  const lineId = useId();
  const noteId = useId();
  const builtin = isBuiltin(row);
  const status = agentRowStatus(row, instances);
  const { state } = status;
  const chip = own ? nextRunChip(own.map(chipOfRow), now, undefined, row.name) : null;
  const line = agentRowLineParts(row, state, chip?.state === "next" ? chip.label : null);
  // The CLI's Preview badge follows its word, or ends a can't-launch line after its reason.
  const preview = line.cli && isPreviewRuntime(row.runtime) ? row.runtime : null;
  // A ready or archived row is quiet: its face wears no mark.
  const mark = state === "ready" ? null : STATE_MARK[state];
  // Opening another agent leaves a routine page; an unsaved routine edit asks first.
  const open = () => showAgent(row.key);
  // Fix goes where the reason is mended: Edit → Runs on for a model endpoint, else Overview.
  const fix = () => {
    if (status.fix === "endpoint") {
      showAgent(row.key, false, () =>
        useAgentsStore.getState().openEditSection(row.key, "runs-on"),
      );
      return;
    }
    showAgent(row.key);
    useAgentsStore.getState().setView("overview");
  };

  return (
    <li
      data-testid="agent-row"
      data-agent-key={row.key}
      data-selected={selected}
      data-state={state}
      aria-current={selected || undefined}
      className="group/row mx-1.5"
    >
      <div
        className={cn(
          "grid h-11 grid-cols-[minmax(0,1fr)_auto] items-center rounded-lg pr-1.5 transition-colors",
          selected
            ? state === "needs"
              ? "bg-needs-wash-strong"
              : "bg-muted"
            : state === "needs"
              ? NEEDS_WASH
              : "hover:bg-muted/55",
        )}
      >
        <button
          type="button"
          onClick={open}
          aria-label={`Open ${row.name}`}
          aria-describedby={[lineId, previewNoteRef(noteId, preview)].filter(Boolean).join(" ")}
          className="grid h-full min-w-0 grid-cols-[28px_minmax(0,1fr)] items-center gap-x-3 rounded-lg pl-2.5 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <span className="relative leading-none">
            <span className={cn("block", state === "archived" && "opacity-60 grayscale")}>
              <AgentRowFace agent={row} size={28} />
            </span>
            {mark && (
              <span className="absolute -right-1 -bottom-1 grid size-3.5 place-items-center rounded-full bg-background">
                <StatusMark mark={mark} size={9} />
              </span>
            )}
          </span>
          <span className="min-w-0">
            <span
              className={cn(
                "block truncate text-[13px] leading-tight font-semibold",
                state === "archived" && "text-muted-foreground",
              )}
            >
              {row.name}
            </span>
            <AgentRowLine id={lineId} broken={state === "broken"} line={line} preview={preview} />
            <PreviewNote id={noteId} cli={preview} />
          </span>
        </button>
        <div className="flex items-center justify-end pl-2">
          <span
            className={cn(
              "text-xs whitespace-nowrap",
              // The word gives way to the actions on hover and focus, as in the mockup; a row with
              // live instances keeps its shortcut among them (the ghost "N running" button).
              "group-focus-within/row:hidden group-has-[[data-state=open]]/row:hidden group-hover/row:hidden",
              state === "needs" && cn("font-semibold", INK.needs),
              state === "running" && cn("font-medium", INK.busy),
              state === "broken" && cn("font-medium", INK.warn),
              (state === "ready" || state === "archived") && "text-muted-foreground",
            )}
          >
            <StateWord row={row} status={status} />
          </span>
          <div className="hidden items-center gap-0.5 group-focus-within/row:flex group-hover/row:flex has-[[data-state=open]]:flex">
            {status.live > 0 && (
              <Button
                size="xs"
                variant="ghost"
                data-testid="agent-running-action"
                aria-label={`Show ${row.name}'s ${status.live} live ${status.live === 1 ? "agent" : "agents"}`}
                onClick={() =>
                  useViewerStore
                    .getState()
                    .revealAgentsListFor({ agentKey: row.key, name: row.name })
                }
              >
                {status.live} running
              </Button>
            )}
            {row.archived ? (
              <Button
                size="xs"
                variant="outline"
                disabled={archiving}
                aria-label={`Unarchive ${row.name}`}
                onClick={() => void unarchive(row.key)}
              >
                <ArchiveRestore />
                Unarchive
              </Button>
            ) : status.canLaunch ? (
              <Button
                size="xs"
                variant="outline"
                data-testid="agent-row-launch"
                aria-label={`Launch ${row.name}`}
                onClick={() => openLaunchSheet({ teamKey, agentKey: row.key })}
              >
                <Rocket />
                Launch
              </Button>
            ) : (
              <Button
                size="xs"
                variant="outline"
                data-testid="agent-row-fix"
                aria-label={
                  status.fix === "endpoint"
                    ? `Fix the model endpoint for ${row.name}`
                    : `Fix the account for ${row.name}`
                }
                onClick={fix}
              >
                <Wrench />
                Fix
              </Button>
            )}
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  size="icon-xs"
                  variant="ghost"
                  data-testid="agent-row-more"
                  aria-label={`More actions for ${row.name}`}
                >
                  <Ellipsis />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-44">
                {!row.archived && !status.canLaunch && (
                  <DropdownMenuItem
                    aria-label={`Launch ${row.name}`}
                    onSelect={() => openLaunchSheet({ teamKey, agentKey: row.key })}
                  >
                    <Rocket />
                    Launch…
                  </DropdownMenuItem>
                )}
                {!row.archived && (
                  <DropdownMenuItem
                    disabled={archiving || builtin}
                    aria-label={`Archive ${row.name}`}
                    onSelect={() => {
                      if (ownRoutines.length > 0) {
                        setConfirmArchive(true);
                        return;
                      }
                      void archive(row.key).then((outcome) => {
                        if (outcome === "needs_confirm") setConfirmArchive(true);
                      });
                    }}
                  >
                    <Archive />
                    Archive
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem
                  disabled={copying}
                  aria-label={`Duplicate ${row.name}`}
                  onSelect={() => void copyToMine(row.key)}
                >
                  <Copy />
                  Duplicate
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  variant="destructive"
                  disabled={builtin}
                  aria-label={`Delete ${row.name}`}
                  onSelect={() => onDelete(row)}
                >
                  <Trash2 />
                  Delete…
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>
      </div>
      {inUse && (
        <InlineAlert data-testid="agent-in-use" className="mx-2.5 mt-1">
          <span>
            {ownInUse.length > 0 && (
              <>
                {row.name} has{" "}
                {ownInUse.length === 1 ? "1 active routine" : `${ownInUse.length} active routines`}.
                Archive {row.name} instead, or turn those routines off first.
              </>
            )}
            {waitingInUse.length > 0 && (
              <>
                Still used by {waitingInUse.join(", ")}. Archive it instead, or move those routines
                to another agent first.
              </>
            )}
          </span>
        </InlineAlert>
      )}
      <ConfirmActionDialog
        open={confirmArchive}
        onOpenChange={setConfirmArchive}
        title={`Archive ${row.name} and ${
          ownRoutines.length === 1 ? "1 routine" : `${ownRoutines.length} routines`
        }?`}
        description={`They stop running. Restoring ${row.name} offers to bring them back.`}
        confirmLabel="Archive"
        loadingLabel="Archiving…"
        loading={archiving}
        variant="default"
        onConfirm={() => {
          setConfirmArchive(false);
          void archive(row.key, true);
        }}
        testId="agent-archive-confirm"
      >
        <ul className="list-disc pl-5 text-sm">
          {ownRoutines.map((r) => (
            <li key={r.key}>{r.name}</li>
          ))}
        </ul>
      </ConfirmActionDialog>
      {restoreOffer.length > 0 && !offerDismissed && (
        <div
          data-testid="agent-restore-routines"
          className="mx-2.5 mt-1 rounded-md bg-muted px-2.5 py-1.5 text-xs text-muted-foreground"
        >
          <p>
            {row.name} is back. {restoreOffer.length} of {row.name}&apos;s{" "}
            {restoreOffer.length === 1 ? "routine is" : "routines are"} still archived.
          </p>
          <div className="mt-1.5 flex gap-1.5">
            <Button
              size="xs"
              variant="outline"
              disabled={restoring}
              onClick={() => void restoreRoutines(row.key)}
            >
              Restore routines
            </Button>
            <Button size="xs" variant="ghost" onClick={() => setOfferDismissed(true)}>
              Not now
            </Button>
          </div>
        </div>
      )}
    </li>
  );
}

/**
 * A row's second line. A CLI's Preview badge follows its word; on a can't-launch line it goes
 * last, after the reason, and never shrinks. Narrow, the schedule tail gives way first, then the
 * reason down to about 8em, and below that the badge is dropped rather than clipped.
 */
function AgentRowLine({
  id,
  broken,
  line,
  preview,
}: {
  id: string;
  broken: boolean;
  line: AgentRowLineParts;
  preview: string | null;
}) {
  const badge = preview ? (
    <PreviewBadge
      cli={preview}
      className={cn("h-4 px-[5px] text-[10.5px]", broken && "@max-[190px]:hidden")}
    />
  ) : null;
  return (
    <span
      id={id}
      data-testid={broken ? "agent-account-warning" : "agent-row-line"}
      className={cn(
        "@container flex min-w-0 items-center gap-1.5 text-xs leading-snug text-muted-foreground",
        broken && cn("font-medium", INK.warn),
      )}
    >
      {broken && <TriangleAlert aria-hidden="true" className="size-3 shrink-0" />}
      {broken ? (
        <>
          <span className="min-w-[min(8em,100%)] shrink truncate">{`${line.cli} · ${line.rest}`}</span>
          {line.tail && <span className="min-w-0 shrink-[1000] truncate"> {`· ${line.tail}`}</span>}
          {badge}
        </>
      ) : line.cli && badge ? (
        <>
          <span className="shrink-0">{line.cli}</span> {badge}{" "}
          <span className="min-w-0 truncate">{`· ${line.rest}`}</span>
        </>
      ) : (
        <span className="truncate">{[line.cli, line.rest].filter(Boolean).join(" · ")}</span>
      )}
    </span>
  );
}

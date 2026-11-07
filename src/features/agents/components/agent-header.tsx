// SPDX-License-Identifier: MIT
import { type ReactNode } from "react";
import {
  ChevronRight,
  Copy,
  Key,
  Rocket,
  RotateCcw,
  Sparkles,
  Bell,
  CalendarClock,
  TriangleAlert,
  User,
  Users,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { AppButton as Button } from "@/shared/components/app-button";
import { StatusMark } from "@/shared/components/status-mark";
import { cn } from "@/lib/utils";
import { AgentRowFace } from "@/features/notes/components/agent-row-face";
import { openRoutines } from "@/features/routines/routines-virtual-tab";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import type { AgentRow } from "@/tauri/commands";

import {
  accountLine,
  BUILTIN_READ_ONLY,
  createdByLabel,
  isBuiltin,
  placeLabel,
} from "../lib/agent-display";
import { openLaunchSheet } from "../lib/launch-sheet";
import { useAgentInstances } from "../lib/use-agent-instances";
import { chipOfRow, nextRunChip, routinesLabel, routinesOf } from "../lib/agent-routines";
import {
  agentRowStatus,
  type AgentRowStatus,
  modelWord,
  NEEDS_MODEL_ENDPOINT,
  needsModelEndpoint,
  STATE_MARK,
  type AgentRowState,
} from "../lib/agent-row-state";
import { THIS_MAC } from "@/features/viewer/lib/agent-list-view";
import { INK } from "../lib/agent-tones";
import { useAgentsStore } from "../stores/agents-store";
import { AccountMenu } from "./agent-account-control";
import { BuiltinBadge, RuntimeBadge } from "./agent-bits";

function PlaceLine({ row }: { row: AgentRow }) {
  const Icon = row.scope === "personal" ? User : Users;
  return (
    <span className="inline-flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
      <Icon aria-hidden="true" className="size-3" />
      {row.scope === "personal" ? "Mine · only you" : `${placeLabel(row)} · shared`}
    </span>
  );
}

/** Launch (with a way to a branch or worktree), Fix when the agent can't launch (its account,
 *  or a model endpoint its CLI needs), or Restore for an archived agent. */
function HeaderActions({ row, status }: { row: AgentRow; status: AgentRowStatus }) {
  const canLaunch = status.canLaunch;
  const openSection = useAgentsStore((s) => s.openEditSection);
  const teamKey = useActiveTeamKey();
  const copyToMine = useAgentsStore((s) => s.copyToMine);
  const copying = useAgentsStore((s) => !!s.pending[`copy:${row.key}`]);
  const unarchive = useAgentsStore((s) => s.unarchive);
  const builtin = isBuiltin(row);
  const launch = () => openLaunchSheet({ teamKey, agentKey: row.key });
  return (
    <div className="flex shrink-0 items-center gap-1.5">
      {builtin && (
        <Button
          size="sm"
          variant="outline"
          data-testid="agent-detail-copy"
          disabled={copying}
          onClick={() => void copyToMine(row.key)}
        >
          <Copy />
          Use template
        </Button>
      )}
      {row.archived ? (
        <Button
          size="sm"
          variant="outline"
          aria-label={`Restore ${row.name}`}
          onClick={() => void unarchive(row.key)}
        >
          <RotateCcw />
          Restore
        </Button>
      ) : !canLaunch ? (
        <>
          {/* Fix is the primary action, but the sheet stays reachable: the account preview is
              this Mac's, and the launch may be for a machine where the CLI is fine. */}
          <Button
            size="sm"
            variant="outline"
            data-testid="agent-detail-launch"
            aria-label={`Launch ${row.name}`}
            onClick={launch}
          >
            <Rocket />
            Launch
          </Button>
          {status.fix === "endpoint" ? (
            <Button
              size="sm"
              data-testid="agent-detail-fix"
              aria-label={`Fix the model endpoint for ${row.name}`}
              className="border border-warning bg-warning-wash text-warning-ink hover:bg-warning-wash"
              onClick={() => openSection(row.key, "runs-on")}
            >
              Fix
            </Button>
          ) : (
            <AccountMenu row={row} mode="fix">
              <Button
                size="sm"
                data-testid="agent-detail-fix"
                aria-label={`Fix the account for ${row.name}`}
                className="border border-warning bg-warning-wash text-warning-ink hover:bg-warning-wash"
              >
                Fix
              </Button>
            </AccountMenu>
          )}
        </>
      ) : (
        <Button
          size="sm"
          data-testid="agent-detail-launch"
          aria-label={`Launch ${row.name}`}
          onClick={launch}
        >
          <Rocket />
          Launch
        </Button>
      )}
    </div>
  );
}

/** One cell of the strip. A cell with `onOpen` is a button; without it, plain text. */
function Cell({
  icon,
  label,
  value,
  sub,
  onOpen,
  openLabel,
  action,
  bad,
  testId,
}: {
  icon: ReactNode;
  label: string;
  value: ReactNode;
  sub: ReactNode;
  onOpen?: () => void;
  openLabel?: string;
  action?: ReactNode;
  bad?: boolean;
  testId?: string;
}) {
  const body = (
    <>
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground [&_svg]:size-3.5">
        {icon}
        {label}
      </span>
      <span className="flex min-w-0 items-center gap-2 text-sm font-medium">{value}</span>
      <span className="truncate text-xs text-muted-foreground">{sub}</span>
    </>
  );
  const base = cn(
    "relative grid min-w-0 content-start gap-0.5 border-l border-border/70 px-3.5 pt-2.5 pb-3 text-left first:border-l-0",
    bad && "bg-warning-wash",
  );
  return onOpen ? (
    <button
      type="button"
      onClick={onOpen}
      aria-label={openLabel}
      data-testid={testId}
      className={cn(
        base,
        "cursor-pointer outline-none hover:bg-muted/55 focus-visible:z-[1] focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      {body}
      <ChevronRight
        aria-hidden="true"
        className="absolute top-2.5 right-2.5 size-3.5 text-muted-foreground opacity-60"
      />
    </button>
  ) : (
    <div className={base} data-testid={testId}>
      {body}
      {action}
    </div>
  );
}

function stateParts(
  state: AgentRowState,
  live: number,
  needsYou: number,
  fix: AgentRowStatus["fix"],
) {
  const mark = STATE_MARK[state];
  switch (state) {
    case "needs":
      return {
        mark,
        word: <span className={INK.needs}>Needs you</span>,
        sub: needsYou === 1 ? "1 session is waiting" : `${needsYou} sessions are waiting`,
      };
    case "running":
      return { mark, word: <span>{live} running</span>, sub: "See Running" };
    case "broken":
      return {
        mark,
        word: <span className={INK.warn}>Can&apos;t launch</span>,
        sub: fix === "endpoint" ? "Set a model endpoint first" : "Fix the account first",
      };
    case "archived":
      return { mark, word: <span>Archived</span>, sub: "Restore to launch" };
    default:
      return { mark, word: <span>Ready</span>, sub: "Nothing waiting" };
  }
}

/** The profile header and its four-fact strip (Runs on, Account, Health, Next run). */
export function AgentProfileHeader({ row }: { row: AgentRow }) {
  const instances = useAgentInstances(row.key);
  const routineRows = useRoutinesStore((s) => s.rows);
  const now = useNowClock(30_000);
  const status = agentRowStatus(row, instances);
  const account = accountLine(row.account, row.runtime);
  const own = routineRows ? routinesOf(routineRows, row.key) : null;
  const chip = own ? nextRunChip(own.map(chipOfRow), now, undefined, row.name) : null;
  const routineCount = own?.length ?? row.routines?.length ?? 0;
  const routinesText =
    own && !row.archived
      ? routineCount === 0
        ? null
        : routineCount === 1
          ? "1 routine"
          : `${routineCount} routines`
      : routinesLabel(row);
  const health = stateParts(status.state, status.live, status.needsYou, status.fix);
  const setView = useAgentsStore((s) => s.setView);
  const openSection = useAgentsStore((s) => s.openEditSection);
  const builtin = isBuiltin(row);
  const hasLogin = row.account.provider !== null;
  const pinned = row.account.source === "binding";
  // The strip names a pinned or Active account; only a real system default is "your own login".
  const named = pinned || row.account.resolved_source === "active";
  const bad = account.kind === "warning";
  // A CLI that runs only on a configured model endpoint, and the agent names none.
  const noEndpoint = needsModelEndpoint(row);
  return (
    <div className="px-6 pt-5">
      <div className="flex items-center gap-4">
        <span className={cn("shrink-0", row.archived && "opacity-60 grayscale")}>
          <AgentRowFace agent={row} size={48} />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="flex items-center gap-2.5 text-xl leading-tight font-semibold tracking-tight">
            <span className="truncate">{row.name}</span>
            {builtin && <BuiltinBadge />}
            <PlaceLine row={row} />
          </h2>
          {row.description && (
            <p className="mt-0.5 truncate text-sm text-muted-foreground">{row.description}</p>
          )}
          <p className="mt-0.5 text-xs text-muted-foreground">
            <span data-testid="agent-created-by">Created by {createdByLabel(row)}</span>
          </p>
          {builtin && (
            <p data-testid="agent-builtin-note" className="mt-0.5 text-xs text-muted-foreground">
              {BUILTIN_READ_ONLY}.
            </p>
          )}
        </div>
        <HeaderActions row={row} status={status} />
      </div>
      <div className="mt-4 mb-3.5 grid grid-cols-4 rounded-xl border border-border bg-card/60">
        <Cell
          icon={<Sparkles />}
          label="Runs on"
          bad={noEndpoint}
          value={
            <>
              {/* Without an endpoint the reason takes the room; the CLI is named on the row. */}
              {!noEndpoint && <RuntimeBadge runtime={row.runtime} />}
              {noEndpoint ? (
                <span className={cn("flex min-w-0 items-center gap-1.5", INK.warn)}>
                  <TriangleAlert aria-hidden="true" className="size-3.5 shrink-0" />
                  <span className="line-clamp-2 min-w-0 leading-tight" title={NEEDS_MODEL_ENDPOINT}>
                    {NEEDS_MODEL_ENDPOINT}
                  </span>
                </span>
              ) : (
                <span className="truncate">{modelWord(row)}</span>
              )}
            </>
          }
          sub={
            noEndpoint
              ? `${THIS_MAC} · no endpoint set`
              : row.model
                ? "Model from --model"
                : "CLI default model"
          }
          onOpen={builtin ? undefined : () => openSection(row.key, "runs-on")}
          openLabel={`Change what ${row.name} runs on`}
          testId="agent-strip-runs-on"
        />
        <Cell
          icon={<Key />}
          label="Account"
          bad={bad}
          value={
            <>
              {bad && <TriangleAlert aria-hidden="true" className="size-3.5 shrink-0" />}
              <span className={cn("truncate", bad && INK.warn)} data-testid="agent-strip-account">
                {!hasLogin
                  ? "No CodeSense login"
                  : bad
                    ? "Account unavailable"
                    : named
                      ? account.text
                      : "Your own login"}
              </span>
              {pinned && !bad && <Badge tone="quiet">pinned</Badge>}
            </>
          }
          sub={
            !hasLogin ? (
              "Model settings are in Edit → Runs on"
            ) : bad ? (
              <span data-testid="agent-account-reason" title={account.text}>
                {account.text}
              </span>
            ) : (
              "Only you"
            )
          }
          action={
            hasLogin ? (
              <AccountMenu row={row} mode={bad ? "fix" : "change"} answersRequests>
                <Button
                  size="xs"
                  data-testid="agent-strip-account-action"
                  variant={bad ? "default" : "ghost"}
                  className={cn(
                    "absolute top-1.5 right-1.5 z-[2]",
                    bad &&
                      "border border-warning bg-warning-wash text-warning-ink hover:bg-warning-wash",
                  )}
                  aria-label={
                    bad ? `Fix the account for ${row.name}` : `Change the account for ${row.name}`
                  }
                >
                  {bad ? "Fix" : "Change"}
                </Button>
              </AccountMenu>
            ) : undefined
          }
        />
        <Cell
          icon={<Bell />}
          label="Health"
          value={
            <>
              {health.mark && <StatusMark mark={health.mark} size={9} />}
              {health.word}
            </>
          }
          sub={health.sub}
          onOpen={() => setView("running")}
          openLabel={`Running instances of ${row.name}`}
          testId="agent-strip-health"
        />
        <Cell
          icon={<CalendarClock />}
          label="Next run"
          value={
            chip ? (
              <span
                data-testid="agent-next-run"
                data-state={chip.state}
                role="img"
                aria-label={chip.tooltip}
                title={chip.tooltip}
                className="truncate capitalize"
              >
                {chip.label}
              </span>
            ) : (
              <span className="truncate">None</span>
            )
          }
          sub={routinesText ?? "No routines"}
          onOpen={
            routineCount > 0 && !row.archived
              ? () =>
                  openRoutines({
                    agentKey: row.key,
                    name: row.name,
                    routineKeys: own ? own.map((r) => r.key) : row.used_by,
                  })
              : undefined
          }
          openLabel={`${row.name}'s routines`}
          testId="agent-routines-link"
        />
      </div>
    </div>
  );
}

/** The one-line header the other tabs collapse to. */
export function AgentSlimHeader({ row }: { row: AgentRow }) {
  const instances = useAgentInstances(row.key);
  const status = agentRowStatus(row, instances);
  return (
    <div className="flex items-center gap-3 px-6 pt-3.5 pb-2.5">
      <span className={cn("shrink-0", row.archived && "opacity-60 grayscale")}>
        <AgentRowFace agent={row} size={28} />
      </span>
      <h2 className="truncate text-lg font-semibold tracking-tight">{row.name}</h2>
      {isBuiltin(row) && <BuiltinBadge />}
      <PlaceLine row={row} />
      <div className="ml-auto">
        <HeaderActions row={row} status={status} />
      </div>
    </div>
  );
}

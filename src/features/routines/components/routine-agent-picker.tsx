// SPDX-License-Identifier: MIT
import {
  PortaledSelect,
  type PortaledSelectGroup,
  type PortaledSelectOption,
} from "@/components/ui/portaled-select";
import { openAgents } from "@/features/agents/agents-virtual-tab";
import { RuntimeBadge } from "@/features/agents/components/agent-bits";
import { groupAgents, usedByLabel } from "@/features/agents/lib/agent-display";
import { useOrgStore } from "@/shared/stores/org-store";
import type { AgentRow, RoutineAgentDisplay } from "@/tauri/commands";

import { agentRefFor, findAgent, offeredTo, type RoutineSpace } from "../lib/routine-agent-ref";
import { useRoutinesStore } from "../stores/routines-store";

/** The value of the option standing for a saved reference no listed agent answers to. */
const UNLISTED = "unlisted:";

const LINK = "underline decoration-dotted underline-offset-2 hover:text-foreground";

/** The routine's saved agent: what the form loaded, and how the routine row shows it. */
export interface SavedAgent {
  ref: string;
  display: RoutineAgentDisplay;
}

/** The team in view as `org/team`, the space a personal routine's team agents come from. */
function useTeamInView(): string | null {
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  return org && team ? `${org}/${team}` : null;
}

function option(row: AgentRow, archived = false) {
  return {
    value: row.key,
    label: archived ? `${row.name} (archived)` : row.name,
    icon: <RuntimeBadge runtime={row.runtime} />,
  };
}

/**
 * The sentence's agent slot: a pick from your agents (Mine), the team's and the starters
 * CodeSense ships (Built-in), never typed. A team routine is offered only its own team's
 * agents and the starters. Archived agents are left out,
 * but the routine's saved agent always stays pickable, marked when archived, so an edit
 * never loses it.
 */
export function RoutineAgentPicker({
  value,
  onChange,
  routine,
  saved,
}: {
  /** The reference the draft holds. */
  value: string;
  onChange: (ref: string) => void;
  routine: RoutineSpace;
  saved?: SavedAgent;
}) {
  const agents = useRoutinesStore((s) => s.agents);
  const agentsError = useRoutinesStore((s) => s.agentsError);
  const teamInView = useTeamInView();
  const rows = agents ?? [];
  const offered = rows.filter((r) => !r.archived && offeredTo(r, routine, teamInView));
  const { mine, teams } = groupAgents(offered);

  const groups: PortaledSelectGroup[] = [];
  const savedRef = saved?.ref.trim() ?? "";
  const savedRow = savedRef ? findAgent(rows, savedRef, routine) : undefined;
  const picked = findAgent(rows, value, routine);
  // The saved agent, and an agent picked before the team in view changed, stay pickable
  // when they are not on offer, so the slot always shows what the draft holds.
  const current: PortaledSelectOption[] = [];
  for (const row of [savedRow, picked]) {
    if (row && !offered.includes(row) && !current.some((o) => o.value === row.key)) {
      current.push(option(row, row.archived));
    }
  }
  const held = value.trim();
  if (held && held !== savedRef && !picked) {
    current.unshift({ value: `${UNLISTED}${held}`, label: `${held} (not found)` });
  }
  if (savedRef && !savedRow) {
    current.unshift({
      value: `${UNLISTED}${savedRef}`,
      label: agents === null && saved ? saved.display.name : `${savedRef} (not found)`,
    });
  }
  if (current.length > 0) groups.push({ label: "Current", options: current });
  if (routine.scope === "personal" && mine.length > 0) {
    groups.push({ label: "Mine", options: mine.map((r) => option(r)) });
  }
  for (const team of teams) {
    groups.push({ label: `Team · ${team.label}`, options: team.rows.map((r) => option(r)) });
  }
  // Templates (builtins) are deliberately excluded here: a routine is a configured, named
  // agent — pick one you've saved. New agent → Use template to make one first.

  const selected = picked ? picked.key : held ? `${UNLISTED}${held}` : "";

  return (
    <div className="inline-flex flex-col gap-0.5">
      <PortaledSelect
        ariaLabel="Agent"
        value={selected}
        groups={groups}
        placeholder={
          agents !== null ? "pick an agent" : agentsError ? "agents unavailable" : "Loading agents…"
        }
        triggerClassName="h-8 w-56 rounded-lg border-input text-sm"
        onValueChange={(key) => {
          if (key.startsWith(UNLISTED)) {
            onChange(key.slice(UNLISTED.length));
            return;
          }
          const row = rows.find((r) => r.key === key);
          if (!row) return;
          // The agent already held keeps the reference as written, so re-picking it is
          // no edit.
          if (row === picked) onChange(value);
          else if (row === savedRow) onChange(savedRef);
          else onChange(agentRefFor(row, routine));
        }}
      />
      {agentsError && (
        <span data-testid="routine-agents-error" className="text-xs leading-4 text-destructive">
          Couldn&apos;t load agents: {agentsError}
        </span>
      )}
    </div>
  );
}

/**
 * The picked agent at a glance: what it runs on, how many routines use it and a way to
 * open it in the Agents tab, and, for a personal routine, where it lives when that is a
 * team other than the one in view (its runs need a Mac that syncs that team).
 */
export function RoutineAgentSummary({ value, routine }: { value: string; routine: RoutineSpace }) {
  const agents = useRoutinesStore((s) => s.agents);
  const teamInView = useTeamInView();
  const row = agents ? findAgent(agents, value, routine) : undefined;
  const offer = <OfferHint routine={routine} teamInView={teamInView} />;
  if (!row) return offer;
  const elsewhere =
    routine.scope === "personal" && row.scope === "team" && `${row.org}/${row.team}` !== teamInView;
  return (
    <div
      data-testid="routine-agent-summary"
      className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground"
    >
      <RuntimeBadge runtime={row.runtime} />
      <button
        type="button"
        data-testid="routine-agent-used-by"
        className={LINK}
        onClick={() => openAgents(row.key, row.archived)}
      >
        {usedByLabel(row)}
      </button>
      <button
        type="button"
        data-testid="routine-agent-open"
        className={LINK}
        onClick={() => openAgents(row.key, row.archived)}
      >
        Open agent
      </button>
      {elsewhere && (
        <span data-testid="routine-agent-elsewhere">
          This agent lives in team {row.team} · {row.org}; its runs need a Mac that syncs that team.
        </span>
      )}
      {offer}
    </div>
  );
}

/** Why the slot offers the agents it does, so a missing one reads as where to look,
 *  not as a refusal. */
function OfferHint({ routine, teamInView }: { routine: RoutineSpace; teamInView: string | null }) {
  const text =
    routine.scope === "team"
      ? "A team routine runs only its own team's agents or a built-in one, since any teammate's Mac may run it."
      : teamInView
        ? `Offers your agents and team ${teamInView.slice(teamInView.lastIndexOf("/") + 1)}'s; to pick another team's agent, switch to that team.`
        : "Offers your agents; open a team to pick one of its agents.";
  return (
    <span data-testid="routine-agent-offer" className="text-xs text-muted-foreground">
      {text}
    </span>
  );
}

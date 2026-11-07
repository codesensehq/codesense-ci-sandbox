// SPDX-License-Identifier: MIT
import { Inbox, Rocket, Terminal } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { AppButton as Button } from "@/shared/components/app-button";
import { StatusMark } from "@/shared/components/status-mark";
import { EmptyState } from "@/shared/components/empty-state";
import { cn } from "@/lib/utils";
import { goToSession } from "@/features/inbox/actions";
import { AgentRowFace } from "@/features/notes/components/agent-row-face";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import type { AgentRow } from "@/tauri/commands";

import type { RunningInstance } from "../lib/agent-running";
import { INSTANCE_MARK } from "../lib/agent-row-state";
import { INK, NEEDS_WASH } from "../lib/agent-tones";
import { openLaunchSheet } from "../lib/launch-sheet";

function InstanceRow({ agent, instance: i }: { agent: AgentRow; instance: RunningInstance }) {
  const open = () => void goToSession(i.handle);
  const needs = i.state === "needs";
  return (
    <li
      data-testid="agent-instance"
      data-handle={i.handle}
      data-state={i.state}
      className={cn(
        "group/inst grid grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-3 rounded-md border-b border-border/70 px-2 py-1.5 hover:bg-muted/55",
        needs && NEEDS_WASH,
      )}
    >
      <span className="relative leading-none">
        <AgentRowFace agent={agent} size={28} />
        <span className="absolute -right-1 -bottom-1 grid size-3.5 place-items-center rounded-full bg-background">
          <StatusMark mark={INSTANCE_MARK[i.state]} size={9} />
        </span>
      </span>
      <button
        type="button"
        onClick={open}
        aria-label={`Open ${i.name}`}
        data-testid="agent-instance-open"
        className="min-w-0 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="flex items-center gap-2 text-sm font-medium">
          <span className="truncate">{i.name}</span>
          {i.unread > 0 && (
            <Badge
              tone={needs ? "needs" : "neutral"}
              aria-label={`${i.unread} unread`}
              data-testid="agent-instance-unread"
            >
              {i.unread}
            </Badge>
          )}
        </span>
        <span className="block truncate text-xs text-muted-foreground">
          <span className={cn(needs && cn("font-medium", INK.needs))}>{i.stateWord}</span> ·{" "}
          {i.where}
          {i.machine ? ` · ${i.machine}` : ""}
          {i.since ? ` · since ${i.since}` : ""}
        </span>
      </button>
      <Button
        size="xs"
        variant="outline"
        tabIndex={-1}
        aria-hidden="true"
        onClick={open}
        className="opacity-0 group-focus-within/inst:opacity-100 group-hover/inst:opacity-100"
      >
        {i.unread > 0 ? <Inbox /> : <Terminal />}
        Open
      </Button>
    </li>
  );
}

/**
 * The live instances of this saved agent: state, where, since (when known) and unread. A row
 * opens that instance's chat or terminal; its messages stay in the Inbox, per instance.
 */
export function AgentRunning({
  agent,
  instances,
}: {
  agent: AgentRow;
  instances: readonly RunningInstance[];
}) {
  const teamKey = useActiveTeamKey();
  const launch = () => openLaunchSheet({ teamKey, agentKey: agent.key });
  return (
    <section aria-label="Running" data-testid="agent-running-view" className="px-6 pt-4 pb-6">
      <div className="mb-1.5 flex min-h-7 items-center gap-2">
        <h3 className="text-sm font-semibold">Running now</h3>
        <span className="text-[13px] text-muted-foreground">
          {instances.length === 1
            ? `1 of ${agent.name}'s sessions`
            : `${instances.length} of ${agent.name}'s sessions`}
        </span>
        {!agent.archived && (
          <Button
            size="xs"
            variant="outline"
            className="ml-auto"
            aria-label={`Launch another ${agent.name}`}
            onClick={launch}
          >
            <Rocket />
            Launch another
          </Button>
        )}
      </div>
      {instances.length === 0 ? (
        <EmptyState
          testId="agent-running-empty"
          icon={<Terminal />}
          title="Nothing running"
          body={`${agent.name} has no live sessions. Launch it to start one.`}
        />
      ) : (
        <ul className="m-0 list-none border-t border-border/70 p-0">
          {instances.map((i) => (
            <InstanceRow key={i.handle} agent={agent} instance={i} />
          ))}
        </ul>
      )}
      <p className="mt-3.5 flex items-center gap-1.5 text-xs text-muted-foreground">
        <Inbox aria-hidden="true" className="size-3" />
        Each session&apos;s messages are in the Inbox. Earlier versions of {agent.name} are in
        History.
      </p>
    </section>
  );
}

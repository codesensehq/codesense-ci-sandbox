// SPDX-License-Identifier: MIT
/**
 * The "Launch agent ▸" menu the story surfaces share: one item per agent (the
 * Agents tab's agents), the shortcut on the first available one, and one install
 * row per agent CLI that isn't on the login-shell PATH.
 *
 * Rendered as a plain button list (nav context menu) reading the nearest
 * `StoryAgentLaunchProvider`.
 */

import { cn } from "@/lib/utils";
import {
  groupStoryLaunchMenu,
  type StoryAgentLaunchActions,
  type StoryLaunchAgent,
  type StoryLaunchTarget,
} from "./story-agent-launch";

/** Stable test id for one agent's item. */
export function launchItemTestId(agent: StoryLaunchAgent): string {
  return `launch-agent-${agent.agentKey}`;
}

/** The agent's name, with where it lives when that is not the story's own team. */
function ItemBody({ agent }: { agent: StoryLaunchAgent }) {
  return (
    <span className="flex min-w-0 flex-col">
      <span className="truncate">
        {agent.name}
        {agent.place && <span className="ml-1.5 text-muted-foreground">· {agent.place}</span>}
      </span>
    </span>
  );
}

/** Stable test id for a missing CLI's install row. */
export function missingCliTestId(binary: string): string {
  return `launch-agent-missing-${binary}`;
}

/** A plain button list of the agents (nav context menu). */
export function LaunchAgentButtonList({
  actions,
  target,
  itemClassName,
  itemRole,
  onChosen,
}: {
  actions: StoryAgentLaunchActions;
  target: StoryLaunchTarget;
  itemClassName?: string;
  /** `"menuitem"` when the list sits inside a keyboard menu, so its arrow keys
   *  reach these rows too. */
  itemRole?: "menuitem";
  onChosen?: () => void;
}) {
  // A menu moves focus between its items itself, so they leave the Tab order.
  const roleProps = itemRole ? { role: itemRole, tabIndex: -1 } : {};
  const menu = groupStoryLaunchMenu(actions.agents);
  return (
    <>
      {menu.agents.map((agent) => (
        <button
          key={agent.agentKey}
          type="button"
          {...roleProps}
          data-testid={launchItemTestId(agent)}
          className={cn("flex w-full items-center gap-2", itemClassName)}
          onClick={(e) => {
            e.stopPropagation();
            onChosen?.();
            actions.launch(target, agent);
          }}
        >
          <ItemBody agent={agent} />
          {agent === menu.shortcutAgent && actions.shortcutLabel && (
            <span className="ml-auto pl-3 text-xs tracking-widest text-muted-foreground">
              {actions.shortcutLabel}
            </span>
          )}
        </button>
      ))}
      {menu.missing.map((row) => (
        <button
          key={row.binary}
          type="button"
          {...roleProps}
          disabled
          data-testid={missingCliTestId(row.binary)}
          className={cn(
            "flex w-full items-center gap-2 text-muted-foreground disabled:cursor-not-allowed",
            itemClassName,
          )}
        >
          {row.label}
        </button>
      ))}
    </>
  );
}

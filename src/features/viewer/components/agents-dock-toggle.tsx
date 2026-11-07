// SPDX-License-Identifier: MIT
import { StatusMark } from "@/shared/components/status-mark";
import { useEffect, useRef } from "react";

import { ChevronRight, Network } from "lucide-react";

import { useTeamAgentCounts } from "@/features/viewer/hooks/use-team-agents";
import {
  focusAgentsList,
  requestAgentsFocus,
  takeAgentsFocus,
} from "@/features/viewer/lib/agents-list-focus";
import { revealCollapsedTerminal } from "@/features/viewer/lib/focus-cascade";
import { isTerminalColumnShown } from "@/features/viewer/lib/terminal-column";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * The Agents control at the left end of the bottom workspace bar: the way back to the agents
 * list whenever it is off screen — folded, or gone with a closed CLI panel. It shows how many
 * agents run and how many wait on you; clicking it opens the list (and the panel, when closed)
 * and moves focus into it. While the list is on screen it renders nothing: the list's own
 * header folds it, and folding from there lands focus here.
 */
export function AgentsDockToggle({ teamKey }: { teamKey: string }): React.JSX.Element | null {
  const collapsed = useViewerStore((s) => s.workspacePanelCollapsed);
  // The list lives in the terminal column: on screen while that column is (split or full-screen).
  const columnShown = useViewerStore(isTerminalColumnShown);
  const setCollapsed = useViewerStore((s) => s.setWorkspacePanelCollapsed);
  const { agents, needsYou } = useTeamAgentCounts(teamKey);
  const ref = useRef<HTMLButtonElement>(null);
  // Whether keyboard focus sits on the control: it unmounts when the list comes back by any
  // route (its own click, ⌘\, a pill), and focus must follow into the list, not fall to <body>.
  const hadFocus = useRef(false);
  const listShown = columnShown && !collapsed;

  useEffect(() => {
    if (!listShown) {
      if (takeAgentsFocus(teamKey, "toggle")) ref.current?.focus();
      return;
    }
    if (!hadFocus.current) return;
    hadFocus.current = false;
    focusAgentsList(teamKey);
  }, [listShown, teamKey]);

  if (listShown) return null;

  const counts = [
    `${agents} running`,
    ...(needsYou > 0 ? [`${needsYou} ${needsYou === 1 ? "needs" : "need"} you`] : []),
  ].join(", ");

  const open = () => {
    // Opening the list brings its column back too (in place, never into full-screen).
    revealCollapsedTerminal();
    requestAgentsFocus(teamKey, "list");
    setCollapsed(false);
  };

  return (
    <button
      ref={ref}
      type="button"
      className="cs-agent-identity ag-dock-toggle"
      aria-expanded={false}
      aria-label={`Agents, ${counts}`}
      title={`Show agents · ${counts}`}
      data-agents-toggle=""
      onFocus={() => (hadFocus.current = true)}
      onBlur={() => (hadFocus.current = false)}
      onClick={open}
    >
      <Network className="size-3.5 shrink-0" aria-hidden />
      <span>Agents</span>
      <span className="ag-dock-n" aria-hidden>
        {agents}
      </span>
      {needsYou > 0 && (
        <span
          className="ag-count ag-count-needs ag-dock-needs"
          aria-hidden
          data-testid="agents-toggle-needs"
        >
          <StatusMark mark="question" size={5} />
          {needsYou}
        </span>
      )}
      <ChevronRight className="size-3 shrink-0 opacity-70" aria-hidden />
    </button>
  );
}

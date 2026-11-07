// SPDX-License-Identifier: MIT
import { StatusMark } from "@/shared/components/status-mark";
import { AlertCircle, Plus, RefreshCw } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import type { ConnectionPhase } from "@/shared/components/supervised-agent-session";
import type { AgentTabOption } from "@/shared/hooks/use-workspace-agent-tabs";

/**
 * SW-GS-2d — the in-surface session picker. Rendered INSIDE
 * the chat surface (not on the tray chrome) so the user binds without a tray
 * trip: Start new session + Connect to an open tab (live-agent
 * filtered with a ● live / ◐ not-responding hint). When the agent didn't ack
 * the handshake, it surfaces a recovery affordance — never a silent
 * wedge. Generic + agent-agnostic: any workspace agent surface renders it from
 * its `useAgentWorkspaceSession` api.
 */
export interface AgentSessionPickerProps {
  options: AgentTabOption[];
  onStartNew: () => void;
  onConnectToTab: (sessionId: string) => void;
  recovery: boolean;
  onRebind: () => void;
  boundSessionId: string | null;
  connection: ConnectionPhase;
}

export function AgentSessionPicker({
  options,
  onStartNew,
  onConnectToTab,
  recovery,
  onRebind,
  boundSessionId,
  connection,
}: AgentSessionPickerProps): React.JSX.Element {
  const busy = connection === "reconnecting";
  return (
    <div className="flex flex-col gap-2 rounded-md border border-border bg-muted/30 p-2">
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-muted-foreground">Agent session</span>
        <Button size="sm" variant="outline" onClick={onStartNew} disabled={busy}>
          <Plus className="size-3.5" aria-hidden />
          Start new session
        </Button>
      </div>

      {options.length > 0 && (
        <ul className="flex flex-col gap-1" data-testid="agent-tab-list">
          {options.map((o) => {
            const isBound = o.sessionId === boundSessionId;
            // A not-responding (◐) tab isn't connectable — surface WHY (use
            // Re-bind) since the disabled button's reason isn't otherwise visible.
            const notResponding = o.live === false;
            const connectButton = (
              <button
                type="button"
                onClick={() => onConnectToTab(o.sessionId)}
                // Only a PROVEN-live (●) tab is connectable: attaching to a
                // not-responding (◐) tab would prime a dead shell. To revive a
                // dead owned tab, the user uses Re-bind (state-aware reconnect).
                disabled={busy || o.live !== true}
                // native title: renders on a disabled button, which Radix InfoTip cannot
                title={notResponding ? "Not responding — use Re-bind to revive" : undefined}
                aria-label={`Connect to ${o.name}`}
                className={cn(
                  "flex-1 truncate rounded px-2 py-1 text-left text-sm hover:bg-accent disabled:opacity-50",
                  isBound && "font-medium text-foreground",
                )}
              >
                {o.name}
                {isBound && <span className="ml-1 text-xs text-muted-foreground">(bound)</span>}
              </button>
            );
            return (
              <li key={o.sessionId} className="flex items-center gap-2">
                <StatusMark
                  data-testid={`tab-state-${o.sessionId}`}
                  data-live={String(o.live)}
                  mark={o.live === true ? "ready" : o.live === false ? "error" : "unknown"}
                  size="md"
                />
                {connectButton}
              </li>
            );
          })}
        </ul>
      )}

      {recovery && (
        <div className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-xs">
          <AlertCircle className="mt-0.5 size-3.5 shrink-0 text-amber-600" aria-hidden />
          <div className="flex-1">
            Agent isn't responding — open its terminal in the tray, get it to a prompt, then
            Re-bind.
          </div>
          <Button size="sm" variant="outline" onClick={onRebind} disabled={busy}>
            <RefreshCw className="size-3.5" aria-hidden />
            Re-bind
          </Button>
        </div>
      )}
    </div>
  );
}

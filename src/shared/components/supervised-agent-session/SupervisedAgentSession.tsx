// SPDX-License-Identifier: MIT
import { AGENT_STATUS_WORDS, type AgentStatus } from "@/shared/lib/agent-status-config";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import {
  STATUS_PILL_CLASS,
  StatusPillContent,
  statusPillStyle,
} from "@/shared/components/status-pill";
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import type { ManagedSessionState } from "@/shared/lib/managed-session";

import type { ConnectionPhase, SessionOwner } from "./types";
import { ConnectionDot } from "./connection-dot";
import { useAgentSessionOwnership, type AgentSessionBinding } from "./use-agent-session-ownership";

/**
 * SW-GS-1 — the reusable supervised-agent-session control chrome
 * (architecture Rev-2026-06-23 §D). Renders the top-right ownership chip over
 * a supervised area and hands the input-lock state to its children so they can
 * gate whatever terminal they render.
 *
 * GENERIC: bound only to an `AgentSessionBinding`. SW-DR and future
 * agentic-driving over story/epic workspaces adopt it by passing a different
 * binding — nothing here is git-sync-specific.
 *
 * Agent-agnostic copy (AC-20): "Agent driving" / "You're in control" /
 * "Reconnecting…" — never a vendor string. The chip IS both the indicator and
 * the control (AC-10): the action button is co-located with the lock it
 * governs.
 */

/**
 * Pure chip label — agent-agnostic, exported for the AC-20 label test. Only
 * claims "Agent driving" when the agent actually holds a LIVE session (AC-14
 * unambiguity): `disconnected`/`reconnecting` get their own labels so the chip
 * never implies driving while there is no live session.
 */
export function agentSessionChipLabel(owner: SessionOwner, connection: ConnectionPhase): string {
  if (owner === "user") return "You're in control";
  switch (connection) {
    case "live":
      return "Agent driving";
    case "reconnecting":
      return "Reconnecting…";
    case "disconnected":
      return "Disconnected";
  }
}

/**
 * SW-AW-4 (D) — the TASK-level badge, a SEPARATE axis from owner/connection.
 * Returns the human-facing label for a managed state, or `null` while `running`
 * (the ownership chip already conveys "agent driving" — no badge needed). The
 * badge lets the human tell awaiting-answer / needs-attention / failed / done at a glance
 * without an ambiguous spinner (AC-7). Agent-agnostic; no vendor strings.
 */
export function taskStateBadgeLabel(state: ManagedSessionState): string | null {
  const status = taskStatus(state);
  return status ? AGENT_STATUS_WORDS[status] : null;
}

/** A managed task state as the agent status it reads as, in the shared vocabulary. */
function taskStatus(state: ManagedSessionState): AgentStatus | null {
  switch (state) {
    case "awaiting-answer":
      return "awaiting-answer";
    case "needs-attention":
      return "dropped";
    case "failed":
      return "failed";
    case "done":
      return "completed";
    case "running":
      return null;
  }
}

export interface SupervisedAgentSessionProps {
  binding: AgentSessionBinding;
  /**
   * SW-AW-4: the managed task-state for the bound session (a SEPARATE axis from
   * owner/connection, keyed by `sessionId`). When present and not `running`, a
   * task badge renders alongside the ownership chip. Omit for a plain (unmanaged)
   * supervised session — the chrome then shows only owner/connection, unchanged.
   */
  taskState?: ManagedSessionState;
  /**
   * The supervised area (typically the embedded terminal). Receives the
   * input-lock so it can pass `inputLocked` to its terminal. Render-prop keeps
   * the primitive agnostic of the terminal component.
   */
  children?: (api: { inputLocked: boolean }) => ReactNode;
  className?: string;
  /** Test/label hook. */
  "data-testid"?: string;
}

export function SupervisedAgentSession({
  binding,
  taskState,
  children,
  className,
  "data-testid": testId,
}: SupervisedAgentSessionProps): React.JSX.Element {
  const { owner, connection, inputLocked, isDriving, disconnect, reconnect } =
    useAgentSessionOwnership(binding);

  const label = agentSessionChipLabel(owner, connection);
  const taskBadge = taskState ? taskStateBadgeLabel(taskState) : null;
  const taskMark = taskState ? taskStatus(taskState) : null;
  const reconnecting = owner === "agent" && connection === "reconnecting";
  // Disconnect is the action only while the agent holds a live (or coming-up)
  // session; otherwise (user-in-control, or agent with no live session) the
  // action is Reconnect — so the chip never offers "disconnect from nothing".
  const showDisconnect = owner === "agent" && (isDriving || reconnecting);

  return (
    <div className={cn("relative", className)} data-testid={testId}>
      {/* SW-AW-4: task-state badge — a SEPARATE axis from the ownership chip.
          Renders only for a managed, non-running session so the human can tell
          awaiting-answer / needs-attention / failed / done at a glance (AC-7). */}
      {taskState && taskBadge && taskMark ? (
        <div
          className={cn(
            STATUS_PILL_CLASS,
            "absolute top-2 left-2 z-10 bg-popover px-2 py-1 whitespace-nowrap shadow-sm",
          )}
          style={statusPillStyle(markOfAgentStatus(taskMark))}
          role="status"
          aria-label={`Task status: ${taskBadge}`}
          data-task-state={taskState}
        >
          <StatusPillContent mark={markOfAgentStatus(taskMark)} label={taskBadge} />
        </div>
      ) : null}

      {/* Top-right ownership chip — compact but unmistakably interactive
          (AC-10). The action button is the only always-present control while
          the terminal is input-locked. */}
      <div
        className="absolute top-2 right-2 z-10 flex items-center gap-1.5 rounded-full border border-border bg-popover/95 px-2 py-1 text-xs shadow-sm backdrop-blur-sm"
        role="status"
        aria-label={`Session control: ${label}`}
        data-owner={owner}
        data-driving={isDriving}
      >
        <ConnectionDot connection={connection} />
        <span className="font-medium whitespace-nowrap">{label}</span>
        {showDisconnect ? (
          <Button
            variant="outline"
            size="xs"
            onClick={disconnect}
            disabled={reconnecting}
            aria-label="Disconnect — take control of the terminal"
          >
            Disconnect
          </Button>
        ) : (
          <Button
            variant="outline"
            size="xs"
            onClick={reconnect}
            aria-label="Reconnect — hand control back to the agent"
          >
            Reconnect
          </Button>
        )}
      </div>

      {children?.({ inputLocked })}
    </div>
  );
}

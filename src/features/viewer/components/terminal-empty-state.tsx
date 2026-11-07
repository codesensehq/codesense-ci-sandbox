// SPDX-License-Identifier: MIT
import { SquareTerminal } from "lucide-react";

import { TerminalServerNotice } from "@/features/shell/components/terminal-server-notice";
import { AppButton } from "@/shared/components/app-button";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";

import { CliShortcutsHint } from "./cli-shortcuts-hint";

interface TerminalEmptyStateProps {
  /** Opens the workspace picker so the user can start a story / epic-integration
   *  workspace (the only things that live in the CLI). */
  onOpenWorkspace: () => void;
  /** Starts a fresh agent terminal in the team, the empty CLI's first action. Absent, only
   *  "Open workspace" is offered. */
  onStartAgent?: () => void;
  startAgentUnavailable?: string;
  /** SW-EW-30: how many workspaces are minimized (soft-closed) and still running
   *  in the background. >0 → the copy acknowledges them (they're also listed in
   *  the left manager panel); 0 → it nudges opening the first one. Kept
   *  panel-agnostic — a statement of fact, not "look in the panel →" — so it
   *  reads correctly whether or not the manager panel is expanded (D7). */
  backgroundCount?: number;
}

/**
 * SW-EW-18 AC-8 / SW-EW-30 — the CLI empty state. Shown when no story /
 * epic-integration workspace is open (including after the user closes the last
 * one). The CLI is ALWAYS one of those workspaces, never a bare terminal, so the
 * empty state offers to open a workspace rather than spawn a shell. When
 * workspaces are still running in the background it says so (SW-EW-30).
 */
export function TerminalEmptyState({
  onOpenWorkspace,
  onStartAgent,
  startAgentUnavailable,
  backgroundCount = 0,
}: TerminalEmptyStateProps) {
  const hasBackground = backgroundCount > 0;
  // The live binding for View → Toggle Terminal: follows a rebind, empty when unbound.
  const hideKey = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-terminal"));
  return (
    <div
      data-testid="terminal-empty-state"
      className="flex h-full flex-col items-center justify-center gap-3 text-center"
      style={{ backgroundColor: "var(--cs-doc-bg, #0d1117)" }}
    >
      {/* SW-RT-6c: why new terminals can't open while the terminal-server
          restart is put off, with the way back to it. */}
      <TerminalServerNotice />
      <SquareTerminal size={28} className="opacity-40" aria-hidden="true" />
      <div className="max-w-xs px-6">
        <p className="text-sm text-[var(--foreground)]">No workspace open</p>
        <p className="mt-1 text-xs text-muted-foreground">
          {hasBackground
            ? `${backgroundCount} workspace${backgroundCount === 1 ? "" : "s"} still running in the background — open one to bring it forward, or start another.`
            : onStartAgent
              ? "Start an agent here, or open a story's workspace."
              : "Open a story's workspace to start a terminal here."}
        </p>
      </div>
      <div className="flex items-center gap-2">
        {onStartAgent && (
          <AppButton
            size="sm"
            data-testid="terminal-empty-state-start-agent"
            onClick={onStartAgent}
            disabled={!!startAgentUnavailable}
            title={startAgentUnavailable}
          >
            Start an agent
          </AppButton>
        )}
        <button
          type="button"
          data-testid="terminal-empty-state-open"
          onClick={onOpenWorkspace}
          className="rounded border border-[var(--border)] px-3 py-1.5 text-xs text-[var(--muted-foreground)] transition-colors hover:bg-[var(--accent)] hover:text-[var(--foreground)]"
        >
          Open workspace
        </button>
      </div>
      {startAgentUnavailable && (
        <p role="status" className="text-xs text-muted-foreground">
          {startAgentUnavailable}
        </p>
      )}
      {/* Stays after the one-time shortcuts hint is dismissed: the way out of an
          empty pane, since the shortcut hides it when there is nothing to focus. */}
      {hideKey && (
        <p data-testid="terminal-empty-state-hide-hint" className="text-xs text-muted-foreground">
          <kbd className="rounded border border-b-2 border-[var(--border)] px-1 font-mono text-[10px]">
            {hideKey}
          </kbd>{" "}
          hides the terminal
        </p>
      )}
      <CliShortcutsHint />
    </div>
  );
}

// SPDX-License-Identifier: MIT
import { useLayoutEffect, useRef } from "react";

import { ScrollText, SquareTerminal, X } from "lucide-react";

import {
  AgentRunOutputBody,
  closeAgentResult,
  dismissAgentResult,
} from "@/features/viewer/components/agent-result-panel";
import { useAgentDrill } from "@/features/viewer/hooks/use-agent-drill";
import { useCorrectedAgentRoster } from "@/features/viewer/hooks/use-corrected-agent-roster";
import { agentRowLabel, isHeadlessSession } from "@/features/viewer/lib/agent-row-presentation";
import { runLabel } from "@/features/viewer/lib/recent-runs-model";
import { useFocusWhenShown } from "@/shared/lib/warm-scopes";
import { useAgentResultStore } from "@/shared/stores/use-agent-result-store";

interface AgentRunOutputHostProps {
  teamKey: string;
  repoPath: string;
  visible?: boolean;
}

/**
 * The open run's output, laid over the CLI tiles of the bottom agent area. It covers the tiles
 * rather than replacing them, so every live terminal keeps its PTY, scrollback and active tab, and
 * closing it hands the column back exactly as it was. Renders nothing unless a run was opened while
 * this team was active.
 */
export function AgentRunOutputHost({
  teamKey,
  repoPath,
  visible = true,
}: AgentRunOutputHostProps): React.JSX.Element | null {
  const runId = useAgentResultStore((s) => s.runId);
  const openFor = useAgentResultStore((s) => s.teamKey);
  if (!runId || openFor !== teamKey) return null;
  return (
    <OpenRunOutput
      key={runId}
      runId={runId}
      teamKey={teamKey}
      repoPath={repoPath}
      visible={visible}
    />
  );
}

function OpenRunOutput({
  runId,
  teamKey,
  repoPath,
  visible,
}: AgentRunOutputHostProps & { runId: string }): React.JSX.Element {
  const sessionId = useAgentResultStore((s) => s.sessionId);
  const storyId = useAgentResultStore((s) => s.storyId);
  const { sessions } = useCorrectedAgentRoster();
  const { onLocate } = useAgentDrill(teamKey);
  // Only a live terminal agent has a tab to jump to; a headless run has no PTY.
  const liveRow = sessionId
    ? sessions?.find((r) => r.sessionId === sessionId && r.alive && !isHeadlessSession(r))
    : undefined;

  const regionRef = useRef<HTMLElement>(null);
  // Jumping to the terminal moves focus there itself; restoring the old focus would undo it.
  const restoreFocus = useRef(true);
  useLayoutEffect(() => {
    if (useAgentResultStore.getState().preserveFocus) return;
    // Take focus so keystrokes never land in a terminal hidden underneath, and give it back to
    // whatever had it (often that terminal) once the output closes.
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const region = regionRef.current;
    region?.focus();
    return () => {
      // Check ownership before unmount removes the region and clears its focus.
      if (restoreFocus.current && region?.contains(document.activeElement) && previous?.isConnected)
        previous.focus();
    };
  }, []);
  // A retained output stays mounted while its team is hidden: take focus again when it returns.
  useFocusWhenShown(teamKey, !useAgentResultStore.getState().preserveFocus, () =>
    regionRef.current?.focus({ preventScroll: true }),
  );

  return (
    <section
      ref={regionRef}
      aria-label="Run output"
      tabIndex={-1}
      data-testid="agent-run-output"
      data-run-output
      // The CLI band is dark whatever the app theme, so the output laid over its tiles is too.
      data-surface="canvas"
      className="ag-output-in absolute inset-0 z-30 flex flex-col bg-background text-foreground focus-visible:outline-none"
      onKeyDown={(e) => {
        if (e.key !== "Escape" || e.defaultPrevented) return;
        e.stopPropagation();
        closeAgentResult();
      }}
    >
      <AgentRunOutputBody
        teamKey={teamKey}
        repoPath={repoPath}
        visible={visible}
        header={(outcome) => {
          const name =
            (liveRow && agentRowLabel(liveRow)) ??
            runLabel(outcome?.story_id ?? storyId) ??
            "Agent run";
          return (
            <header className="flex h-11 shrink-0 items-center gap-2.5 border-b border-border bg-card px-4">
              <ScrollText className="size-4 shrink-0 text-muted-foreground" aria-hidden />
              <div className="flex min-w-0 flex-1 items-baseline gap-2">
                <span
                  className="truncate text-[13px] font-semibold tracking-[-0.005em]"
                  data-testid="agent-run-output-name"
                >
                  {name}
                </span>
                <span className="shrink-0 text-[11px] text-muted-foreground">Run output</span>
                <span
                  className="hidden min-w-0 truncate font-mono text-[10.5px] text-muted-foreground/80 md:inline"
                  title={runId}
                >
                  {runId}
                </span>
              </div>
              {liveRow && (
                <button
                  type="button"
                  className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md border border-border px-2.5 text-xs font-medium transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() => {
                    restoreFocus.current = false;
                    // Jumping into the terminal keeps the column the output revealed.
                    dismissAgentResult();
                    onLocate(liveRow);
                  }}
                >
                  <SquareTerminal className="size-3.5" aria-hidden />
                  Go to terminal
                </button>
              )}
              <button
                type="button"
                aria-label="Close run output"
                title="Close (Esc)"
                className="grid size-7 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                onClick={closeAgentResult}
              >
                <X className="size-4" />
              </button>
            </header>
          );
        }}
      />
    </section>
  );
}

/** Whether keyboard focus is inside an open run output, so app shortcuts can act on it rather than
 *  on a terminal or file tab hidden underneath. */
export function isRunOutputFocused(): boolean {
  const active = typeof document === "undefined" ? null : document.activeElement;
  return active instanceof Element && active.closest("[data-run-output]") !== null;
}

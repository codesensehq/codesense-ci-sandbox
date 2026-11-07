// SPDX-License-Identifier: MIT
import { useCallback, useMemo, type ReactNode } from "react";
import { SquareTerminal } from "lucide-react";

import { Button } from "@/components/ui/button";
import { SupervisedAgentSession } from "@/shared/components/supervised-agent-session";
import { AgentChatSurface } from "@/shared/components/agent-chat-surface";
import { AgentSessionPicker } from "@/shared/components/agent-session-picker";
import { ManagedQuestionPanel } from "@/shared/components/managed-question-panel";
import { ErrorBoundary } from "@/shared/components/error-boundary";
import {
  useManagedSessionStore,
  selectManagedState,
} from "@/shared/stores/use-managed-session-store";
import { useOldestPendingAsk } from "@/shared/hooks/use-oldest-pending-ask";
import { useSessionWorking } from "@/shared/hooks/use-session-working";
import { answerAskUser } from "@/shared/hooks/use-managed-session-bridge";
import type { AskUserResponse } from "@/shared/lib/managed-session";
import type { AgentWorkspaceSessionApi } from "@/shared/hooks/use-agent-workspace-session";
import { useRegisterPanel, type PanelDefinition } from "@/features/shell/right-pane/panel-registry";

import { useAgentModule } from "./use-agent-module";
import type { AgentModuleConfig, AgentModuleApi } from "./agent-module-config";

/**
 * SW-AW-7 — invokes the consumer's `answerablePrompts` render-prop inside its OWN
 * render so a synchronous throw is caught by the enclosing `<ErrorBoundary>` (the
 * render-prop must be called during a CHILD's render, not eagerly in the parent,
 * or the throw escapes the boundary).
 */
function AnswerableSlot({
  render,
  ctx,
}: {
  render: NonNullable<AgentModuleConfig["answerablePrompts"]>;
  ctx: Parameters<NonNullable<AgentModuleConfig["answerablePrompts"]>>[0];
}): ReactNode {
  return render(ctx);
}

/**
 * SW-AW-5 (AC-5) — the standard agent chrome rendered inside the right-pane panel:
 * the transcript (`<AgentChatSurface>` + `<SupervisedAgentSession>`), the in-surface
 * session picker, the consumer's `actionCards`, and the module-owned answerable-
 * prompt card (`<ManagedQuestionPanel>`, global + `sessionId`-keyed).
 *
 * Presentational: it receives the bound `session` as a prop (the lifecycle-owning
 * `useAgentModule` hook lives in the consumer), and subscribes only to the
 * `sessionId`-keyed managed store for the task badge — so hooks live INSIDE this
 * child element, never in the panel-def closure (adding-a-panel.md contract).
 */
function AgentModuleBody({
  session,
  actionCards,
  answerablePrompts,
}: {
  session: AgentWorkspaceSessionApi;
  actionCards?: AgentModuleConfig["actionCards"];
  answerablePrompts?: AgentModuleConfig["answerablePrompts"];
}): React.JSX.Element {
  const { binding, boundSessionId, canSend, disabledReason, recovery, picker } = session;

  // SW-AW-4: the managed task-state is a SEPARATE axis keyed by sessionId (not the
  // surface-keyed binding) — drives the task badge + the answerable-prompt card.
  const taskState = useManagedSessionStore(selectManagedState(boundSessionId ?? ""));
  // SW-AW-7: the module owns ask-selection (shared with `ManagedQuestionPanel`), so
  // a consumer `answerablePrompts` slot receives a guaranteed-non-null ask + count.
  const { ask, pendingCount } = useOldestPendingAsk(boundSessionId ?? "");
  // SW-DX-10 (AC-6): the chat surface's "working…" liveness lamp.
  const working = useSessionWorking(boundSessionId);
  const correlationId = ask?.correlationId ?? null;
  // Stable per ask (not per render) so a consumer memoizing on `answer` doesn't
  // churn on unrelated store ticks. The `void` keeps the answer path non-blocking.
  const answer = useCallback(
    (response: AskUserResponse) => {
      if (correlationId) void answerAskUser(correlationId, response);
    },
    [correlationId],
  );

  // SW-AW-4/7: the single answerable-prompt surface. A consumer `answerablePrompts`
  // slot (git-sync's conflict redline) OVERRIDES the default `<ManagedQuestionPanel>`;
  // the module keeps the ask feed, the empty-state guard, and the single
  // `answerAskUser` answer channel (no re-wire, non-blocking). Null when no
  // session/ask is pending. The slot is keyed by `correlationId` so it remounts per
  // ask (fresh consumer state, no bleed). Because `ask_user` is BLOCKING, a consumer
  // render throw must not leave the human with no way to answer: the `<ErrorBoundary>`
  // degrades to the module-owned `<ManagedQuestionPanel>` (a working generic answer
  // card) rather than an inert message — the question stays answerable, the session
  // never silently wedges. (Re-keyed per ask, so the boundary also resets per ask.)
  let answerable: ReactNode = null;
  if (boundSessionId) {
    if (answerablePrompts && ask) {
      answerable = (
        <ErrorBoundary
          key={ask.correlationId}
          scope="agent-module"
          fallback={<ManagedQuestionPanel sessionId={boundSessionId} />}
        >
          <AnswerableSlot
            render={answerablePrompts}
            ctx={{ ask, answer, pendingCount, sessionId: boundSessionId }}
          />
        </ErrorBoundary>
      );
    } else if (!answerablePrompts) {
      answerable = <ManagedQuestionPanel sessionId={boundSessionId} />;
    }
  }

  return (
    <div className="flex flex-1 flex-col gap-4 overflow-auto p-4" data-testid="agent-module-body">
      <SupervisedAgentSession binding={binding} taskState={taskState} />
      <AgentSessionPicker
        options={picker.options}
        onStartNew={picker.startNew}
        onConnectToTab={picker.connectToTab}
        recovery={recovery}
        onRebind={binding.onReconnect}
        boundSessionId={boundSessionId}
        connection={binding.connection}
      />
      {actionCards?.({ boundSessionId })}
      {answerable}
      <AgentChatSurface
        sessionId={boundSessionId}
        canSend={canSend}
        disabledReason={disabledReason}
        working={working}
      />
    </div>
  );
}

/**
 * SW-RP-3 header action: the "Open terminal" affordance. A child component (hooks,
 * if any, stay here — never in the `headerActions` closure body) that reveals the
 * born-minimized CLI workspace via the consumer-supplied `onOpenTerminal`.
 */
function OpenTerminalAction({ onOpenTerminal }: { onOpenTerminal: () => void }): React.JSX.Element {
  return (
    <Button
      size="sm"
      variant="ghost"
      className="h-6 gap-1 px-2 text-xs"
      onClick={onOpenTerminal}
      data-testid="agent-module-open-terminal"
    >
      <SquareTerminal className="size-3.5" aria-hidden />
      Open terminal
    </Button>
  );
}

/**
 * SW-AW-5 (AC-4) — register the agent module as a right-pane panel for the lifetime
 * of the calling component. Pass the `api` from `useAgentModule` (so ONE hook
 * instance owns the session) plus the `config`; the memoized def's `render`/
 * `headerActions` closures capture the live session per the memoized-def contract.
 *
 * A consumer that needs the handle (GS-10 — `promote()` on conflict-mode entry) does:
 *   const api = useAgentModule(config);
 *   useRegisterAgentModulePanel(config, api);
 * A panel-only consumer can use `<AgentModulePanel config={…} />` below instead.
 */
export function useRegisterAgentModulePanel(config: AgentModuleConfig, api: AgentModuleApi): void {
  const { session } = api;
  const def = useMemo<PanelDefinition>(
    () => ({
      id: config.panelId,
      title: config.title,
      icon: config.icon,
      render: () => (
        <AgentModuleBody
          session={session}
          actionCards={config.actionCards}
          answerablePrompts={config.answerablePrompts}
        />
      ),
      headerActions: config.onOpenTerminal
        ? () => <OpenTerminalAction onOpenTerminal={config.onOpenTerminal!} />
        : undefined,
    }),
    // `session` is fresh only on real session events (event-driven re-renders), so
    // the def is re-created — and gap-free same-id re-registered — only then. `config`
    // is a stable consumer constant.
    [session, config],
  );
  useRegisterPanel(def);
}

/**
 * SW-AW-5 (AC-4) — panel-only convenience: owns the `useAgentModule` hook AND
 * registers the panel. Use this when the consumer does NOT need the imperative
 * handle (a consumer that needs `promote()`/`session` uses `useAgentModule` +
 * `useRegisterAgentModulePanel` directly, to keep ONE hook instance). Renders
 * nothing itself — the panel body renders through the right-pane stack.
 */
export function AgentModulePanel({ config }: { config: AgentModuleConfig }): null {
  const api = useAgentModule(config);
  useRegisterAgentModulePanel(config, api);
  return null;
}

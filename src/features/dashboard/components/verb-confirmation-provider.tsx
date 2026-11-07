// SPDX-License-Identifier: MIT
import { VERB_REGISTRY } from "../lib/verb-registry.generated";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import { useAgentStop } from "@/features/viewer/components/agent-stop-dialog";
import { confirmEnd } from "@/features/viewer/lib/terminal-close";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { VerbCallDetails } from "./verb-call-details";
import {
  VerbConfirmationContext,
  type Approval,
  type ConfirmationRequest,
} from "./verb-confirmation-context";

type Request = ConfirmationRequest & { resolve: (answer: Approval) => void };
function requester(request: ConfirmationRequest): string {
  const scope =
    request.caller.kind === "team"
      ? `team ${request.caller.team?.team ?? "content"}`
      : request.caller.kind;
  return `Requested by ${request.caller.dashboard} · ${scope}`;
}
function confirmationHeadline(
  verb: string,
  args: Record<string, unknown>,
  replyRecipient?: string,
) {
  const send = VERB_REGISTRY.find((spec) => spec.name === verb)?.class === "send";
  const target = [args.target, args.handle, args.sessionId, args.to, args.messageId, args.key].find(
    (value) => typeof value === "string" || typeof value === "number",
  );
  if (send)
    return {
      send,
      title: "Confirm send",
      description:
        verb === "replyAsHuman"
          ? replyRecipient
            ? `Reply as you to ${replyRecipient}.`
            : `Reply as you to message ${String(args.messageId)} (recipient could not be resolved).`
          : `Send${verb === "sendAsHuman" ? " as you" : ""} to ${String(target ?? "recipient")}.`,
    };
  if (verb === "routineCancel")
    return {
      send,
      title: "Cancel routine run?",
      description: `Routine ${String(args.key ?? "this routine")}.`,
    };
  if (verb === "openWorkspace") {
    const workspace =
      args.target && typeof args.target === "object"
        ? (args.target as Record<string, unknown>)
        : {};
    const destination =
      args.storyId ??
      workspace.storyId ??
      workspace.branch ??
      workspace.path ??
      workspace.epic ??
      workspace.repo ??
      workspace.kind;
    return {
      send,
      title: "Allow launch?",
      description: destination ? `Launch an agent for ${String(destination)}.` : "Launch an agent.",
    };
  }
  return {
    send,
    title: `Allow ${verb}?`,
    description: `Run ${verb}${target === undefined ? "" : ` on ${String(target)}`}.`,
  };
}
/** Also covers frames mounted while a confirmation is open. */
function suspendFrames() {
  const previous = new Map<HTMLIFrameElement, { inert: boolean; pointerEvents: string }>();
  const suspend = () =>
    document.querySelectorAll("iframe").forEach((frame) => {
      if (previous.has(frame)) return;
      previous.set(frame, { inert: frame.inert, pointerEvents: frame.style.pointerEvents });
      frame.inert = true;
      frame.style.pointerEvents = "none";
    });
  suspend();
  const observer = new MutationObserver(suspend);
  observer.observe(document.body, { childList: true, subtree: true });
  return () => {
    observer.disconnect();
    previous.forEach((before, frame) => {
      frame.inert = before.inert;
      frame.style.pointerEvents = before.pointerEvents;
    });
  };
}
export function VerbConfirmationProvider({ children }: { children: ReactNode }) {
  const [queue, setQueue] = useState<Request[]>([]);
  const current = queue[0];
  const armedAt = useRef(0);
  const [always, setAlways] = useState(false);
  const { requestStop, dialog } = useAgentStop();
  const request = useCallback(
    (call: ConfirmationRequest) =>
      new Promise<Approval>((resolve) => {
        setQueue((pending) => [...pending, { ...call, resolve }]);
      }),
    [],
  );
  const answer = useCallback((request: Request, choice: Approval) => {
    request.resolve(choice);
    setQueue((pending) => {
      return pending[0] === request ? pending.slice(1) : pending;
    });
  }, []);
  useEffect(() => {
    if (!current) return;
    armedAt.current = performance.now() + 500;
    const restore = suspendFrames();
    const canConfirm = () => performance.now() >= armedAt.current;
    const a = (current.args ?? {}) as Record<string, unknown>;
    if (current.verb === "stopSession") {
      let active = true;
      const roster = useAgentListProjectionStore.getState();
      void (roster.sessions === null ? roster.reload() : Promise.resolve()).then(() => {
        if (!active) return;
        const row = useAgentListProjectionStore
          .getState()
          .sessions?.find((row) => row.handle === a.handle);
        if (!row) {
          answer(current, "cancel");
          return;
        }
        requestStop(row, {
          id: current.id,
          requester: requester(current),
          details: <VerbCallDetails args={current.args} />,
          allowAlways: current.allowAlways,
          onOpen: () => {
            armedAt.current = performance.now() + 500;
          },
          canConfirm,
          onAnswer: (allow, remember) =>
            answer(current, allow ? (remember ? "always" : "once") : "cancel"),
        });
      });
      return () => {
        active = false;
        restore();
      };
    }
    if (current.verb === "closeWorkspace" || current.verb === "closeTerminalTab") {
      let remember = false;
      const removeWorktree = current.verb === "closeWorkspace" && a.removeWorktree === true;
      const workspace = removeWorktree
        ? Object.values(useViewerStore.getState().workspaces)
            .flat()
            .find((workspace) => workspace.id === a.target || workspace.storyId === a.target)
        : undefined;
      void confirmEnd({
        id: current.id,
        ...(current.allowAlways
          ? {
              onAlways: (value: boolean) => {
                remember = value;
              },
            }
          : {}),
        title: removeWorktree
          ? "End and delete worktree?"
          : current.verb === "closeWorkspace"
            ? "End workspace?"
            : "End tab?",
        ...(removeWorktree
          ? {
              note: `Delete worktree ${String(workspace?.cwd || a.target)}.${
                a.discardUnsaved === true
                  ? " Uncommitted or unsaved changes will be permanently lost."
                  : ""
              }`,
            }
          : {}),
        running: [String(a.handle ?? a.target ?? "terminal")],
        confirmLabel: removeWorktree ? "End and delete worktree" : "End",
        requester: requester(current),
        details: <VerbCallDetails args={current.args} />,
        canConfirm,
      }).then((allow) => answer(current, allow ? (remember ? "always" : "once") : "cancel"));
    }
    return restore;
  }, [current, answer, requestStop]);
  const native =
    current && ["stopSession", "closeWorkspace", "closeTerminalTab"].includes(current.verb);
  const a = (current?.args ?? {}) as Record<string, unknown>;
  const { send, title, description } = confirmationHeadline(
    current?.verb ?? "",
    a,
    current?.replyRecipient,
  );
  return (
    <VerbConfirmationContext.Provider value={request}>
      {children}
      {dialog}
      {current &&
        !native &&
        createPortal(
          <ConfirmActionDialog
            key={current.id}
            open
            title={title}
            description={`${requester(current)}. ${description}`}
            confirmLabel={
              current.caller.kind === "team" || current.caller.kind === "message"
                ? "Allow once"
                : send
                  ? "Send"
                  : "Allow"
            }
            loadingLabel="Confirming…"
            loading={false}
            variant="default"
            onOpenChange={(open) => {
              if (!open) {
                setAlways(false);
                answer(current, "cancel");
              }
            }}
            onConfirm={() => {
              if (performance.now() < armedAt.current) return;
              setAlways(false);
              answer(current, always ? "always" : "once");
            }}
            testId="verb-confirmation"
          >
            <VerbCallDetails args={current.args} />
            {current.allowAlways && !send && (
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={always}
                  onChange={(e) => setAlways(e.target.checked)}
                />
                Always allow for this dashboard
              </label>
            )}
          </ConfirmActionDialog>,
          document.body,
        )}
    </VerbConfirmationContext.Provider>
  );
}

// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { buttonVariants } from "@/components/ui/button";
import { InlineAlert } from "@/components/ui/inline-alert";
import {
  checkActivityUpgrade,
  closeActivityUpgrade,
  confirmActivityUpgrade,
  useTerminalActivityUpgrade,
} from "../lib/terminal-activity-upgrade";

export function TerminalActivityUpgradeNotice() {
  const state = useTerminalActivityUpgrade();
  if (!state.target || state.open) return null;
  return (
    <InlineAlert
      className="max-w-md text-left"
      action={
        <button
          type="button"
          className={buttonVariants({
            variant: "outline",
            size: "sm",
            className: "text-foreground",
          })}
          onClick={() => void checkActivityUpgrade(true)}
        >
          Review update
        </button>
      }
    >
      Agent status needs a terminal server update. Your terminals can keep running.
    </InlineAlert>
  );
}

export function TerminalActivityUpgradePrompt() {
  const state = useTerminalActivityUpgrade();
  const safe = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    let active = true;
    const subscription = listen("terminal-activity-upgrade-required", () => {
      if (active) void checkActivityUpgrade();
    }).catch(() => null);
    void subscription.then(() => {
      if (active) void checkActivityUpgrade();
    });
    return () => {
      active = false;
      void subscription.then((unlisten) => unlisten?.());
    };
  }, []);
  const offered = useRef<string | null>(null);
  useEffect(() => {
    if (!state.target || state.open) return;
    const identity = `${state.target.serverId}/${state.target.epoch}`;
    if (offered.current === identity) return;
    offered.current = identity;
    toast.warning("Agent status needs a terminal server update", {
      description: "Your terminals can keep running. Review the update when you are ready.",
      action: { label: "Review update", onClick: () => void checkActivityUpgrade(true) },
    });
  }, [state.target, state.open]);
  useEffect(() => {
    if (state.phase === "failed") safe.current?.focus();
  }, [state.phase]);
  if (!state.open) return null;
  const count = state.target?.sessionIds.length ?? 0;
  const busy = state.phase === "checking" || state.phase === "updating";
  const complete = state.phase === "complete";
  return (
    <AlertDialog
      open
      onOpenChange={(open) => {
        if (!open) closeActivityUpgrade();
      }}
    >
      <AlertDialogContent
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          safe.current?.focus();
        }}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>
            {complete ? "Terminal server updated" : "Update the terminal server on This Mac?"}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {complete
              ? "Open a new managed Claude or Codex agent to use detailed activity reporting. Your provider login and existing hooks are unchanged. Codex may ask you to review CodeSense hooks with /hooks."
              : state.oldStopped
                ? "The previous server and its terminals have stopped. Retrying starts a replacement; it won't close an unrelated server."
                : state.phase === "checking"
                  ? "Checking which terminals this update would close…"
                  : state.phase === "failed"
                    ? "The server's current state needs to be checked before another update attempt."
                    : state.target
                      ? count > 0
                        ? `Updating closes ${count} ${count === 1 ? "terminal" : "terminals"} and stops agents running in ${count === 1 ? "it" : "them"}. Their scrollback is lost. This also closes terminals opened before the update finishes. Save your work first. You can keep working and update later.`
                        : "No terminals are running on this server now. Updating also closes any terminals opened before it finishes. Update it to enable detailed agent status and managed agent launches."
                      : "No terminal server update is currently required. Existing agents are not relaunched automatically."}
          </AlertDialogDescription>
        </AlertDialogHeader>
        {state.error && <InlineAlert severity="error">{state.error}</InlineAlert>}
        {state.phase === "updating" && (
          <p role="status" className="text-sm text-muted-foreground">
            Updating and verifying the terminal server…
          </p>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel ref={safe} disabled={state.phase === "updating"}>
            {complete || state.oldStopped || !state.target ? "Close" : "Keep working"}
          </AlertDialogCancel>
          {state.phase === "failed" && state.oldStopped && (
            <button
              type="button"
              className={buttonVariants({ variant: "default" })}
              onClick={() => void confirmActivityUpgrade()}
            >
              Retry update
            </button>
          )}
          {state.phase === "failed" && !state.oldStopped && (
            <button
              type="button"
              className={buttonVariants({ variant: "outline" })}
              onClick={() => void checkActivityUpgrade(true)}
            >
              Check again
            </button>
          )}
          {state.target && state.phase === "idle" && (
            <AlertDialogAction
              className={buttonVariants({ variant: count > 0 ? "destructive" : "default" })}
              disabled={busy}
              onClick={(event) => {
                event.preventDefault();
                void confirmActivityUpgrade();
              }}
            >
              {count > 0
                ? `Close ${count} ${count === 1 ? "terminal" : "terminals"} and update`
                : "Update terminal server"}
            </AlertDialogAction>
          )}
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

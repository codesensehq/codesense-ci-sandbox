// SPDX-License-Identifier: MIT
import { AlertTriangle, RotateCcw } from "lucide-react";

import { Button } from "@/components/ui/button";

// The rotated log the Rust panic hook (SF-3) and the frontend handlers (SF-11)
// both write to. Surfaced to the user so a support request can point at it.
const LOG_PATH = "~/Library/Logs/ai.codesense/CodeSense.log";

/**
 * Root-level crash fallback (SF-11). Renders when the whole `<App/>` subtree
 * throws, in place of the blank document body that an unguarded root unmount
 * leaves behind.
 *
 * MUST stay state-free: it renders precisely because the tree crashed, so it
 * reads no Zustand stores and no feature modules — any of which may be the thing
 * that threw. Primitives + the store-free `Button` atom only.
 */
export function RootCrashFallback() {
  return (
    <div className="flex h-screen w-screen items-center justify-center bg-background p-6 text-foreground">
      <div className="flex max-w-md flex-col items-center gap-4 rounded-lg border border-border bg-card p-8 text-center">
        <AlertTriangle className="size-8 text-destructive" aria-hidden />
        <div className="flex flex-col gap-1">
          <h1 className="text-base font-semibold">Something went wrong</h1>
          <p className="text-sm text-muted-foreground">
            CodeSense hit an unexpected error and couldn&apos;t continue rendering. Reloading the
            window usually recovers it.
          </p>
        </div>
        <Button onClick={() => window.location.reload()}>
          <RotateCcw aria-hidden />
          Reload
        </Button>
        <p className="text-xs text-muted-foreground">
          If this keeps happening, the details are logged to <code>{LOG_PATH}</code>.
        </p>
      </div>
    </div>
  );
}

/**
 * Compact per-team crash fallback (SF-11). Renders when a single team's
 * `<TeamWorkspaceView>` subtree throws — it degrades just that content slot,
 * leaving the app shell (switcher, status bar) and other opened teams alive.
 *
 * `onRetry` bumps the per-team reset nonce in `Workbench`; the boundary `key`
 * carries that nonce, so bumping it remounts a clean subtree. This is the only
 * reset path — backgrounded teams stay mounted with a stable key, so switching
 * away and back does NOT clear a crashed team. Stays state-free for the same
 * reason as the root fallback.
 */
export function TeamCrashFallback({ teamKey, onRetry }: { teamKey: string; onRetry: () => void }) {
  return (
    <div
      data-crashed-team={teamKey}
      className="flex h-full w-full flex-1 items-center justify-center bg-background p-6 text-foreground"
    >
      <div className="flex max-w-sm flex-col items-center gap-3 rounded-lg border border-border bg-card p-6 text-center">
        <AlertTriangle className="size-6 text-destructive" aria-hidden />
        <div className="flex flex-col gap-1">
          <p className="text-sm font-medium">This view hit an error</p>
          <p className="text-xs text-muted-foreground">
            The rest of CodeSense is still running. Retry to rebuild this view.
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={onRetry}>
          <RotateCcw aria-hidden />
          Retry
        </Button>
      </div>
    </div>
  );
}

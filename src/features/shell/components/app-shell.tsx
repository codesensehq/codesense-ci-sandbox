// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AgentLaunchSheetHost } from "@/features/agents/components/agent-launch-sheet-host";
import { EndConfirmDialog } from "@/features/viewer/components/end-confirm-dialog";

import { TerminalServerPrompt } from "./terminal-server-prompt";
import { TitleBar } from "./title-bar";

/**
 * Window-level chrome wrapper. Every App.tsx render branch wraps its
 * content in `<AppShell>` so the `TitleBar`, a single shared `TooltipProvider`
 * (SW-FX-80: delay=0 instant tooltips — the app-wide canonical delay), and a `<Toaster />`
 * are mounted on every screen — splash, login, deps-install,
 * org-picker, and main.
 *
 * The wrapped TooltipProvider replaces previous local wrappers
 * (TitleBar, PipelineStatusNav, and the per-surface providers SW-FX-80
 * removed), so those components render bare `Tooltip`s and inherit the
 * single provider from here.
 *
 * `Toaster` is inside the shell so the 4 splash branches get toast
 * coverage too — previously only the 5 "real" branches mounted a
 * Toaster and any toast fired during a splash was silently dropped.
 */
export function AppShell({ children }: { children: ReactNode }) {
  return (
    <TooltipProvider>
      <div className="flex flex-col h-screen overflow-hidden bg-background text-foreground">
        <TitleBar />
        {children}
      </div>
      {/* SW-RT-6c: the "restart terminal server" prompt — renders nothing until a terminal
          server from another CodeSense version is found. */}
      <TerminalServerPrompt />
      {/* The in-app confirmation before ending terminals that are still running. */}
      <EndConfirmDialog />
      {/* The launch sheet, opened from the agents list or an agent's page. */}
      <AgentLaunchSheetHost />
      {/* The shared Inbox surface (⇧⌘I or a rail's expand). */}
      <Toaster />
    </TooltipProvider>
  );
}

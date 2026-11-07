// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { useTheme } from "next-themes";

import { TooltipProvider } from "@/components/ui/tooltip";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { WorktreeEditorSurface } from "@/features/viewer/components/source-editor-inset";
import type { MachineSummary } from "@/tauri/commands";

import { stressPaths } from "./handlers/worktree-editor";

/**
 * `dev:mock` only: the worktree code editor mounted alone over a stand-in
 * terminal tile, so its states can be driven from a URL without booting a
 * story world —
 * `?harness=worktree-editor[&remote=1][&theme=light|dark][&stress=1][&tabs=N]
 * [&latency=ms][&offline=1][&file=src/auth/session.ts][&width=820]`.
 * The fixture lives in `handlers/worktree-editor.ts`.
 */
const SESSION = "harness-editor-session";
const WS_ID = "harness-editor-ws";

const BASE_FILES = [
  "src/auth/session.ts",
  "src/lib/retry.ts",
  "src/index.ts",
  "README.md",
  "package.json",
  "styles/app.css",
];

function harnessWorkspace(remote: boolean): Workspace {
  return {
    id: WS_ID,
    kind: "story",
    cwd: remote ? "" : "/Users/dev/worktrees/BUG-142",
    status: "ready",
    storyId: "BUG-142",
    storyFilePath: "/Users/dev/context/stories/BUG-142.md",
    sourceRepoPath: "/Users/dev/code/login-service",
    sourceRepoSlug: "acme/login-service",
    ...(remote
      ? {
          location: {
            kind: "machine" as const,
            machineId: "m-studio",
            requestId: "00000000-0000-4000-8000-000000000002",
            baseRef: "main",
            branch: "story/BUG-142-login-timeout",
            handle: "h-editor",
            cwd: "/home/dev/worktrees/BUG-142",
          },
        }
      : {}),
  };
}

function studioMachine(offline: boolean): MachineSummary {
  return {
    machineId: "m-studio",
    kind: "ssh",
    label: "studio",
    scope: null,
    declaration: null,
    binding: null,
    canPublish: true,
    status: offline
      ? {
          state: "unreachable",
          lastReachedAtMs: Date.now() - 12 * 60_000,
          attempt: 3,
          retryInSecs: 20,
        }
      : { state: "connected", workspaces: 1 },
    attention: 0,
    capabilities: { agentCallbacks: true },
  };
}

export function WorktreeEditorHarness() {
  const params = new URLSearchParams(window.location.search);
  const remote = params.get("remote") === "1";
  const offline = params.get("offline") === "1";
  const tabCount = Number(params.get("tabs") ?? 0) || 0;
  const file = params.get("file") ?? "src/auth/session.ts";
  const width = params.get("width");
  const { setTheme } = useTheme();
  const themeParam = params.get("theme");

  useEffect(() => {
    if (themeParam === "light" || themeParam === "dark") setTheme(themeParam);
  }, [themeParam, setTheme]);

  // Seed the stores once, before the editor's first render.
  useState(() => {
    const extra = tabCount > BASE_FILES.length ? stressPaths(tabCount - BASE_FILES.length) : [];
    const tabs = tabCount > 0 ? [...BASE_FILES, ...extra].slice(0, tabCount) : [file];
    useMachinesStore.setState({ byTeam: { "harness-team": [studioMachine(offline)] } });
    useViewerStore.setState((s) => ({
      worktreeEditorOpenBySession: { ...s.worktreeEditorOpenBySession, [SESSION]: true },
      worktreeEditorFile: { ...s.worktreeEditorFile, [WS_ID]: file },
      worktreeEditorTabs: { ...s.worktreeEditorTabs, [WS_ID]: tabs },
    }));
    return true;
  });

  return (
    <TooltipProvider delayDuration={300}>
      <div
        className="relative h-screen overflow-hidden bg-[#0b0d10] text-xs"
        style={{ width: width ? `${width}px` : "100vw" }}
      >
        <pre className="p-4 font-mono leading-5 text-[#9aa4b2]">
          {"$ claude --resume BUG-142\n▸ editing src/auth/session.ts\n"}
        </pre>
        <WorktreeEditorSurface
          workspace={harnessWorkspace(remote)}
          teamKey="harness-team"
          activeTabSessionId={SESSION}
          isFocused
        />
      </div>
    </TooltipProvider>
  );
}

// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { useTheme } from "next-themes";

import { TooltipProvider } from "@/components/ui/tooltip";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { SourceDiffOverlay } from "@/features/viewer/components/source-diff-overlay";

/**
 * `dev:mock` only: the source-diff tool mounted alone over a stand-in terminal
 * tile, so its states can be driven from a URL without booting a whole story
 * world — `?harness=source-diff[&remote=1][&theme=light][&diff=empty|error|slow|stale|large]`.
 * The fixture lives in `handlers/source-diff.ts`.
 */
const SESSION = "harness-session";

function harnessWorkspace(remote: boolean): Workspace {
  return {
    id: "harness-ws",
    kind: "story",
    cwd: remote ? "" : "/Users/dev/worktrees/SW-DIFF-1",
    status: "ready",
    storyId: "SW-DIFF-1",
    storyFilePath: "/Users/dev/context/stories/SW-DIFF-1.md",
    sourceRepoPath: "/Users/dev/code/codesense",
    sourceRepoSlug: "alderco/codesense",
    ...(remote
      ? {
          location: {
            kind: "machine" as const,
            machineId: "m-studio",
            requestId: "00000000-0000-4000-8000-000000000001",
            baseRef: "main",
            branch: "feat/SW-DIFF-1",
            handle: "h-diff",
            cwd: "/home/dev/worktrees/SW-DIFF-1",
          },
        }
      : {}),
  };
}

export function SourceDiffHarness() {
  const params = new URLSearchParams(window.location.search);
  const remote = params.get("remote") === "1";
  const { setTheme } = useTheme();
  const themeParam = params.get("theme");

  useEffect(() => {
    if (themeParam === "light" || themeParam === "dark") setTheme(themeParam);
  }, [themeParam, setTheme]);

  useEffect(() => {
    useViewerStore.setState((s) => ({
      sourceDiffShownBySession: { ...s.sourceDiffShownBySession, [SESSION]: true },
    }));
  }, []);

  return (
    <TooltipProvider delayDuration={300}>
      <div className="relative h-screen w-screen overflow-hidden bg-[#0b0d10] text-xs">
        <pre className="p-4 font-mono leading-5 text-[#9aa4b2]">
          {
            "$ claude --resume SW-DIFF-1\n▸ editing src/features/viewer/components/file-row.tsx\n▸ running pnpm vitest run src/features/viewer\n"
          }
        </pre>
        <SourceDiffOverlay
          workspace={harnessWorkspace(remote)}
          teamKey="harness-team"
          activeTabSessionId={SESSION}
          isFocused
        />
      </div>
    </TooltipProvider>
  );
}

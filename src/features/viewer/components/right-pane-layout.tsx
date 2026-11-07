// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: the right-pane host. It publishes the shared panel data via
 * `RightPaneHostProvider`, registers the built-in panels, and renders the generalized `RightPaneStack` (ordered
 * collapsible sections → tab-groups). The external props are unchanged so the
 * mount site (`team-workspace-view.tsx`) is untouched.
 */
import { registerBuiltinPanels } from "@/features/shell/right-pane/builtin-panels";
import { RightPaneHostProvider } from "@/features/shell/right-pane/right-pane-host-context";
import { RightPaneStack } from "@/features/shell/right-pane/right-pane-stack";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

// Register the built-in panels once at module load so they are present for both
// the stack renderer and the native View → Panels menu rebuild.
registerBuiltinPanels();

interface RightPaneLayoutProps {
  repoPath: string;
  onClose: () => void;
  onFileSelect?: (
    filePath: string,
    options?: { openInNewTab?: boolean; keepLayout?: boolean },
  ) => void;
  fullTree?: DerivedPipelineTree | null;
}

export function RightPaneLayout({
  repoPath,
  onClose,
  onFileSelect,
  fullTree,
}: RightPaneLayoutProps) {
  return (
    <RightPaneHostProvider
      value={{
        repoPath,
        onClose,
        onFileSelect,
        fullTree,
      }}
    >
      {/* SW-LN-B40: the SETUP→LIVE flow strip was removed — truncated at the default
          width, it only repeated the tab strip. The tabs own navigation. */}
      <div className="flex h-full min-h-0 flex-col">
        <RightPaneStack />
      </div>
    </RightPaneHostProvider>
  );
}

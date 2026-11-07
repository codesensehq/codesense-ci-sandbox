// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: shared data the right-pane host provides to its registered panels.
 *
 * Panels consume the parent-loaded `fullTree` (so they never run a second scan),
 * the resolved `repoPath` and the file-select callback. Rather than thread a
 * uniform prop-bag through the registry, the host publishes this once via
 * context and the panel bodies pull what they need. Panels that need nothing
 * (scratch) ignore it.
 */
import { createContext, useContext, type ReactNode } from "react";

import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

export interface RightPaneHostValue {
  /** Team content source repo (the nav repoPath, not the workspace identity). */
  repoPath: string;
  /** Close the whole right pane. */
  onClose: () => void;
  onFileSelect?: (
    filePath: string,
    options?: { openInNewTab?: boolean; keepLayout?: boolean },
  ) => void;
  fullTree?: DerivedPipelineTree | null;
}

const RightPaneHostContext = createContext<RightPaneHostValue | null>(null);

export function RightPaneHostProvider({
  value,
  children,
}: {
  value: RightPaneHostValue;
  children: ReactNode;
}) {
  return <RightPaneHostContext.Provider value={value}>{children}</RightPaneHostContext.Provider>;
}

/** Read the host data. Throws if a panel body is rendered outside the host. */
export function useRightPaneHost(): RightPaneHostValue {
  const value = useContext(RightPaneHostContext);
  if (!value) {
    throw new Error("useRightPaneHost must be used within a RightPaneHostProvider");
  }
  return value;
}

/** Read the host data, or `null` outside the host (a body rendered standalone). */
export function useOptionalRightPaneHost(): RightPaneHostValue | null {
  return useContext(RightPaneHostContext);
}

// SPDX-License-Identifier: MIT
import { registerRightPaneHost } from "@/features/viewer/lib/right-pane-host-registry";

/**
 * Give `teamKey` a right-pane host, as the workbench's right column does, for a test that mounts
 * a team view on its own. Returns the cleanup, which unregisters and removes the host.
 */
export function attachRightPaneHost(teamKey: string): () => void {
  const host = document.body.appendChild(document.createElement("div"));
  host.setAttribute("data-right-pane-host", teamKey);
  const unregister = registerRightPaneHost(teamKey, host);
  return () => {
    unregister();
    host.remove();
  };
}

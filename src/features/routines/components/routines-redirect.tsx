// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { openRoutines } from "../routines-virtual-tab";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** The retired Routines tab (`virtual://routines`), restored from a saved layout or a
 *  link: it opens Agents → Schedule in its place and closes itself (SPEC §10). An
 *  `?agent=<key>` in the path keeps its agent filter. The swap is not the user's open, so
 *  it leaves full-screen CLI on. */
export function RoutinesRedirect({ filePath }: { filePath: string }) {
  useEffect(() => {
    const query = filePath.split("?")[1] ?? "";
    const agentKey = new URLSearchParams(query).get("agent");
    openRoutines(
      agentKey ? { agentKey, name: agentKey.split("/").pop() ?? agentKey, routineKeys: [] } : null,
      { keepLayout: true },
    );
    const teamKey = getActiveTeamKey();
    useViewerStore.getState().closeTab(teamKey, "", filePath);
  }, [filePath]);
  return <p className="p-4 text-sm text-muted-foreground">Routines moved to Agents → Schedule.</p>;
}

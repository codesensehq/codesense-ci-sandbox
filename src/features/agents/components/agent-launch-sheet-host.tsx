// SPDX-License-Identifier: MIT
import { lazy, Suspense } from "react";

import { useLaunchSheetStore } from "../lib/launch-sheet";

const AgentLaunchSheet = lazy(() =>
  import("./agent-launch-sheet").then((m) => ({ default: m.AgentLaunchSheet })),
);

/** The app's one launch sheet: nothing until someone asks to launch an agent. */
export function AgentLaunchSheetHost(): React.JSX.Element | null {
  const request = useLaunchSheetStore((s) => s.request);
  const close = useLaunchSheetStore((s) => s.close);
  if (!request) return null;
  return (
    <Suspense fallback={null}>
      {/* Keyed by the request, so each opening starts from its own prefills. */}
      <AgentLaunchSheet key={JSON.stringify(request)} request={request} onClose={close} />
    </Suspense>
  );
}

// SPDX-License-Identifier: MIT
import type { ReactElement } from "react";

import { render } from "@/test-utils";
import { useAgentRosterListener } from "@/features/viewer/hooks/use-agent-roster-listener";

/**
 * SW-AM-9: render a panel that reads the app-root roster store WITH the app-root
 * listener mounted beside it — the same pairing `App.tsx` sets up in production.
 * A panel consumes the roster via `useAgentSessions` (a store selector); the
 * listener is what warms the store from a test's `dashboardListSessions` mock.
 * Without it the store stays `null` (not-loaded) and the panel renders its loading
 * state forever. Shared so each panel test doesn't hand-roll the same host.
 */
function RosterHost() {
  useAgentRosterListener();
  return null;
}

export function renderWithRosterListener(ui: ReactElement) {
  return render(
    <>
      <RosterHost />
      {ui}
    </>,
  );
}

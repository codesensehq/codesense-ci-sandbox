// SPDX-License-Identifier: MIT
import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { PANE_CHANGED_EVENT, type PaneChangedPayload } from "@/tauri/events";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import {
  useAgentStatusOrder,
  useAgentListProjectionStore,
  watchAgentList,
  projectedStatusNodes,
} from "@/features/viewer/lib/agent-list-projection";
import { AgentStatusChip } from "@/shared/components/agent-status-chip";
import type { AgentTreeNode } from "@/features/viewer/lib/agent-list-view";

function NativeStatusRow() {
  useEffect(watchAgentList, []);
  const sessions = useAgentListProjectionStore((s) => s.sessions) ?? [];
  const statusOf = useAgentStatusOf(sessions);
  const order = useAgentStatusOrder(true);
  const nodes = sessions.map((row, i) => ({
    projectedId: `row-${i}`,
    row,
    workspace: null,
    story: null,
    children: [],
    teamKey: "o:a",
  })) as AgentTreeNode[];
  const ordered = projectedStatusNodes(order, nodes);
  const row = sessions.find((r) => r.id === "scale-49");
  return (
    <section data-testid="native-status-seam">
      {row && (
        <div data-testid="native-status" data-status={statusOf(row)}>
          <AgentStatusChip status={statusOf(row)} />
        </div>
      )}
      <output data-testid="native-status-order">
        {JSON.stringify(ordered.map(({ node }) => [node.row.id, statusOf(node.row)]))}
      </output>
    </section>
  );
}

import { liveHub } from "@/features/dashboard/lib/live-hub";
import { SlotDashboardHost } from "@/features/shell/slots/slot-dashboard-host";
import { HtmlViewer } from "@/features/dashboard/components/html-viewer";

export function LiveHubHarness() {
  const [count, setCount] = useState(0);
  const [status, setStatus] = useState(false);
  const [visible, setVisible] = useState(true);
  const [slot, setSlot] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const [paneId, setPaneId] = useState("scale");
  useEffect(() => {
    const off = listen<PaneChangedPayload>(PANE_CHANGED_EVENT, ({ payload }) => {
      if (payload.id === paneId) setRefreshKey((n) => n + 1);
    });
    return () => {
      void off.then((stop) => stop());
    };
  }, [paneId]);
  const fileName = `${paneId}.html`;
  const filePath = `/mock-ctx/default/panes/${fileName}`;
  Object.assign(window, {
    __liveHubFrameIds: () => [
      ...(liveHub as unknown as { frames: Map<string, unknown> }).frames.keys(),
    ],
    __liveHubStatusStart: () => setStatus(true),
    __liveHubMount: (n: number, shown = true, inSlot = false, id = "scale") => {
      setPaneId(id);
      setSlot(inSlot);
      setCount(n);
      setVisible(shown);
    },
  });
  return (
    <main>
      {status && <NativeStatusRow />}
      {Array.from({ length: count }, (_, i) => (
        <div key={i} style={{ height: 150, display: visible ? "block" : "none" }}>
          {slot ? (
            <SlotDashboardHost
              refreshKey={refreshKey}
              slotId="right-pane"
              entry={{
                id: paneId,
                title: "Scale",
                path: filePath,
                panesDir: "/mock-ctx/default/panes",
                panelId: `pane:${paneId}`,
                status: "ready",
                scope: "team",
              }}
              contentRoot="/mock-ctx/default/content"
              visible={visible}
            />
          ) : (
            <HtmlViewer
              filePath={filePath}
              teamContentPath="/mock-ctx/default/content"
              refreshKey={0}
              visible={visible}
              pane={{
                contentRoot: "/mock-ctx/default/content",
                panesDir: "/mock-ctx/default/panes",
                fileName,
              }}
            />
          )}
        </div>
      ))}
    </main>
  );
}

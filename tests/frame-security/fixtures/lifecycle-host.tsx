import { mountTextEditor } from "./context-menu-editor";
import "../../../src/index.css";
import { HostMenuLayer } from "../../../src/features/shell/host-menu-layer";
import { VerbConfirmationProvider } from "../../../src/features/dashboard/components/verb-confirmation-provider";
import { EndConfirmDialog } from "../../../src/features/viewer/components/end-confirm-dialog";
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { HtmlViewer } from "../../../src/features/dashboard/components/html-viewer";
import { SlotDashboardHost } from "../../../src/features/shell/slots/slot-dashboard-host";
import { FullscreenOverlayShell } from "../../../src/features/viewer/components/fullscreen-overlay-shell";
import { useRetiringFrames } from "../../../src/features/dashboard/components/use-retiring-frames";
import { flushAll } from "../../../src/features/dashboard/lib/frame-lifecycle";
import { installFakeTauri } from "./fake-tauri";
import { NativeEditors } from "./native-editors";
import { useViewerStore } from "../../../src/shared/stores/viewer-store";

const coldNav = new URLSearchParams(location.search).has("cold-nav");
const api = installFakeTauri({
  kind: new URLSearchParams(location.search).has("budget") ? "team" : "private",
  activeTab: coldNav ? { "fixture:dev": "/fixture/cold.md" } : {},
  queryGate: coldNav
    ? new Promise<void>((done) => {
        (window as any).releaseNav = done;
      })
    : undefined,
});
function Host() {
  const [visible, show] = useState(true);
  const [open, setOpen] = useState(true);
  const [overlay, maximize] = useState(false);
  const [count, setCount] = useState(1);
  const [parkedFirst, parkFirst] = useState(false);
  React.useLayoutEffect(() => {
    api.control = { show, setOpen, maximize, setCount, parkFirst, flushAll };
  }, [show, setOpen, maximize, setCount, parkFirst]);
  const desired = React.useMemo(
    () => (open ? Array.from({ length: count }, (_, i) => String(i)) : []),
    [open, count],
  );
  const values = useRetiringFrames(
    desired,
    (id) => id,
    (f, id) => f.documentPath === `/fixture/${id}.html`,
  );
  return (
    <>
      <button id="opener" onClick={() => maximize(true)}>
        Open overlay
      </button>
      <div data-xterm-host>
        <div data-pane-id="terminal">
          <div className="xterm">
            <textarea id="terminal" aria-label="terminal" />
          </div>
        </div>
      </div>
      <FullscreenOverlayShell
        isFullscreen={overlay}
        onExit={() => maximize(false)}
        testId="overlay"
        header={<button onClick={() => maximize(false)}>Exit</button>}
      >
        <div style={{ width: 236, height: 300 }}>
          {values.map((id) => (
            <div
              key={id}
              style={{
                height: 250,
                display: open && visible && !(parkedFirst && id === "0") ? "block" : "none",
              }}
              inert={!open}
            >
              <SlotDashboardHost
                slotId="right-pane"
                contentRoot="/fixture"
                visible={visible && open && !(parkedFirst && id === "0")}
                entry={{
                  id,
                  title: "Lifecycle",
                  path: `/fixture/${id}.html`,
                  panelId: `test:${id}`,
                  panesDir: "/fixture",
                  scope: "private",
                }}
                intent={{ opened: true }}
              />
            </div>
          ))}
        </div>
      </FullscreenOverlayShell>
    </>
  );
}
// The focus authority's native command is deliberately not simulated: seam tests assert it.
api.HtmlViewer = HtmlViewer;
api.mountTextEditor = mountTextEditor;
api.mountNativeEditors = (mode: string) => {
  api.nativeRenames = [];
  useViewerStore.setState({
    customGroupsByTeam: {
      "fixture:dev": [{ id: "fixture-group", name: "Original", workspaceIds: [] }],
    },
  });
  const node = document.createElement("div");
  document.body.prepend(node);
  createRoot(node).render(
    <NativeEditors mode={mode} record={(name) => api.nativeRenames.push(name)} />,
  );
};
api.nativeFontPx = () => useViewerStore.getState().settings.viewerFontSizePx ?? 0;
api.mountViewer = (props: React.ComponentProps<typeof HtmlViewer>) => {
  const node = document.createElement("div");
  document.body.append(node);
  const root = createRoot(node);
  root.render(
    <HostMenuLayer>
      <HtmlViewer {...props} />
    </HostMenuLayer>,
  );
  return () => {
    root.unmount();
    node.remove();
  };
};
createRoot(document.getElementById("root")!).render(
  <VerbConfirmationProvider>
    <HostMenuLayer>
      <Host />
      <EndConfirmDialog />
    </HostMenuLayer>
  </VerbConfirmationProvider>,
);

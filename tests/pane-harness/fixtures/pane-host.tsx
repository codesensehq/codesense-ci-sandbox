import "../../../src/index.css";
import React, { useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { createRoot } from "react-dom/client";
import { Inbox } from "lucide-react";
import { registerPanel } from "../../../src/features/shell/right-pane/panel-registry";
import { SCRATCH_PANEL_ID } from "../../../src/features/shell/right-pane/right-pane-layout";
import { useNativeSlot } from "../../../src/features/shell/slots/native-slot-context";
import { liveHub } from "../../../src/features/dashboard/lib/live-hub";
import { applyTheme } from "../../../src/themes/runtime";
import { HostMenuLayer } from "../../../src/features/shell/host-menu-layer";
import { VerbConfirmationProvider } from "../../../src/features/dashboard/components/verb-confirmation-provider";
import { SlotDashboardHost } from "../../../src/features/shell/slots/slot-dashboard-host";
import { FullscreenOverlayShell } from "../../../src/features/viewer/components/fullscreen-overlay-shell";
import { flushAll } from "../../../src/features/dashboard/lib/frame-lifecycle";
import { useOrgStore } from "../../../src/shared/stores/org-store";
import { requestFrameFocus } from "../../../src/features/dashboard/lib/frame-lifecycle";
import { DOCK_FRAME_PANEL_ID } from "../../../src/features/shell/slots/slot-registry";
import { installFakeTauri } from "../../frame-security/fixtures/fake-tauri";
import type { PaneHostFixtures, PaneTheme } from "../harness";

const fixtures: PaneHostFixtures | undefined = (window as any).paneFixtures;
const api = installFakeTauri(fixtures ?? {});

/** The app's own theme palette, plus any host CSS variables the fixture overrides;
 *  the real host reads them into the pane's `--cs-*` theme. */
function setTheme(theme: PaneTheme = { scheme: "dark" }) {
  const root = document.documentElement;
  applyTheme(theme.id ?? "codesense", theme.scheme);
  root.dataset.surface =
    fixtures?.slotId === "file-nav" || fixtures?.slotId === "dock" ? "sidebar" : "viewer";
  for (const [name, value] of Object.entries(theme.vars ?? {})) root.style.setProperty(name, value);
  liveHub.publishTheme(theme.id ?? "codesense", theme.scheme);
}
setTheme(fixtures?.theme);

/** The built-in a failed pane falls back to, showing the slot state it was given. */
function NativeSelection() {
  const slot = useNativeSlot();
  return (
    <output data-testid="native-selection">
      {String(slot?.state.selected ?? slot?.state.conversation ?? slot?.intent?.conversation ?? "")}
    </output>
  );
}
registerPanel({
  id: SCRATCH_PANEL_ID,
  title: "Notes",
  icon: Inbox,
  render: () => <NativeSelection />,
});
if (fixtures?.ui)
  await liveHub.writeUi(`ui:${fixtures.slotId ?? "right-pane"}`, "all", fixtures.ui);

function Host() {
  const [visible, show] = useState(true);
  const [refreshKey, refresh] = useState(0);
  const [open, setOpen] = useState(true);
  const [overlay, maximize] = useState(false);
  const [intent, setIntent] = useState(fixtures?.intent);
  // Right-pane sections; the pane body lives in one stable container that moves between
  // them with appendChild, as the real right pane reparents a panel body.
  const [section, setSection] = useState<"a" | "b">("a");
  const sections = { a: useRef<HTMLDivElement>(null), b: useRef<HTMLDivElement>(null) };
  const body = useMemo(() => {
    const el = document.createElement("div");
    el.style.height = "100%";
    return el;
  }, []);
  React.useLayoutEffect(() => {
    const slot = sections[section].current;
    if (slot && body.parentElement !== slot) slot.appendChild(body);
  });
  React.useLayoutEffect(() => {
    api.control = {
      show,
      refresh: () => refresh((key) => key + 1),
      setOpen,
      maximize,
      flushAll,
      focus: () =>
        requestFrameFocus(
          fixtures?.slotId === "dock" ? DOCK_FRAME_PANEL_ID : `harness:${fixtures?.id}`,
        ),
      setTeam: (org: string, team: string) =>
        useOrgStore.setState({ activeOrgSlug: org, activeTeamName: team }),
      reparent: async () => {
        await flushAll(300);
        setSection((current) => (current === "a" ? "b" : "a"));
      },
      setTheme,
      sendIntent: (next: Record<string, unknown>) => setIntent({ ...next }),
    };
  }, []);
  if (!fixtures) return null;
  return (
    <>
      <button id="opener">Host</button>
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
        header={
          <div data-testid="overlay-strip">
            {fixtures.title ?? "Pane"} · private
            <button onClick={() => maximize(false)}>Exit</button>
          </div>
        }
      >
        {(["a", "b"] as const).map((id) => (
          <div
            key={id}
            ref={sections[id]}
            data-section={id}
            style={{
              width: overlay ? "100%" : (fixtures.width ?? 320),
              height: id === section ? (fixtures.height ?? 480) : 0,
            }}
          />
        ))}
        {open &&
          createPortal(
            <div style={{ height: "100%", display: visible ? "block" : "none" }}>
              <SlotDashboardHost
                slotId={fixtures.slotId ?? "right-pane"}
                renderNative={fixtures.slotId === "dock" ? () => <NativeSelection /> : undefined}
                headerless={fixtures.slotId === "dock" || fixtures.slotId === "file-nav"}
                refreshKey={refreshKey}
                contentRoot="/fixture"
                visible={visible}
                entry={{
                  id: fixtures.id,
                  title: fixtures.title ?? "Pane",
                  path: `/fixture/${fixtures.id}/${fixtures.file}`,
                  panelId:
                    fixtures.slotId === "dock" ? DOCK_FRAME_PANEL_ID : `harness:${fixtures.id}`,
                  panesDir: `/fixture/${fixtures.id}`,
                  scope: "private",
                  ...(fixtures.surface ? { surface: fixtures.surface } : {}),
                }}
                intent={intent}
              />
            </div>,
            body,
          )}
      </FullscreenOverlayShell>
    </>
  );
}

createRoot(document.getElementById("root")!).render(
  <VerbConfirmationProvider>
    <HostMenuLayer>
      <Host />
    </HostMenuLayer>
  </VerbConfirmationProvider>,
);

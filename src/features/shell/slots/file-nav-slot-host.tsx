// SPDX-License-Identifier: MIT
import { useUiStore } from "@/shared/stores/ui-store";
import { OwningScopeProvider } from "@/shared/lib/warm-scopes";
import { postDashboardCommand } from "@/features/dashboard/lib/frame-lifecycle";
import {
  NavFileActionsOwner,
  type PipelineStatusNavProps,
} from "@/features/dashboard/components/nav-file-actions-owner";
import { PipelineStatusNav } from "@/features/dashboard/components/pipeline-status-nav";
import type { PaneEntry } from "@/tauri/commands";
import type { LiveFrame } from "@/features/dashboard/lib/frame-lifecycle";
import { useRetiringFrames } from "@/features/dashboard/components/use-retiring-frames";
import {
  isValidElement,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { create } from "zustand";
import { useDocumentVisible } from "@/features/inbox/hooks/use-document-visible";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { buildTeamKey } from "@/shared/lib/team-key";
import { NativeSlotProvider } from "./native-slot-context";
import { selectedRefs } from "./reveal-slot";
import { useSingleSlotDashboard } from "./single-slot-dashboard";
import { SlotDashboardHost } from "./slot-dashboard-host";

type Mount = {
  teamKey: string;
  target: HTMLDivElement;
  rect: DOMRect;
  native: ReactNode;
  selected?: string;
};
const useMount = create<{ mount: Mount | null }>(() => ({ mount: null }));

function useChoice() {
  const profile = useDashboardProfileStore((s) => s.profile);
  const org = useOrgStore((s) => s.activeOrgSlug);
  return org && profile ? selectedRefs("file-nav", org)[0] : undefined;
}

/** Native content stays in its team view. Only a chosen dashboard needs a measured mount. */
export function FileNavSlotMount(props: {
  teamKey: string;
  selected?: string;
  children: ReactNode;
}) {
  const child = props.children;
  if (isValidElement<PipelineStatusNavProps>(child) && child.type === PipelineStatusNav) {
    return (
      <NavFileActionsOwner
        {...child.props}
        teamKey={props.teamKey}
        renderNative={(body) => <FileNavSlotMountBody {...props}>{body}</FileNavSlotMountBody>}
      />
    );
  }
  return <FileNavSlotMountBody {...props} />;
}

function FileNavSlotMountBody({
  teamKey,
  selected,
  children,
}: {
  teamKey: string;
  selected?: string;
  children: ReactNode;
}) {
  const choice = useChoice();
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  const active = !!org && !!team && buildTeamKey(org, team) === teamKey;
  const target = useRef<HTMLDivElement>(null);
  const nativeBody = children;
  const native = useRef(nativeBody);
  useLayoutEffect(() => {
    native.current = children;
  }, [teamKey, children]);
  const chosen = !!choice;
  useLayoutEffect(() => {
    const element = target.current;
    if (!chosen || !active || !element) return;
    const measure = () =>
      useMount.setState({
        mount: {
          teamKey,
          target: element,
          rect: element.getBoundingClientRect(),
          native: native.current,
          selected,
        },
      });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
      if (useMount.getState().mount?.target === element) useMount.setState({ mount: null });
    };
  }, [active, chosen, teamKey, selected]);
  useLayoutEffect(() => {
    if (active && target.current && useMount.getState().mount?.target === target.current)
      useMount.setState((s) => ({
        mount: s.mount ? { ...s.mount, native: native.current } : null,
      }));
  }, [children, active]);
  const viewKey = `team:${teamKey.replace(/:([^:]*)$/, "/$1")}`;
  useEffect(() => {
    void liveHub
      .writeUi("ui:file-nav", viewKey, { selected: selected ?? null })
      .catch(console.error);
  }, [selected, viewKey]);
  if (!choice)
    return (
      <NativeSlotProvider slotId="file-nav" panelId="slot:file-nav" viewKey={viewKey}>
        {nativeBody}
      </NativeSlotProvider>
    );
  return (
    <div ref={target} className="flex min-h-0 flex-1 flex-col" data-file-nav-active={active} />
  );
}

/** One persistent frame per window; its DOM node never moves between team views. */
export function FileNavSlotHost() {
  useEffect(
    () =>
      useViewerStore.subscribe((state, previous) => {
        if (
          state.statusNavExpandedPaths === previous.statusNavExpandedPaths &&
          state.navLens === previous.navLens
        )
          return;
        const teams = new Set([
          ...Object.keys(state.statusNavExpandedPaths),
          ...Object.keys(state.navLens),
        ]);
        for (const teamKey of teams) {
          if (
            state.statusNavExpandedPaths[teamKey] === previous.statusNavExpandedPaths[teamKey] &&
            state.navLens[teamKey]?.source.kind === previous.navLens[teamKey]?.source.kind
          )
            continue;
          const at = teamKey.lastIndexOf(":");
          liveHub.refreshUi("ui:file-nav", `team:${teamKey.slice(0, at)}/${teamKey.slice(at + 1)}`);
        }
      }),
    [],
  );
  const mount = useMount((s) => s.mount);
  const collapsed = useViewerStore((s) =>
    mount ? (s.navCollapsed[mount.teamKey] ?? false) : true,
  );
  const visible = !!mount && !collapsed && mount.rect.width > 0 && mount.rect.height > 0;
  const documentVisible = useDocumentVisible();
  const current = useSingleSlotDashboard("file-nav", visible && documentVisible);
  const key = current?.key;
  const revealNonce = useViewerStore((s) => (mount ? (s.revealCounter[mount.teamKey] ?? 0) : 0));
  const seenReveal = useRef("");
  useEffect(() => {
    const signal = `${mount?.teamKey}:${revealNonce}`;
    if (!key || !mount?.selected || seenReveal.current === signal) return;
    seenReveal.current = signal;
    let cancelled = false;
    const path = mount.selected;
    const at = mount.teamKey.lastIndexOf(":");
    const viewKey = `team:${mount.teamKey.slice(0, at)}/${mount.teamKey.slice(at + 1)}`;
    void liveHub
      .writeUi("ui:file-nav", viewKey, { selected: path })
      .then(() => {
        if (cancelled) return;
        postDashboardCommand(key, "file-nav", {
          id: "nav.reveal-file",
          args: { path },
          label: "Reveal file",
          dashboardTitle: current.entry.title,
        });
      })
      .catch(console.error);
    return () => {
      cancelled = true;
    };
  }, [key, mount?.teamKey, mount?.selected, revealNonce, current?.entry.title]);
  const focusNonce = useUiStore((s) => s.leftNavSearchFocusNonce);
  const seenFocusNonce = useRef(focusNonce);
  useEffect(() => {
    if (seenFocusNonce.current === focusNonce) return;
    seenFocusNonce.current = focusNonce;
    if (key && visible)
      postDashboardCommand(key, "file-nav", {
        id: "nav.focus-left-search",
        label: "Focus Nav Search",
        dashboardTitle: current?.entry.title ?? "Files",
      });
  }, [focusNonce, key, visible, current?.entry.title]);
  const activeHosts = useMemo(() => (current ? [current] : []), [current]);
  const hosts = useRetiringFrames(activeHosts, (host) => host.key, ownsFileNavFrame);
  // The provider stays in the tree once a team has mounted, so a retiring frame keeps its owner
  // and its place while the mount is absent.
  const [lastTeamKey, setLastTeamKey] = useState<string | null>(null);
  if (mount && mount.teamKey !== lastTeamKey) setLastTeamKey(mount.teamKey);
  const owner = mount?.teamKey ?? lastTeamKey;
  const renderNative = useCallback(() => mount?.native, [mount?.native]);
  const style = useMemo(
    () => ({
      position: "fixed" as const,
      left: mount?.rect.left ?? 0,
      top: mount?.rect.top ?? 0,
      width: mount?.rect.width ?? 0,
      height: mount?.rect.height ?? 0,
      display: visible ? undefined : "none",
      zIndex: 10,
      pointerEvents: "none" as const,
    }),
    [mount?.rect, visible],
  );
  if (!hosts.length) return null;
  const body = (
    <div style={style} data-surface="sidebar">
      {hosts.map((host) => {
        const outgoing = host.key !== key;
        return (
          <div
            key={host.key}
            className={
              outgoing
                ? "pointer-events-none invisible absolute inset-0"
                : "flex h-full min-h-0 flex-col"
            }
            inert={outgoing || undefined}
            aria-hidden={outgoing || undefined}
          >
            <SlotDashboardHost
              slotId="file-nav"
              entry={host.entry}
              contentRoot={host.contentRoot}
              visible={visible && !outgoing}
              renderNative={renderNative}
              fallbackTarget={outgoing ? undefined : mount?.target}
              headerless
              refreshKey={host.refreshKey}
              onReload={host.reload}
              unavailable={
                host.missing
                  ? { kind: "missing", technical: `not found: ${host.entry.path}` }
                  : undefined
              }
            />
          </div>
        );
      })}
    </div>
  );
  return <OwningScopeProvider scope={owner}>{body}</OwningScopeProvider>;
}
const ownsFileNavFrame = (frame: LiveFrame, host: { entry: PaneEntry }) =>
  frame.slotId === "file-nav" && frame.documentPath === host.entry.path;

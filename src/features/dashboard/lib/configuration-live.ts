// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { liveHub, type LiveHub, type HubSnapshot } from "./live-hub";
import {
  configurationPrepare,
  type DashboardProfile,
  type WorkspaceConfig,
} from "@/tauri/commands";
import { followOrgProfile, resetProfiles, useDashboardProfileStore } from "./dashboard-profile";
import {
  applyWorkspaceConfig,
  resetWorkspaceConfigView,
  bindWorkspaceConfigPrincipal,
} from "@/shared/stores/config-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { useOrgStore } from "@/shared/stores/org-store";

export function profileFromView(view: HubSnapshot): DashboardProfile | null {
  if (!view.snapshot.length) return null;
  const profile: Record<string, unknown> = { slots: {}, chrome: {} };
  for (const row of view.snapshot) {
    const field = String(row.field);
    if (typeof row.key === "string") {
      const entries = (profile[field] ??= {}) as Record<string, unknown>;
      entries[row.key] = row.value;
    } else profile[field] = row.value;
  }
  return profile as unknown as DashboardProfile;
}
export function workspaceConfigFromView(view: HubSnapshot): WorkspaceConfig {
  return {
    repos: Object.fromEntries(view.snapshot.map((row) => [row.id, row.value])),
  } as WorkspaceConfig;
}

/** The principal whose profiles the dashboard-profile store holds: another one starts clean. */
let profilePrincipal: string | null = null;

/** App owns one bridge per storage principal and active org; retained pane registrations only read stores. */
export function connectConfiguration(
  principal: string,
  org: string | null,
  hub: LiveHub = liveHub,
) {
  const owner = crypto.randomUUID();
  let disposed = false;
  let shown = true;
  const frames: ReturnType<LiveHub["attach"]>[] = [];
  if (profilePrincipal !== principal) resetProfiles();
  profilePrincipal = principal;
  followOrgProfile(org);
  bindWorkspaceConfigPrincipal(principal);
  const ready = Promise.all(
    (["workspaceConfig", ...(org ? ["profile"] : [])] as Array<"profile" | "workspaceConfig">).map(
      async (topic) => {
        const view = await configurationPrepare(
          topic,
          principal,
          topic === "profile" ? org! : undefined,
        );
        if (disposed) return;
        const apply = (snapshot: HubSnapshot) => {
          if (disposed || !shown) return;
          if (topic === "profile")
            useDashboardProfileStore.setState({ profile: profileFromView(snapshot) });
          else applyWorkspaceConfig(workspaceConfigFromView(snapshot));
        };
        hub.seed(view);
        const frame = hub.attach(
          `native:configuration:${topic}:${owner}`,
          view.viewKey,
          (message) => {
            if (message.topic !== topic || !shown || disposed) return;
            void frame
              .subscribe(topic, undefined, { viewKey: view.viewKey })
              .then(apply)
              .catch(report);
          },
          shown,
          true,
        );
        frames.push(frame);
        apply(await frame.subscribe(topic, view.rev, { viewKey: view.viewKey }));
      },
    ),
  );
  function report(error: unknown) {
    if (!disposed) console.error(error);
  }
  return {
    ready,
    visible(value: boolean) {
      shown = value;
      for (const frame of frames) frame.visible(value);
    },
    dispose() {
      disposed = true;
      for (const frame of frames) frame.dispose();
    },
  };
}

export function useConfigurationLive(): void {
  const principal = useCurrentUser()?.principal;
  const org = useOrgStore((state) => state.activeOrgSlug);
  useEffect(() => {
    if (!principal) {
      resetProfiles();
      profilePrincipal = null;
      resetWorkspaceConfigView();
      return;
    }
    const connection = connectConfiguration(principal, org);
    const visibility = () => connection.visible(document.visibilityState !== "hidden");
    visibility();
    document.addEventListener("visibilitychange", visibility);
    void connection.ready.catch(console.error);
    return () => {
      document.removeEventListener("visibilitychange", visibility);
      connection.dispose();
    };
  }, [principal, org]);
}

// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import { useAgentModule } from "./use-agent-module";
import type { AgentModuleConfig, AgentModuleApi } from "./agent-module-config";

/**
 * SW-GS-12 (§9) — the generic, headless host that realizes the reusable `prewarm`
 * policy. Mounting the {@link useAgentModule} hook IS engaging (its session config's
 * `autoConnect` fires on mount); so "when to spawn" reduces to "when to mount the
 * hook", which this host gates on `active` × `prewarm`:
 *
 * - `prewarm: "onHostMount"` → engaged when `active` is true, or (uncontrolled,
 *   `active` omitted) the moment the host mounts.
 * - `prewarm: "lazy"` (default) → dormant until `active` goes true; uncontrolled it
 *   never engages. (The consumer drives `active` — that IS the imperative engage.)
 *
 * Kept strictly agent-agnostic: NO feature/vendor literal. A consumer wanting a
 * panel supplies the `children` render-prop (gets the bound api → register/promote);
 * a purely headless warm omits `children` and the host just keeps the session warm.
 *
 * Single-mount contract: a consumer must mount ONE host per `session.targetWorkspace`.
 * Two hosts on the same target would drive two mount-effects against the ONE shared
 * binding (state lives in `useAgentSessionStore` by surfaceKey) → connect churn.
 */
export interface AgentModuleHostProps {
  config: AgentModuleConfig;
  /**
   * Controlled engagement signal — the "when". Omit to let `prewarm` decide the
   * default (`"onHostMount"` → engage on mount; `"lazy"` → stay dormant).
   */
  active?: boolean;
  /**
   * Render-prop, invoked ONLY while engaged (the hook is mounted), with the bound
   * module api — use it to register a panel and/or promote. Omit for a headless
   * warm with no panel.
   */
  children?: (api: AgentModuleApi) => ReactNode;
}

export function AgentModuleHost({
  config,
  active,
  children,
}: AgentModuleHostProps): React.JSX.Element | null {
  const engaged = active ?? (config.prewarm ?? "lazy") === "onHostMount";
  if (!engaged) return null;
  // Rendered in a child so the `useAgentModule` hook is mounted/unmounted as a unit
  // when `engaged` flips — hooks can't be called conditionally in this component.
  return <MountedAgentModule config={config}>{children}</MountedAgentModule>;
}

function MountedAgentModule({
  config,
  children,
}: {
  config: AgentModuleConfig;
  children?: (api: AgentModuleApi) => ReactNode;
}): React.JSX.Element {
  const api = useAgentModule(config);
  return <>{children?.(api)}</>;
}

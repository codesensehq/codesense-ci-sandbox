// SPDX-License-Identifier: MIT
/**
 * SW-AW-5 — the reusable right-pane agent module: spawn/reuse ONE warm CLI-workspace
 * agent at a caller-specified cwd, mount it as a registered right-pane panel with the
 * standard agent chrome, and hand consumers a single shared session. Two seams:
 * `useAgentModule` (headless session provider) + the panel registration.
 */
export { useAgentModule } from "./use-agent-module";
export { AgentModulePanel, useRegisterAgentModulePanel } from "./agent-module-panel";
export { AgentModuleHost } from "./agent-module-host";
export type { AgentModuleHostProps } from "./agent-module-host";
export { shouldRegisterPaneChip } from "./agent-module-config";
export type { AgentModuleConfig, AgentModuleApi, PaneVisibility } from "./agent-module-config";

// SW-AW-7 — the `answerablePrompts` slot surface: consumers render the pending ask
// their own way and build the reply with these builders (single import site; no
// deep reach into `@/shared/lib/managed-session`).
export type { AskUserRequest, AskUserResponse } from "@/shared/lib/managed-session";
export {
  acceptAnswer,
  declineAnswer,
  cancelAnswer,
  handledAnswer,
} from "@/shared/lib/managed-session";

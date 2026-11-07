// SPDX-License-Identifier: MIT
// The active-workspace terminal-tab, per-session status/progress and status-mark read
// surface. The implementation lives inside the CLI workspace module's store door
// (`cli-workspace/store/terminal-tab-selectors.ts`), where subscribing to the
// viewer store is legal; this file is the stable import path the viewer feature
// and its consumers use.
export {
  getActiveWorkspaceTabs,
  getActiveWorkspaceActiveTab,
  useActiveWorkspaceTabs,
  useActiveWorkspaceActiveTab,
  useSessionAiStatus,
  getSessionAiStatus,
  useSessionProgress,
  useHiddenTerminalTabs,
  useScopeMark,
  useTerminalLives,
  type DotScope,
  type ScopeMark,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";

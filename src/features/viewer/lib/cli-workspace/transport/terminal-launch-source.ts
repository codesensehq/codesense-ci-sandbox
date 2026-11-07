// SPDX-License-Identifier: MIT
/**
 * Transport seam — session launch concerns: agent MCP scoping, live-agent
 * probing, the agent provider set, and the epic-workspace assembly DTOs. A
 * sanctioned `@/tauri/commands` importer for the CLI workspace module (NFR5).
 *
 * The epic-workspace runtime commands are injected at their call sites (via
 * dependency parameters), so only their DTO types are re-exported here.
 */

export {
  projectMcpScopeConfig,
  sessionHasLiveAgent,
  AGENT_PROVIDERS,
  remoteShellTab,
  listPreparedRemoteWorkspaces,
} from "@/tauri/commands";

export type {
  AgentProvider,
  AgentAccountRequirement,
  AgentAccountResolution,
  McpScopeLaunch,
  McpScopeOutcome,
  ScopedExecution,
  ResolveAgentAccountInput,
  AgentSessionRow,
  EnsureEpicWorkspaceParams,
  EpicWorkspaceHandle,
  AssembleEpicResult,
  RemoteShellTabRequest,
  RemoteShellLaunch,
  RemoteTabOutcome,
  PreparedRemoteWorkspaceChoice,
} from "@/tauri/commands";

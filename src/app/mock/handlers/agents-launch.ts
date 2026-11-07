// SPDX-License-Identifier: MIT
import { isAgentRuntime } from "@/shared/lib/agent-runtimes";
import { hermesGolden, hermesAdmission } from "./hermes-golden";
import type { AgentRuntime } from "@/tauri/commands";
import type { HandlerRegistry } from "../mock-router";
import type { AgentLaunchWarning, LaunchMode, PreparedAgentLaunch } from "@/tauri/commands";
import { parseTeamKey } from "@/shared/lib/team-key";

/**
 * The mock backend for a named agent's interactive launch (`agent_launch_prepare`). Two team
 * agents — a Claude one with a permission rule and a default prompt, and a Codex one — resolve
 * the way the real command does: a rendered key as is, or a bare id in the launch's team then
 * the personal space; the starters (Claude, Codex, their unattended twins)
 * resolve to `builtin/<id>`. A launch that names no agent runs `builtin/claude`, or
 * `builtin/claude-unattended` (permission checks skipped) when it runs in the background. A Claude launch gets a per-session `--settings` file outside the
 * workspace; a Codex launch gets its scoped home as the environment binding. A prompt whose
 * leading `/command` the agent does not start with is warned about, never refused.
 */

interface MockAgent {
  id: string;
  /** Its display name, which titles its terminal tab. */
  name: string;
  runtime: AgentRuntime;
  args: string[];
  revision: number;
  /** The `/commands` the agent starts with. */
  commands: string[];
  defaultPrompt: string | null;
  mcpAllow: string[];
  /** A built-in starter: a bare id resolves to `builtin/<id>`. */
  builtin?: boolean;
  startupSettings?: Record<string, unknown> | null;
}

const AGENTS = [
  ...[hermesGolden(), hermesGolden(true), hermesGolden(false, true)].map(
    ({ definition }): MockAgent => {
      return {
        id: definition.id,
        name: definition.name,
        runtime: definition.runtime,
        args: definition.args ?? [],
        revision: definition.revision,
        commands: [],
        defaultPrompt: definition.default_prompt ?? null,
        mcpAllow: definition.mcp?.servers ?? [],
        builtin: definition.created_by === "CodeSense",
        startupSettings: definition.startup_settings,
      };
    },
  ),
  {
    id: "triage",
    name: "Slack triage",
    runtime: "claude",
    args: ["--model", "opus"],
    revision: 3,
    commands: ["/triage-slack", "/bmad-help"],
    defaultPrompt: "/triage-slack",
    mcpAllow: ["slack"],
  },
  {
    id: "claude",
    name: "Claude",
    runtime: "claude",
    args: [],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
    builtin: true,
  },
  {
    id: "codex",
    name: "Codex",
    runtime: "codex",
    args: [],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
    builtin: true,
  },
  {
    id: "opencode",
    name: "OpenCode",
    runtime: "opencode",
    args: [],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
    builtin: true,
  },
  {
    id: "claude-unattended",
    name: "Claude (unattended)",
    runtime: "claude",
    args: ["--dangerously-skip-permissions"],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
    builtin: true,
  },
  {
    id: "codex-unattended",
    name: "Codex (unattended)",
    runtime: "codex",
    args: [],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
    builtin: true,
  },
  {
    id: "opencode-unattended",
    name: "OpenCode (unattended)",
    runtime: "opencode",
    args: ["--auto"],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
    builtin: true,
  },
  {
    id: "reviewer",
    name: "Reviewer",
    runtime: "codex",
    args: ["--model", "gpt-5"],
    revision: 1,
    commands: [],
    defaultPrompt: null,
    mcpAllow: [],
  },
].filter((agent) => isAgentRuntime(agent.runtime)) as MockAgent[];

const SETTINGS_DIR = "/private/var/folders/mock/T/codesense-launch-settings";
const SCOPE_DIR = "/private/var/folders/mock/T/codesense-mcp-scope";

/** `org:team` → `org/team`, the key prefix of a team agent. */
function teamPrefix(team: unknown): string | null {
  if (typeof team !== "string") return null;
  const { orgSlug, teamName } = parseTeamKey(team);
  return orgSlug && teamName ? `${orgSlug}/${teamName}` : null;
}

function find(reference: string, team: unknown): { agent: MockAgent; key: string } {
  const prefix = teamPrefix(team);
  const keyed = reference.includes("/");
  const id = keyed ? reference.slice(reference.lastIndexOf("/") + 1) : reference;
  const agent = AGENTS.find((a) => a.id === id);
  if (!agent) {
    const where = prefix ? `${prefix} or your private space` : "your private space";
    throw new Error(`no agent ${JSON.stringify(reference)} in ${where}`);
  }
  const key = keyed
    ? reference
    : agent.builtin
      ? `builtin/${agent.id}`
      : prefix
        ? `${prefix}/${agent.id}`
        : `_personal/1/personal/${id}`;
  return { agent, key };
}

function commandWarning(agent: MockAgent, prompt: string | null): AgentLaunchWarning[] {
  const command = prompt?.trim().match(/^\/[\w:-]+/)?.[0];
  if (!command || agent.commands.includes(command)) return [];
  return [
    {
      code: "command_not_in_manifest",
      message: `this agent doesn't start with ${command} — add it to the agent or the run won't find it`,
    },
  ];
}

export function prepareAgentLaunch(args: Record<string, unknown>): PreparedAgentLaunch {
  const reference = String(args.agentKey ?? "").trim();
  const sessionId = String(args.sessionId ?? "");
  const { agent, key } = find(reference, args.team);
  const prompt = typeof args.prompt === "string" && args.prompt.trim() ? args.prompt : null;
  const mode = (args.mode as LaunchMode | null) ?? "interactive";
  const claude = agent.runtime === "claude";
  const opencode = agent.runtime === "opencode";
  const hermes =
    agent.runtime === "hermes"
      ? hermesGolden(agent.id.endsWith("-unattended"), agent.id === "hermes-open")
      : null;
  if (hermes) {
    const admission = hermesAdmission({
      ...hermes.definition,
      args: agent.args,
      startup_settings: agent.startupSettings,
      default_prompt: prompt ?? agent.defaultPrompt ?? "",
    });
    if (!admission.allowed) throw new Error(admission.reason ?? "Launch is blocked");
  }
  const launchArgs = claude
    ? [...agent.args, "--settings", `${SETTINGS_DIR}/${sessionId}--0f3a.json`]
    : hermes && mode === "interactive"
      ? [
          ...hermes.interactive_startup.args,
          ...agent.args,
          ...((prompt ?? agent.defaultPrompt)?.trim()
            ? [`${hermes.interactive_startup.prompt_flag}${prompt ?? agent.defaultPrompt}`]
            : []),
        ]
      : agent.args;
  return {
    name: agent.name,
    agentPath: `/opt/bin/${agent.runtime}`,
    launch: {
      id: key,
      binary: agent.runtime,
      args: launchArgs,
      escalating: hermes?.launch.escalating ?? false,
      mcp_allow: agent.mcpAllow,
      mode,
      env_suppress: hermes
        ? hermes.launch.env_suppress
        : opencode
          ? [
              "OPENAI_API_KEY",
              "ANTHROPIC_API_KEY",
              "OPENCODE_API_KEY",
              "GOOGLE_API_KEY",
              "GEMINI_API_KEY",
              "OPENROUTER_API_KEY",
            ]
          : [claude ? "ANTHROPIC_API_KEY" : "OPENAI_API_KEY"],
    },
    account: {
      runtime: agent.runtime,
      overrideAccountId: typeof args.accountId === "string" ? args.accountId : null,
      overrideAccountGeneration:
        typeof args.accountGeneration === "number" ? args.accountGeneration : null,
    },
    agent: `${key}@${agent.revision}`,
    manifestHash: `sha256:mock-${agent.id}-${agent.revision}`,
    warnings: commandWarning(agent, prompt ?? agent.defaultPrompt),
    defaultPrompt: hermes && mode === "interactive" ? null : agent.defaultPrompt,
    ...(hermes && mode === "interactive" ? { promptInArgv: true } : {}),
    scope: claude
      ? {
          extraArgs: ["--mcp-config", `${SCOPE_DIR}/${sessionId}.json`, "--strict-mcp-config"],
          environmentBinding: null,
        }
      : { extraArgs: [], environmentBinding: `mock-scope-${sessionId}` },
  };
}

export function agentLaunchHandlers(): HandlerRegistry {
  return {
    agent_launch_prepare: (args) => prepareAgentLaunch(args),
  };
}

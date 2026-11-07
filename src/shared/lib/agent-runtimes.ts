// SPDX-License-Identifier: MIT
import type { AgentRuntime } from "@/tauri/commands";

import modelSchemas from "../../../src-tauri/src/docs_schemas.generated.json";

/** Registered CLI metadata, exported from AgentCli::ALL + UNLISTED and adapter facts.
 *  Unlisted rows stay available as metadata; every fresh-launch choice filters listed. */
export const REGISTERED_AGENT_CLIS = modelSchemas.agentClis as readonly {
  runtime: AgentRuntime;
  /** The executable a launch runs and the dependency check finds. */
  binary: string;
  listed: boolean;
  supportsRemoteLaunch: boolean;
  preview: boolean;
  /** Its model comes from a configured endpoint, not from its launch args. */
  modelEndpoint: boolean;
  /** Its permissions are described, not edited, in the agent editor. */
  permissionsDisplayOnly: boolean;
}[];

type RegisteredCli = (typeof REGISTERED_AGENT_CLIS)[number];

/** The available CLIs in backend registry order. Promotion needs only a Rust re-export. */
export const AGENT_RUNTIMES = REGISTERED_AGENT_CLIS.filter((row) => row.listed).map(
  (row) => row.runtime,
);

/** `AGENT_RUNTIMES` as a runtime set for membership checks. */
export const AGENT_RUNTIME_SET: ReadonlySet<string> = new Set(AGENT_RUNTIMES);

/** Available CLIs whose desktop adapter supports native remote launch. */
export const REMOTE_LAUNCH_CLIS = REGISTERED_AGENT_CLIS.filter(
  (row) => row.listed && row.supportsRemoteLaunch,
).map((row) => row.runtime);

/** The native launch choices for a remote tab: a shell, or a remotely-launchable CLI. */
export const REMOTE_LAUNCHES = ["shell", ...REMOTE_LAUNCH_CLIS] as const;

/** Every `launch` value the `open_remote_tab` wire accepts (`NativeLaunch`): a shell or any
 *  agent CLI. A restored or hand-passed value must not be dropped even when that CLI is not
 *  offered for a fresh remote launch. */
export const NATIVE_LAUNCHES = ["shell", ...AGENT_RUNTIMES] as const;

/** Whether a detected CLI name is one of CodeSense's own agent CLIs. */
export function isAgentRuntime(cli: string | null | undefined): cli is AgentRuntime {
  return cli != null && AGENT_RUNTIME_SET.has(cli);
}

/** Recognition includes restored/unlisted agents; fresh-launch choices still use listed. */
export function isRegisteredAgentRuntime(cli: string | null | undefined): cli is AgentRuntime {
  return cli != null && REGISTERED_AGENT_CLIS.some((row) => row.runtime === cli);
}

/** What a preview CLI's label says is still to come. */
export const PREVIEW_RUNTIME_NOTE =
  "Some features, such as usage tracking and session resume, arrive in later releases.";

/** The registered CLI `runtime` names, if any. */
export function registeredCli(runtime: string | null | undefined): RegisteredCli | undefined {
  return REGISTERED_AGENT_CLIS.find((row) => row.runtime === runtime);
}

/** The registered CLI whose executable is `binary`, if any. */
export function registeredCliOfBinary(
  binary: string | null | undefined,
): RegisteredCli | undefined {
  return REGISTERED_AGENT_CLIS.find((row) => row.binary === binary);
}

/** Whether `cli` is offered as a preview, which the New agent templates and Settings label. */
export function isPreviewRuntime(cli: string | null | undefined): boolean {
  return registeredCli(cli)?.preview === true;
}

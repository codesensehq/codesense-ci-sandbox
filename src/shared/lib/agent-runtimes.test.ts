// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import modelSchemas from "../../../src-tauri/src/docs_schemas.generated.json";
import {
  AGENT_RUNTIMES,
  REGISTERED_AGENT_CLIS,
  AGENT_RUNTIME_SET,
  NATIVE_LAUNCHES,
  REMOTE_LAUNCH_CLIS,
  REMOTE_LAUNCHES,
  isAgentRuntime,
  isRegisteredAgentRuntime,
} from "./agent-runtimes";

describe("agent runtimes registry", () => {
  it("mirrors the backend AgentCli::ALL in canonical order", () => {
    expect(AGENT_RUNTIMES).toEqual(
      modelSchemas.agentClis.filter((row) => row.listed).map((row) => row.runtime),
    );
    expect(AGENT_RUNTIMES).toEqual(modelSchemas.schemas["agent-yaml"].properties.runtime.enum);
  });

  it("derives every launch shape from the one list", () => {
    expect(REMOTE_LAUNCHES).toEqual(["shell", ...REMOTE_LAUNCH_CLIS]);
    expect(NATIVE_LAUNCHES).toEqual(["shell", ...AGENT_RUNTIMES]);
    expect([...AGENT_RUNTIME_SET].sort()).toEqual([...AGENT_RUNTIMES].sort());
  });

  it("the inbox gate accepts exactly the registered CLIs", () => {
    for (const cli of AGENT_RUNTIMES) expect(isAgentRuntime(cli), cli).toBe(true);
    expect(isAgentRuntime("wombat")).toBe(false);
    expect(isAgentRuntime("gemini")).toBe(false);
    expect(isAgentRuntime(null)).toBe(false);
    expect(isAgentRuntime(undefined)).toBe(false);
  });

  it("keeps registered metadata while every choice follows backend listing facts", () => {
    expect(REGISTERED_AGENT_CLIS.some((row) => row.runtime === "hermes")).toBe(true);
    for (const row of REGISTERED_AGENT_CLIS) {
      expect(isAgentRuntime(row.runtime)).toBe(row.listed);
      expect(isRegisteredAgentRuntime(row.runtime)).toBe(true);
      expect(NATIVE_LAUNCHES.includes(row.runtime)).toBe(row.listed);
      expect(REMOTE_LAUNCH_CLIS.includes(row.runtime)).toBe(row.listed && row.supportsRemoteLaunch);
    }
  });
});

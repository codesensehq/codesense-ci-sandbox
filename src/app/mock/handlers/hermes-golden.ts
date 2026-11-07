// SPDX-License-Identifier: MIT
import golden from "../../../../src-tauri/src/commands/agents_launch/adapter_launch_golden.json";
import type { AgentDefinition, AgentLaunchProfile, AgentGeneratedEntry } from "@/tauri/commands";

/** Mock Hermes facts come from the real managed adapter's recorded launches. */
export function hermesGolden(unattended = false, configured = false, headless = false) {
  const mode = headless ? "Headless" : "Interactive";
  const key = configured
    ? `hermes-open ${mode}`
    : `builtin/hermes${unattended ? "-unattended" : ""} ${mode}`;
  const entry = (
    golden as unknown as Record<
      string,
      {
        definition: AgentDefinition;
        launch: AgentLaunchProfile;
        generated: AgentGeneratedEntry[];
        interactive_startup: { args: string[]; prompt_flag: string };
        launch_admission: import("@/tauri/commands").AgentLaunchAdmission;
        spawn: { argv: string[]; env: [string, string][] };
      }
    >
  )[key];
  if (!entry) throw new Error(`Missing adapter launch golden: ${key}`);
  return structuredClone(entry);
}

export function hermesGenerated(unattended = false, headless = false): AgentGeneratedEntry[] {
  const generated = hermesGolden(unattended, false, headless).generated;
  if (!Array.isArray(generated))
    throw new Error(
      "Hermes adapter golden has no generated entries; regenerate it before running mocks",
    );
  return generated;
}

export function hermesAdmission(
  definition: AgentDefinition,
): import("@/tauri/commands").AgentLaunchAdmission {
  const mode = hermesGolden(
    definition.id.endsWith("-unattended"),
    false,
    definition.launch === "headless",
  ).launch_admission;
  if (!mode) throw new Error("Hermes adapter golden has no admission metadata; regenerate it");
  let result = mode;
  if (definition.startup_settings?.model) {
    const configured = hermesGolden(false, true, definition.launch === "headless").launch_admission;
    if (!configured)
      throw new Error("Hermes adapter golden has no admission metadata; regenerate it");
    result = {
      ...configured,
      permission_display: mode.permission_display,
      model_arguments: mode.model_arguments,
    };
  }
  if (
    result.initial_prompt_max_bytes !== undefined &&
    new TextEncoder().encode(definition.default_prompt ?? "").length >
      result.initial_prompt_max_bytes
  ) {
    result.allowed = false;
    result.reason = result.initial_prompt_too_long ?? null;
  }
  return result;
}

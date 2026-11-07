// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { DependencyStatus } from "@/tauri/commands";
import { mockDependencyStatus } from "../fixtures";

/**
 * The manual dependency re-check backing the deps-gate's retry button. The
 * dependency status is scenario-independent (the git/agent binaries being
 * installed is orthogonal to how far onboarding has progressed), so it mirrors
 * the always-ready boot fixture.
 */
export function setupHandlers(): HandlerRegistry {
  return {
    recheck_dependencies: (): DependencyStatus => mockDependencyStatus,
    resolve_agent_cli: (args): string | null =>
      typeof args.basename === "string"
        ? mockDependencyStatus.agents[args.basename]?.available
          ? (mockDependencyStatus.agents[args.basename]?.path ?? null)
          : null
        : null,
  };
}

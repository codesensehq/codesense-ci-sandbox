// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { worktreeHandlers } from "./worktree";
import { resolveScenario } from "../scenario";
import type {
  SetupProvenanceReport,
  WorktreeConfigProbe,
  WorktreeRunResult,
} from "@/tauri/commands";
import type { WorktreeConfig } from "@/shared/types/worktree-config";

describe("worktreeHandlers", () => {
  it("reports a present config with a parsed WorktreeConfig when the scenario has one", () => {
    const h = worktreeHandlers(resolveScenario("?scenario=onboarded"));
    const probe = h.probe_worktree_config({}) as WorktreeConfigProbe;
    expect(probe.status).toEqual({ kind: "present" });
    expect(probe.config?.version).toBe(1);

    const config = h.get_worktree_config({}) as WorktreeConfig | null;
    expect(config?.setup.length).toBeGreaterThan(0);
  });

  it("reports a missing config (null) when the scenario has none", () => {
    const h = worktreeHandlers(resolveScenario("?scenario=fresh"));
    const probe = h.probe_worktree_config({}) as WorktreeConfigProbe;
    expect(probe.status).toEqual({ kind: "missing" });
    expect(probe.config).toBeNull();
    expect(h.get_worktree_config({})).toBeNull();
  });

  it("reports local provenance, with a config hash only when a config exists", () => {
    const withConfig = worktreeHandlers(resolveScenario("?scenario=onboarded"));
    const present = withConfig.evaluate_setup_provenance({}) as SetupProvenanceReport;
    expect(present.provenance).toBe("local");
    expect(present.config_hash).not.toBeNull();

    const withoutConfig = worktreeHandlers(resolveScenario("?scenario=fresh"));
    const absent = withoutConfig.evaluate_setup_provenance({}) as SetupProvenanceReport;
    expect(absent.config_hash).toBeNull();
  });

  it("resolves a story's default start point to a remote ref, configured or not", () => {
    const h = worktreeHandlers(resolveScenario("?scenario=fresh"));
    expect(h.story_default_start_point({ repoPath: "/r", defaultStartPoint: null })).toBe(
      "refs/remotes/origin/main",
    );
    expect(h.story_default_start_point({ repoPath: "/r", defaultStartPoint: "" })).toBe(
      "refs/remotes/origin/main",
    );
    expect(h.story_default_start_point({ repoPath: "/r", defaultStartPoint: "develop" })).toBe(
      "refs/remotes/origin/develop",
    );
  });

  it("runs setup to a clean single-step success", () => {
    const h = worktreeHandlers(resolveScenario("?scenario=fresh"));
    const result = h.run_worktree_setup({}) as WorktreeRunResult;
    expect(result.exit_code).toBe(0);
    expect(result.results).toHaveLength(1);
    expect(result.results[0].passed).toBe(true);
    expect(result.results[0].phase).toBe("setup");
  });
});

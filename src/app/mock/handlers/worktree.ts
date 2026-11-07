// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type {
  SetupProvenanceReport,
  WorktreeConfigProbe,
  WorktreeRunResult,
} from "@/tauri/commands";
import { WORKTREE_CONFIG_VERSION } from "@/shared/types/worktree-config";
import type { WorktreeConfig } from "@/shared/types/worktree-config";
import type { MockScenario } from "../scenario";

/** A representative worktree.yaml the mock returns when the scenario has a
 *  config on disk — one setup command so `run_worktree_setup` has a real step
 *  to report, plus the conventional branch prefix. */
const MOCK_WORKTREE_CONFIG: WorktreeConfig = {
  version: WORKTREE_CONFIG_VERSION,
  setup: ["pnpm install"],
  branch_prefix: "feat/",
  default_start_point: "main",
};

/** A stable non-cryptographic stand-in for the SHA-256 of the config bytes.
 *  The provenance gate only compares it for equality, so a fixed string is
 *  enough for the mock — no hashing dependency pulled in. */
const MOCK_CONFIG_HASH = "mockconfighash0000000000000000000000000000000000000000000000000000";

/**
 * Worktree-config probes + the setup runner (SW-SO-2 / SW-WT-9 / SW-WT-1).
 * Config presence is scenario-driven off `worktreePresent`; provenance is always
 * `local` (a mock config is authored here, never teammate-synced); the runner
 * reports a clean single-step success without mutating the scenario.
 */
export function worktreeHandlers(scenario: MockScenario): HandlerRegistry {
  const present = scenario.worktreePresent;
  const config = present ? MOCK_WORKTREE_CONFIG : null;

  return {
    probe_worktree_config: (): WorktreeConfigProbe => ({
      status: present ? { kind: "present" } : { kind: "missing" },
      config,
    }),

    get_worktree_config: (): WorktreeConfig | null => config,

    evaluate_setup_provenance: (): SetupProvenanceReport => ({
      provenance: "local",
      config_hash: present ? MOCK_CONFIG_HASH : null,
    }),

    // The mock clone has `origin/main`, so a branch name resolves to its remote ref,
    // as `configured_start_point` / `resolve_story_start_point` do on a real clone.
    story_default_start_point: (args): string => {
      const configured = args.defaultStartPoint;
      const name = typeof configured === "string" && configured ? configured : "main";
      return name.startsWith("refs/") ? name : `refs/remotes/origin/${name}`;
    },

    run_worktree_setup: (): WorktreeRunResult => ({
      results: [
        {
          command: MOCK_WORKTREE_CONFIG.setup[0],
          passed: true,
          stdout: "mock: setup step completed",
          stderr: "",
          exit_code: 0,
          phase: "setup",
          step_index: 0,
        },
      ],
      exit_code: 0,
    }),
  };
}

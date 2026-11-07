// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import { reconfigureWarmPool, setTerminalShellOverride } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

/** PT-8 default pool size when the setting is unset. Mirrors DEFAULT_SETTINGS. */
const WARM_POOL_DEFAULT = 2;

/**
 * PT-8: keep the Rust warm terminal pool in sync with settings.
 *
 * The Rust pool starts empty; this fires once at launch (after settings
 * hydrate) to fill it (AC-1), and again whenever the pool size, starship preset,
 * shell-integration setting, or shell override changes — Rust drains the
 * now-stale members and refills with the new params (AC-15). SW-LN-35: the shell
 * override is pushed to Rust first, so the refilled members run the new shell.
 * Fire-and-forget; the calls are no-ops outside the Tauri shell.
 */
export function useWarmPool(): void {
  const size = useViewerStore((s) => s.settings.terminalWarmPoolSize);
  const starshipPreset = useViewerStore((s) => s.settings.starshipPreset);
  const shellIntegration = useViewerStore((s) => s.settings.shellIntegration);
  const terminalShell = useViewerStore((s) => s.settings.terminalShell);

  useEffect(() => {
    setTerminalShellOverride(terminalShell)
      .catch((e) => logger.debug("pty", `setTerminalShellOverride failed: ${e}`))
      .then(() =>
        reconfigureWarmPool(size ?? WARM_POOL_DEFAULT, starshipPreset, shellIntegration ?? true),
      )
      .catch((e) => logger.debug("pty", `reconfigureWarmPool failed: ${e}`));
  }, [size, starshipPreset, shellIntegration, terminalShell]);
}

// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

import { EmptyState } from "@/shared/components/empty-state";
import { getErrorMessage } from "@/shared/lib/error-message";
import { logger } from "@/shared/lib/logger";
import { getLogFilePath } from "@/tauri/commands";

import { AppSplash } from "./app-splash";

/** SW-FU-B3: how long a boot splash may stay up before it says what it is
 *  waiting on and offers a way out. */
export const BOOT_SPLASH_WATCHDOG_MS = 15_000;

/** The boot gate a watched splash is holding. */
export type BootWaitStage = "startup" | "discovery" | "workspace";

const STALLED_COPY: Record<BootWaitStage, string> = {
  startup: "Still starting CodeSense",
  discovery: "Still loading your teams",
  workspace: "Still setting up your team",
};

async function openLogs(): Promise<void> {
  try {
    await revealItemInDir(await getLogFilePath());
  } catch (e) {
    logger.warn("boot", `could not open the log folder: ${getErrorMessage(e)}`);
  }
}

/**
 * A boot problem with its reason and a way forward: **Try again** and
 * **Open logs**. Never a dead end.
 */
export function BootProblemView({
  heading,
  body,
  onRetry,
}: {
  heading: string;
  body: string;
  onRetry: () => void;
}) {
  return (
    <div className="flex-1 flex items-center justify-center" data-testid="boot-problem">
      <EmptyState
        size="page"
        icon={<AlertTriangle />}
        title={heading}
        body={body}
        action={{ label: "Try again", onClick: onRetry }}
        secondaryAction={{ label: "Open logs", onClick: () => void openLogs() }}
      />
    </div>
  );
}

/**
 * The boot splash (startup, org discovery, the first-run team create). After
 * {@link BOOT_SPLASH_WATCHDOG_MS} it turns into a {@link BootProblemView}
 * naming the gate, while the wait carries on underneath (App leaves the gate on
 * its own as soon as it resolves). App renders it at one tree position for
 * every gate, so the timer spans them: the whole wait is bounded, not each step.
 */
export function WatchedSplash({ stage }: { stage: BootWaitStage }) {
  const [stalled, setStalled] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setStalled(true), BOOT_SPLASH_WATCHDOG_MS);
    return () => clearTimeout(timer);
  }, []);
  useEffect(() => {
    if (stalled) {
      logger.warn(
        "boot",
        `splash watchdog tripped after ${BOOT_SPLASH_WATCHDOG_MS}ms at: ${stage}`,
      );
    }
  }, [stalled, stage]);

  if (!stalled) return <AppSplash />;
  return (
    <BootProblemView
      heading={STALLED_COPY[stage]}
      body="This is taking longer than it should. Try again, or open the logs to see what CodeSense is waiting on."
      onRetry={() => window.location.reload()}
    />
  );
}

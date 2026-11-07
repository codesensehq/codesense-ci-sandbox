// SPDX-License-Identifier: MIT
// Runs a heavy build job through an optional local job queue when one is installed, and runs
// the same command directly when it is not (the usual case).
import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

export function runBuildJob(label, [command, ...args], options = {}) {
  const home = (options.env ?? process.env).HOME;
  const queue = home && join(home, ".local/bin/mq");
  const result =
    queue && existsSync(queue)
      ? spawnSync(
          queue,
          ["run", "--slot", "build", "--who", label, "--", command, ...args],
          options,
        )
      : spawnSync(command, args, options);
  // A command that cannot start (e.g. no cargo on PATH) reports its spawn error, not a bare status.
  if (result.error) throw result.error;
  return result;
}

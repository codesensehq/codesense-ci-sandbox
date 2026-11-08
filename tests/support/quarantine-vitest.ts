// SPDX-License-Identifier: MIT
import { quarantineId } from "../../scripts/lib/quarantine.mjs";
import { beforeEach } from "vitest";
import { quarantineReason } from "./quarantine";

beforeEach((context) => {
  const names = [context.task.name];
  for (let suite = context.task.suite; suite && suite !== context.task.file; suite = suite.suite) {
    names.unshift(suite.name);
  }
  const reason = quarantineReason(quarantineId("vitest", context.task.file.filepath, names));
  if (reason) {
    console.warn(reason);
    context.skip(reason);
  }
});

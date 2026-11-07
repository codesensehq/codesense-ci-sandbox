// SPDX-License-Identifier: MIT
// @vitest-environment node
import { readFileSync, writeFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { dumpPath, fixtureDumps, serializeDump } from "../__test-utils__/fixture-dumps";

// `pnpm run fixtures:dump` runs this file with UPDATE_FIXTURE_DUMPS=1 to
// rewrite the dumps instead of checking them.
const update = process.env.UPDATE_FIXTURE_DUMPS === "1";

describe("checked-in fixture dumps", () => {
  it("match a fresh headless replay (run `pnpm run fixtures:dump` if not)", async () => {
    for (const [name, dump] of await fixtureDumps()) {
      if (update) writeFileSync(dumpPath(name), serializeDump(dump));
      expect(readFileSync(dumpPath(name), "utf8"), name).toBe(serializeDump(dump));
    }
  });
});

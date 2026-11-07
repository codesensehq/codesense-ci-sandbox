// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

describe("physical repository path authority", () => {
  it("keeps host-aware physical path formatting out of TypeScript", () => {
    const sources = [
      "src/shared/lib/team-content-path.ts",
      "src/shared/lib/build-synthetic-repo.ts",
    ].map((path) => readFileSync(join(process.cwd(), path), "utf8"));

    for (const source of sources) {
      expect(source).not.toContain("pathToTeamRepoDir");
      expect(source).not.toContain("TEAM_REPOS_DIR");
    }
  });
});

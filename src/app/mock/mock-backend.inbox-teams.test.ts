// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { createMockBackend } from "./mock-backend";
import type { OrgSummary } from "@/shared/types/domain";
import type { LoadedOrg } from "@/shared/types/registry";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { TEAM_CONTENT_PATH } from "./fixtures";
import { resolveScenario } from "./scenario";

it.each(["", "?terminals=demo", "?agents=teams", "?scenario=noTeamInOrg"])(
  "reports configured teams' content directories in mock mode %s",
  async (search) => {
    const invoke = createMockBackend(resolveScenario(search), search);
    const orgs = (await invoke("discover_local_orgs")) as OrgSummary[];
    for (const summary of orgs) {
      const { org } = (await invoke("load_org_cmd", { slug: summary.slug })) as LoadedOrg;
      expect(org).not.toBeNull();
      for (const team of org!.teams)
        expect(
          await invoke("check_path_exists", {
            path: pathToTeamContentDirSync(org!.org_root, team.name),
          }),
        ).toBe(true);
      expect(
        await invoke("check_path_exists", {
          path: pathToTeamContentDirSync(org!.org_root, "not-synced"),
        }),
      ).toBe(false);
    }
    if (search === "?terminals=demo")
      expect(await invoke("check_path_exists", { path: `${TEAM_CONTENT_PATH}/scratch` })).toBe(
        true,
      );
    if (search === "?scenario=noTeamInOrg")
      expect(await invoke("check_path_exists", { path: TEAM_CONTENT_PATH })).toBe(false);
  },
);

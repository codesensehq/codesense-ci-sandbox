// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { resolveTeamMainDocPreference } from "./team-main-doc";
const legacy = {
  repos: {
    "workspace:acme:planning:acme/api": { mainDoc: "Epic.md" },
    "workspace:acme:planning:acme/web": { mainDoc: "" },
  },
};
it("carries a unique valid legacy choice without source ordering and preserves source settings", () => {
  const resolved = resolveTeamMainDocPreference("acme:planning", legacy);
  expect(resolved.preference).toEqual({ kind: "ready", mainDoc: "Epic.md" });
  expect(resolved.config.repos["team:acme:planning"]).toEqual({ mainDoc: "Epic.md" });
  expect(resolved.config.repos["workspace:acme:planning:acme/api"]).toEqual({ mainDoc: "Epic.md" });
  expect(resolveTeamMainDocPreference("acme:planning", resolved.config).config).toBe(
    resolved.config,
  );
});
it("explicit empty wins and conflicting choices stay nonblocking", () => {
  expect(
    resolveTeamMainDocPreference("acme:planning", {
      repos: { ...legacy.repos, "team:acme:planning": { mainDoc: "" } },
    }).preference,
  ).toEqual({ kind: "ready", mainDoc: "" });
  const conflict = resolveTeamMainDocPreference("acme:planning", {
    repos: { ...legacy.repos, "workspace:acme:planning:acme/web": { mainDoc: "Summary.md" } },
  });
  expect(conflict.preference.kind).toBe("conflict");
  expect(conflict.preference.mainDoc).toBe("");
  expect(conflict.config.repos["team:acme:planning"]).toBeUndefined();
});
it("does not migrate invalid filename conventions or another team's setting", () => {
  expect(
    resolveTeamMainDocPreference("acme:planning", {
      repos: {
        "workspace:acme:other:acme/api": { mainDoc: "Other.md" },
        "workspace:acme:planning:acme/web": { mainDoc: "../outside.md" },
      },
    }).preference.mainDoc,
  ).toBe("");
});

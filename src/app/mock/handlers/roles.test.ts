// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { MockEventBus } from "../mock-events";
import { roleHandlers } from "./roles";

describe("roleHandlers", () => {
  it("lists the team's roles and saves a role by id, keeping the others", async () => {
    const h = roleHandlers(new MockEventBus());
    const before = (await h.roles_list({ teamKey: "acme/widgets:default" })) as { id: string }[];
    expect(before.map((r) => r.id)).toEqual(["triage", "reviewer"]);
    const saved = await h.role_save({
      teamKey: "acme/widgets:default",
      config: { id: "triage", name: "On-call triage", icon: null, defaultHandler: null },
    });
    expect(saved).toMatchObject({ id: "triage", name: "On-call triage" });
    const after = (await h.roles_list({ teamKey: "acme/widgets:default" })) as {
      id: string;
      name: string;
    }[];
    expect(after.map((r) => r.name)).toEqual(["On-call triage", "Reviewer"]);
  });

  it("adds a role it has not seen", async () => {
    const h = roleHandlers(new MockEventBus());
    await h.role_save({
      teamKey: "acme/widgets:default",
      config: { id: "docs", name: "Docs", icon: null, defaultHandler: null },
    });
    const rows = (await h.roles_list({ teamKey: "acme/widgets:default" })) as { id: string }[];
    expect(rows.map((r) => r.id)).toContain("docs");
  });

  it("keeps one team's roles apart from another's", async () => {
    const h = roleHandlers(new MockEventBus());
    expect(await h.roles_list({ teamKey: "other:team" })).toEqual([]);
  });
});

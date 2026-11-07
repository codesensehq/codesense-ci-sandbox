// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  buildTargetPicker,
  crossOrgReason,
  noOtherReposLabel,
  selectableTargetIds,
  CREATE_TEAM_NEXT_STEP,
} from "@/features/context-mobility/target-picker";
import {
  personalContextId,
  teamAllContextId,
  teamRepoContextId,
  type Context,
} from "@/shared/lib/context-model";
import type { Org, Team } from "@/shared/types/domain";

const BANNED = /\b(provider|overlay|layer|scratch|TeamWorkspace|TeamRepo|Workspace)\b/;

function org(slug: string, display: string, teams: Team[]): Org {
  return {
    slug,
    display_name: display,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams,
    org_root: `/mock/${slug}`,
  };
}

function team(name: string, repos: { owner: string; repo: string }[] = [], archived = false): Team {
  return { name, source_repos: repos, ...(archived ? { archived: true } : {}) };
}

const personalSource: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Personal",
};

const teamAllSource = (slug: string, teamName: string): Context => ({
  id: teamAllContextId(slug, teamName),
  kind: "team-all",
  sharing: "shared",
  label: `All of ${teamName}`,
  teamName,
});

describe("buildTargetPicker", () => {
  it("lists same-org team targets + always Personal; excludes the source", () => {
    const orgs = [org("acme", "Acme", [team("default", [{ owner: "acme", repo: "api" }])])];
    const picker = buildTargetPicker({
      source: teamAllSource("acme", "default"),
      activeOrgSlug: "acme",
      orgs,
    });

    expect(picker.personal?.context.id).toBe(personalContextId());
    expect(picker.personal?.disabled).toBe(false);
    // Same-org: "All of default" is the source (excluded); the repo row remains.
    const sameIds = picker.sameOrg.map((r) => r.context.id);
    expect(sameIds).not.toContain(teamAllContextId("acme", "default"));
    expect(sameIds).toContain(teamRepoContextId("acme", "default", { owner: "acme", repo: "api" }));
    expect(picker.sameOrg.every((r) => r.disabled === false)).toBe(true);
    expect(picker.crossOrg).toHaveLength(0);
    expect(picker.nextStep).toBeNull();
  });

  it("omits Personal from its own picker when the source is Personal", () => {
    const orgs = [org("acme", "Acme", [team("default")])];
    const picker = buildTargetPicker({ source: personalSource, activeOrgSlug: "acme", orgs });
    expect(picker.personal).toBeNull();
    // A move OUT of Personal still lists the team target.
    expect(picker.sameOrg.map((r) => r.context.id)).toContain(teamAllContextId("acme", "default"));
  });

  it("shows cross-org targets DISABLED with the switch-orgs reason, never omitted", () => {
    const orgs = [
      org("acme", "Acme", [team("default")]),
      org("globex", "Globex", [team("core", [{ owner: "globex", repo: "svc" }])]),
    ];
    const picker = buildTargetPicker({
      source: teamAllSource("acme", "default"),
      activeOrgSlug: "acme",
      orgs,
    });
    expect(picker.crossOrg.length).toBeGreaterThan(0);
    for (const row of picker.crossOrg) {
      expect(row.disabled).toBe(true);
      expect(row.reason).toBe(crossOrgReason("Globex"));
      expect(row.orgSlug).toBe("globex");
    }
    // Cross-org rows are never selectable.
    expect(selectableTargetIds(picker)).not.toContain(teamAllContextId("globex", "core"));
  });

  it("zero-teams active org offers Create-a-team instead of an empty picker", () => {
    const orgs = [org("acme", "Acme", [])];
    const picker = buildTargetPicker({ source: personalSource, activeOrgSlug: "acme", orgs });
    expect(picker.sameOrg).toHaveLength(0);
    expect(picker.nextStep).toEqual(CREATE_TEAM_NEXT_STEP);
  });

  it("an all-archived active org counts as zero teams (create-team)", () => {
    const orgs = [org("acme", "Acme", [team("old", [], true)])];
    const picker = buildTargetPicker({ source: personalSource, activeOrgSlug: "acme", orgs });
    expect(picker.sameOrg).toHaveLength(0);
    expect(picker.nextStep).toEqual(CREATE_TEAM_NEXT_STEP);
    expect(picker.crossOrg).toHaveLength(0);
  });

  it("no banned internal word appears on any row label or reason", () => {
    const orgs = [
      org("acme", "Acme", [team("default", [{ owner: "acme", repo: "api" }])]),
      org("globex", "Globex", [team("core")]),
    ];
    const picker = buildTargetPicker({
      source: teamAllSource("acme", "default"),
      activeOrgSlug: "acme",
      orgs,
    });
    const strings = [
      picker.personal?.context.label,
      ...picker.sameOrg.map((r) => r.context.label),
      ...picker.crossOrg.map((r) => r.context.label),
      ...picker.crossOrg.map((r) => r.reason),
      picker.nextStep?.label,
      noOtherReposLabel("Acme").label,
    ].filter(Boolean) as string[];
    for (const s of strings) expect(s).not.toMatch(BANNED);
  });
});

// SPDX-License-Identifier: MIT
import { resetContentFileIndexForTests } from "@/shared/lib/pipeline-graph/content-scan-queue";
import { useEffect, useState } from "react";
import { listScratchSpace } from "@/tauri/commands";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";
import type { ScratchSpaceListing } from "@/tauri/commands";

/**
 * Private item action seam: the verbs shared by the nav and All orgs, through the REAL command
 * wrappers — only Tauri's `invoke` is stubbed, so each test pins the exact payload
 * the backend move/copy/remove spine receives (the same spine the MCP tools drive).
 */

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    success: vi.fn(),
    warning: vi.fn(),
    error: vi.fn(),
    info: vi.fn(),
  }),
}));

import { toast } from "sonner";
import { ScratchItemMenu } from "./scratch-item-actions";

const ORG = "acme/ctx";
const OTHER = "acme/other";

function org(slug: string, name: string): Org {
  return {
    slug,
    display_name: name,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [
      { name: "default", source_repos: [] },
      { name: "platform", source_repos: [] },
    ],
    org_root: `/ctx/${slug}`,
  } as unknown as Org;
}

const LISTING: ScratchSpaceListing = {
  orgSlug: ORG,
  exists: true,
  files: [
    { path: "/s/orgs/acme/ctx/content/docs/ideas.md", logical: "docs/ideas.md", inertKind: null },
    {
      path: "/s/orgs/acme/ctx/content/prompts/standup.md",
      logical: "prompts/standup.md",
      inertKind: "prompt",
    },
  ],
  allOrgs: [
    {
      path: "/s/all-orgs/skills/review/SKILL.md",
      logical: "skills/review/SKILL.md",
      kind: "skill",
    },
  ],
  syncedOrgs: [ORG, OTHER],
};

let teamFiles: string[] = [];
let otherOrgFiles: string[] = [];

beforeEach(() => {
  resetContentFileIndexForTests();
  vi.clearAllMocks();
  teamFiles = [];
  otherOrgFiles = [];
  invoke.mockImplementation(async (cmd, args) => {
    switch (cmd) {
      case "list_scratch_space":
        return args?.orgSlug === OTHER
          ? {
              ...LISTING,
              orgSlug: OTHER,
              files: otherOrgFiles.map((l) => ({ path: l, logical: l, inertKind: null })),
            }
          : LISTING;
      case "list_context_files":
        return teamFiles.map((relativePath) => ({ relativePath }));
      case "move_content_between_contexts": {
        const p = args?.params as {
          retainSource?: boolean;
          force?: boolean;
          logicalName: string;
          toLogicalName?: string;
          to: { kind: string };
        };
        const target = p.toLogicalName ?? p.logicalName;
        if (p.to.kind === "teamAll" && !p.force && teamFiles.includes(target)) {
          return { kind: "rejectedTargetExists" };
        }
        return { kind: p.retainSource ? "copied" : "moved" };
      }
      case "remove_content_from_context":
        return { kind: "removed" };
      case "get_scratch_team_use":
        return { useScratch: true, useTeamFiles: [] };
      default:
        return [];
    }
  });
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Ada", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  } as never);
  useOrgStore.setState({
    orgs: [org(ORG, "Acme"), org(OTHER, "Other Co")],
    activeOrgSlug: ORG,
    activeTeamName: "platform",
  } as never);
});

function calls(cmd: string) {
  return invoke.mock.calls.filter(([c]) => c === cmd).map(([, a]) => a);
}

function ScratchItemsFixture() {
  const [listing, setListing] = useState<ScratchSpaceListing | null>(null);
  const reload = () => {
    void listScratchSpace(ORG).then(setListing);
  };
  useEffect(reload, []);
  return (
    <>
      {listing?.files.map((file) => (
        <ScratchItemMenu
          key={file.path}
          item={{
            orgSlug: ORG,
            logical: file.logical,
            where: "org",
            inertKind: file.inertKind,
            defaultTeam: "platform",
          }}
          onDone={reload}
        />
      ))}
      {listing?.allOrgs.map((file) => (
        <ScratchItemMenu
          key={file.path}
          item={{
            orgSlug: ORG,
            logical: file.logical,
            where: "allOrgs",
            inertKind: file.kind,
            defaultTeam: "platform",
          }}
          onDone={reload}
        />
      ))}
    </>
  );
}

async function openMenu(fileName: string) {
  const user = userEvent.setup();
  render(<ScratchItemsFixture />);
  await user.click(await screen.findByRole("button", { name: `Actions for ${fileName}` }));
  return user;
}

describe("Scratch space row verbs (SLW-PT-22)", () => {
  it("Share with ‹Team› moves the item into the open team — no sole-copy step", async () => {
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Share with platform/ }));
    const sheet = await screen.findByTestId("scratch-share-sheet");
    expect(sheet).toHaveTextContent("Everyone on platform will see it");
    await user.click(within(sheet).getByRole("button", { name: "Share" }));
    // Privacy was the share sheet itself, and nothing is lost: no further step.
    await waitFor(() => expect(calls("move_content_between_contexts")).toHaveLength(1));
    expect(calls("move_content_between_contexts")[0]).toEqual({
      params: {
        from: { kind: "personal", orgSlug: ORG },
        to: { kind: "teamAll", orgSlug: ORG, teamName: "platform" },
        logicalName: "docs/ideas.md",
        userId: 42,
        retainSource: false,
      },
    });
    expect(screen.queryByTestId("move-sheet")).toBeNull();
    expect(toast.success).toHaveBeenCalledWith("Moved 'ideas.md' to platform");
    // The listing re-reads so the item leaves the list.
    await waitFor(() => expect(calls("list_scratch_space").length).toBeGreaterThan(1));
  });

  it("Share offers the collision chooser and Keep both lands under a fresh name", async () => {
    teamFiles = ["docs/ideas.md"];
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Share with platform/ }));
    await user.click(await screen.findByTestId("scratch-share-confirm"));
    await user.click(await screen.findByTestId("move-gate-collision-keep-both"));
    // The first ask reported the collision; the second lands under a fresh name.
    await waitFor(() => expect(calls("move_content_between_contexts")).toHaveLength(2));
    expect(calls("move_content_between_contexts")[1]).toMatchObject({
      params: { toLogicalName: "docs/ideas-2.md", force: false },
    });
  });

  it("Copy to another org copies into that org's scratch space with no org switch", async () => {
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Copy to another org/ }));
    const sheet = await screen.findByTestId("scratch-copy-sheet");
    expect(sheet).toHaveTextContent("Private for Other Co");
    await user.click(within(sheet).getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(calls("move_content_between_contexts")).toHaveLength(1));
    expect(calls("move_content_between_contexts")[0]).toMatchObject({
      params: {
        from: { kind: "personal", orgSlug: ORG },
        to: { kind: "personal", orgSlug: OTHER },
        retainSource: true,
      },
    });
    expect(useOrgStore.getState().activeOrgSlug).toBe(ORG);
  });

  it("Copy to another org offers only orgs synced on this Mac", async () => {
    useOrgStore.setState({
      orgs: [org(ORG, "Acme"), org(OTHER, "Other Co"), org("acme/ghost", "Not here")],
    } as never);
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Copy to another org/ }));
    const sheet = await screen.findByTestId("scratch-copy-sheet");
    await user.click(within(sheet).getByRole("combobox", { name: "Org" }));
    expect(screen.getByRole("option", { name: "Other Co" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "Not here" })).toBeNull();
  });

  it("with no other org synced, Copy says so instead of offering one", async () => {
    invoke.mockImplementation(async (cmd) =>
      cmd === "list_scratch_space" ? { ...LISTING, syncedOrgs: [ORG] } : [],
    );
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Copy to another org/ }));
    expect(await screen.findByTestId("scratch-copy-none")).toHaveTextContent(
      "No other org is synced on this Mac",
    );
    expect(screen.getByTestId("scratch-copy-confirm")).toBeDisabled();
  });

  it("a Share that landed but kept the original says so, and the listing re-reads", async () => {
    invoke.mockImplementation(async (cmd) => {
      if (cmd === "move_content_between_contexts")
        return { kind: "movedSourceKept", reason: "permission denied" };
      return cmd === "list_scratch_space" ? LISTING : [];
    });
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Share with platform/ }));
    await user.click(await screen.findByTestId("scratch-share-confirm"));
    await waitFor(() =>
      expect(toast.warning).toHaveBeenCalledWith(
        "Moved 'ideas.md' to platform, but the original couldn't be removed (permission denied). Try again, or remove it yourself.",
      ),
    );
    await waitFor(() => expect(calls("list_scratch_space").length).toBeGreaterThan(1));
  });

  it("Use in all orgs is refused up front for a non-inert file, and moves an inert one", async () => {
    const user = await openMenu("ideas.md");
    const refused = screen.getByRole("menuitem", { name: /Use in all orgs/ });
    expect(refused).toHaveAttribute("data-disabled");
    expect(refused).toHaveTextContent(
      "Only skills, prompts, snippets and templates can apply in every org",
    );
    await user.keyboard("{Escape}");

    await user.click(screen.getByRole("button", { name: "Actions for standup.md" }));
    await user.click(screen.getByRole("menuitem", { name: "Use in all orgs" }));
    // It stays in the scratch space: no privacy or sole-copy step.
    await waitFor(() => expect(calls("move_content_between_contexts")).toHaveLength(1));
    expect(calls("move_content_between_contexts")[0]).toMatchObject({
      params: {
        from: { kind: "personal", orgSlug: ORG },
        to: { kind: "scratchAllOrgs" },
        logicalName: "prompts/standup.md",
        retainSource: false,
      },
    });
  });

  it("Only this org moves an all-orgs item back into the open org's scratch space", async () => {
    const user = await openMenu("SKILL.md");
    await user.click(screen.getByRole("menuitem", { name: "Only this org" }));
    await waitFor(() => expect(calls("move_content_between_contexts")).toHaveLength(1));
    expect(calls("move_content_between_contexts")[0]).toMatchObject({
      params: {
        from: { kind: "scratchAllOrgs" },
        to: { kind: "personal", orgSlug: ORG },
        logicalName: "skills/review/SKILL.md",
      },
    });
  });

  it("Remove warns it may be the only copy before deleting permanently", async () => {
    const user = await openMenu("ideas.md");
    await user.click(screen.getByRole("menuitem", { name: /Remove/ }));
    const header = await screen.findByTestId("remove-sheet-header");
    expect(header).toHaveTextContent("'ideas.md' may exist only on this machine");
    expect(screen.getByTestId("remove-sheet")).toHaveTextContent("can't be undone");
    await user.click(screen.getByRole("button", { name: "Delete permanently" }));
    await waitFor(() => expect(calls("remove_content_from_context")).toHaveLength(1));
    expect(calls("remove_content_from_context")[0]).toEqual({
      params: {
        context: { kind: "personal", orgSlug: ORG },
        logicalName: "docs/ideas.md",
        userId: 42,
      },
    });
  });
});

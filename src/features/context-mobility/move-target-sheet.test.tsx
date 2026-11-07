// SPDX-License-Identifier: MIT
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MoveTargetSheet } from "@/features/context-mobility/move-target-sheet";
import { captureCrossOrgOp } from "@/features/context-mobility/cross-org-resume";
import {
  personalContextId,
  teamAllContextId,
  teamRepoContextId,
  type Context,
} from "@/shared/lib/context-model";
import type { Org, Team } from "@/shared/types/domain";

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

const acme = org("acme", "Acme", [
  { name: "default", source_repos: [{ owner: "acme", repo: "api" }] },
]);
const globex = org("globex", "Globex", [{ name: "core", source_repos: [] }]);

const personalSource: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Personal",
};

function baseProps() {
  return {
    open: true,
    logicalName: "notes/plan.md",
    fileName: "plan.md",
    source: personalSource,
    activeOrgSlug: "acme",
    orgs: [acme, globex],
    onPickTarget: vi.fn(),
    onSwitchOrg: vi.fn(),
    onCreateTeam: vi.fn(),
    onClose: vi.fn(),
  };
}

describe("MoveTargetSheet", () => {
  it("picking a same-org target hands (source, target) up to open the MoveSheet", () => {
    const props = baseProps();
    render(<MoveTargetSheet {...props} />);
    // Move OUT of Personal → "All of default" is a same-org target.
    fireEvent.click(
      screen.getByTestId(`move-target-option-${teamAllContextId("acme", "default")}`),
    );
    expect(props.onPickTarget).toHaveBeenCalledWith(
      expect.objectContaining({ id: teamAllContextId("acme", "default") }),
    );
  });

  it("cross-org targets render disabled-with-reason and drive the switch on click", () => {
    const props = baseProps();
    render(<MoveTargetSheet {...props} />);
    const crossRow = screen.getByTestId(
      `move-target-cross-org-${teamAllContextId("globex", "core")}`,
    );
    expect(crossRow).toHaveAttribute("aria-disabled", "true");
    expect(crossRow).toHaveTextContent("Globex — switch orgs to place content here");
    fireEvent.click(crossRow);
    expect(props.onSwitchOrg).toHaveBeenCalledWith(
      expect.objectContaining({ targetOrgSlug: "globex", targetOrgName: "Globex" }),
    );
    // No target placed yet — the switch precedes the resume.
    expect(props.onPickTarget).not.toHaveBeenCalled();
  });

  it("resume restates the op and Continue places into the pre-selected target", () => {
    const props = baseProps();
    const target: Context = {
      id: teamAllContextId("globex", "core"),
      kind: "team-all",
      sharing: "shared",
      label: "All of core",
      teamName: "core",
    };
    const resuming = captureCrossOrgOp({
      op: "move",
      source: personalSource,
      logicalName: "notes/plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
    render(<MoveTargetSheet {...props} activeOrgSlug="globex" resuming={resuming} />);
    expect(screen.getByTestId("move-target-header")).toHaveTextContent(
      "Continue moving 'plan.md' into Globex › All of core?",
    );
    fireEvent.click(screen.getByTestId("move-target-resume-continue"));
    expect(props.onPickTarget).toHaveBeenCalledWith(target);
  });

  it("Cancel during resume leaves both orgs untouched (no placement)", () => {
    const props = baseProps();
    const target: Context = {
      id: teamAllContextId("globex", "core"),
      kind: "team-all",
      sharing: "shared",
      label: "All of core",
      teamName: "core",
    };
    const resuming = captureCrossOrgOp({
      op: "move",
      source: personalSource,
      logicalName: "notes/plan.md",
      fileName: "plan.md",
      targetOrgSlug: "globex",
      targetOrgName: "Globex",
      target,
    });
    render(<MoveTargetSheet {...props} activeOrgSlug="globex" resuming={resuming} />);
    fireEvent.click(screen.getByTestId("move-target-resume-cancel"));
    expect(props.onPickTarget).not.toHaveBeenCalled();
    expect(props.onClose).toHaveBeenCalled();
  });

  it("zero-teams active org offers Create-a-team, never an empty list", () => {
    const props = baseProps();
    render(<MoveTargetSheet {...props} orgs={[org("acme", "Acme", []), globex]} />);
    const createBtn = screen.getByTestId("move-target-create-team");
    expect(createBtn).toHaveTextContent("Create a team to share into");
    fireEvent.click(createBtn);
    expect(props.onCreateTeam).toHaveBeenCalled();
  });

  it("moving a team item still lists Personal as a target", () => {
    const props = baseProps();
    const teamSource: Context = {
      id: teamRepoContextId("acme", "default", { owner: "acme", repo: "api" }),
      kind: "team-repo",
      sharing: "shared",
      label: "default · api",
      teamName: "default",
      repoSlug: { owner: "acme", repo: "api" },
    };
    render(<MoveTargetSheet {...props} source={teamSource} />);
    expect(screen.getByTestId(`move-target-option-${personalContextId()}`)).toBeInTheDocument();
  });

  it("no banned internal word appears anywhere on the rendered picker", () => {
    const props = baseProps();
    const { container } = render(<MoveTargetSheet {...props} />);
    expect(container.textContent ?? "").not.toMatch(
      /\b(provider|overlay|layer|scratch|TeamWorkspace|TeamRepo|Workspace)\b/,
    );
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";

let orgState = { activeOrgSlug: "acme" as string | null, stateLoaded: true };
let target = { orgRoot: "/root", team: "backend" as string | null, teamKey: "acme:backend" };

vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: (selector: (s: typeof orgState) => unknown) => selector(orgState),
}));
vi.mock("@/features/settings/hooks/use-active-team-config-target", () => ({
  useActiveTeamConfigTarget: () => target,
}));
vi.mock("@/features/viewer/hooks/use-status-taxonomy", () => ({
  useStatusTaxonomy: vi.fn(),
}));
vi.mock("./phases-panel", () => ({
  PhasesPanel: (props: { team: string }) => (
    <div data-testid="phases-panel-stub" data-team={props.team} />
  ),
}));

const { PhasesSection } = await import("./phases-section");

beforeEach(() => {
  orgState = { activeOrgSlug: "acme", stateLoaded: true };
  target = { orgRoot: "/root", team: "backend", teamKey: "acme:backend" };
});

describe("PhasesSection", () => {
  it("renders the phases editor for the active team", () => {
    render(<PhasesSection query="" />);
    expect(screen.getByTestId("phases-panel-stub")).toHaveAttribute("data-team", "backend");
  });

  it("shows the no-active-workspace notice when there is no active org", () => {
    orgState = { activeOrgSlug: null, stateLoaded: true };
    render(<PhasesSection query="" />);
    expect(screen.getByTestId("settings-phases-no-active")).toBeInTheDocument();
    expect(screen.queryByTestId("phases-panel-stub")).not.toBeInTheDocument();
  });

  it("shows the no-team state instead of the editor for an unresolved team", () => {
    target = { orgRoot: "", team: null, teamKey: "acme:backend" };
    render(<PhasesSection query="" />);
    expect(screen.getByTestId("settings-phases-no-team")).toBeInTheDocument();
    expect(screen.queryByTestId("phases-panel-stub")).not.toBeInTheDocument();
  });

  it("renders nothing when the search query excludes the section", () => {
    const { container } = render(<PhasesSection query="zzz-no-match" />);
    expect(container).toBeEmptyDOMElement();
  });
});

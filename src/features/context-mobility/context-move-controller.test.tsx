// SPDX-License-Identifier: MIT
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ContextMoveController } from "@/features/context-mobility/context-move-controller";
import { personalContextId, teamAllContextId, type Context } from "@/shared/lib/context-model";
import type { Org } from "@/shared/types/domain";

// The gate sheet calls the backend on a clean no-gate move; stub it out so the
// controller wiring test stays about the flow, not the backend. RemoveSheet
// resolves its own backend too — stub it likewise.
vi.mock("@/tauri/commands", () => ({
  moveContentBetweenContexts: vi.fn().mockResolvedValue({ kind: "moved" }),
  removeContentFromContext: vi.fn().mockResolvedValue({ kind: "removed" }),
}));

function org(slug: string, display: string, teams: Org["teams"]): Org {
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

const acme = org("acme", "Acme", [{ name: "default", source_repos: [] }]);
const globex = org("globex", "Globex", [{ name: "core", source_repos: [] }]);

const personalSource: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Private",
};

function renderController(over: Partial<React.ComponentProps<typeof ContextMoveController>> = {}) {
  const onSwitchOrg = vi.fn();
  render(
    <ContextMoveController
      source={personalSource}
      logicalName="notes/plan.md"
      fileName="plan.md"
      activeOrgSlug="acme"
      orgs={[acme, globex]}
      userId={7}
      onSwitchOrg={onSwitchOrg}
      {...over}
    />,
  );
  return { onSwitchOrg };
}

describe("ContextMoveController — the per-file entry point wired end to end", () => {
  it("⋯ Move to… opens the org-scoped target picker with the known source", () => {
    renderController();
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    expect(screen.getByTestId("move-target-sheet")).toBeInTheDocument();
    expect(screen.getByTestId("move-target-header")).toHaveTextContent("Move 'plan.md' to…");
    // Same-org target present; the source (Personal) is not offered onto itself.
    expect(
      screen.getByTestId(`move-target-option-${teamAllContextId("acme", "default")}`),
    ).toBeInTheDocument();
    expect(screen.queryByTestId(`move-target-option-${personalContextId()}`)).toBeNull();
  });

  it("choosing a same-org target advances to the SLW-PT-12 gate sheet", () => {
    renderController();
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    fireEvent.click(
      screen.getByTestId(`move-target-option-${teamAllContextId("acme", "default")}`),
    );
    expect(screen.getByTestId("move-sheet")).toBeInTheDocument();
    // The gate sheet restates the op with the chosen target label.
    expect(screen.getByTestId("move-sheet-header")).toHaveTextContent("plan.md");
  });

  it("choosing a cross-org target captures the op, switches org, and resumes", () => {
    const { onSwitchOrg } = renderController();
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-move"));
    fireEvent.click(
      screen.getByTestId(`move-target-cross-org-${teamAllContextId("globex", "core")}`),
    );
    // The host is asked to switch org with the full captured op (§K).
    expect(onSwitchOrg).toHaveBeenCalledWith(
      expect.objectContaining({ targetOrgSlug: "globex", targetOrgName: "Globex" }),
    );
    // The controller re-presents the picker in resume mode with the op restated.
    expect(screen.getByTestId("move-target-header")).toHaveTextContent(
      "Continue moving 'plan.md' into Globex › All of core?",
    );
  });

  it("Copy to… threads the copy verb through the picker header", () => {
    renderController();
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-copy"));
    expect(screen.getByTestId("move-target-header")).toHaveTextContent("Copy 'plan.md' to…");
  });

  it("⋯ Remove opens the RemoveSheet in the scratch sole-copy §J warning", () => {
    renderController();
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-remove"));
    expect(screen.getByTestId("remove-sheet")).toBeInTheDocument();
    // The §J sole-copy warning fires with its "only on this machine" copy.
    expect(screen.getByTestId("remove-gate-soleCopyBlock")).toBeInTheDocument();
    expect(screen.getByTestId("remove-sheet-header")).toHaveTextContent(
      "'plan.md' may exist only on this machine",
    );
  });

  it("⋯ Remove on a SHARED item is a plain confirm (no sole-copy gate)", () => {
    const shared: Context = {
      id: teamAllContextId("acme", "default"),
      kind: "team-all",
      sharing: "shared",
      label: "All of default",
      teamName: "default",
    };
    renderController({ source: shared });
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-remove"));
    expect(screen.getByTestId("remove-gate-normal")).toBeInTheDocument();
  });
});

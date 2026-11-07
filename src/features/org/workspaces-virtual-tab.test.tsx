// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./components/context-attachments", () => ({
  ContextAttachments: ({ initialCreate }: { initialCreate: boolean }) => (
    <div data-testid="context-attachments-stub">{initialCreate ? "create" : "actions"}</div>
  ),
}));

import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { CODESENSE_DOCS_SLUG } from "@/shared/lib/codesense-docs";
import type { Org } from "@/shared/types/domain";
import { ManageWorkspacesTab } from "./workspaces-virtual-tab";

const org = (slug: string, display_name: string) =>
  ({ slug, display_name, teams: [], people: [] }) as unknown as Org;

describe("Context repos tab (SW-OC-16)", () => {
  beforeEach(() => {
    useUiStore.setState({ workspacesScreen: "home" });
    useOrgStore.setState({
      orgs: [
        org("local/zeta", "Zeta"),
        org(CODESENSE_DOCS_SLUG, "CodeSense Docs"),
        org("local/acme", "Acme"),
      ],
      activeOrgSlug: "local/acme",
      disabledOrgSlugs: [],
    });
  });

  it("is titled Context repos, in one column that never scrolls sideways", () => {
    render(<ManageWorkspacesTab />);
    const column = screen
      .getByRole("heading", { name: "Context repos" })
      .closest("header")!.parentElement!;
    expect(column.contains(screen.getByTestId("context-attachments-stub"))).toBe(true);
    expect(screen.getByTestId("workspaces-tab").className).toContain("overflow-x-hidden");
  });

  it("pins the CodeSense docs first and never offers to disable them", () => {
    render(<ManageWorkspacesTab />);
    const rows = screen.getAllByTestId(/^workspaces-row-/).map((row) => row.dataset.testid);
    expect(rows).toEqual([
      `workspaces-row-${CODESENSE_DOCS_SLUG}`,
      "workspaces-row-local/acme",
      "workspaces-row-local/zeta",
    ]);
    expect(screen.queryByTestId(`workspaces-disable-${CODESENSE_DOCS_SLUG}`)).toBeNull();
    expect(screen.getByTestId("workspaces-disable-local/zeta")).toBeInTheDocument();
  });

  it("opens straight on the create form when asked to create", () => {
    useUiStore.setState({ workspacesScreen: "create" });
    render(<ManageWorkspacesTab />);
    expect(screen.getByTestId("context-attachments-stub")).toHaveTextContent("create");
  });
});

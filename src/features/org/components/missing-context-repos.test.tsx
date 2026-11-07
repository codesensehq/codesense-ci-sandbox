// SPDX-License-Identifier: MIT
import { act, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useOrgStore } from "@/shared/stores/org-store";
import { CODESENSE_DOCS_SLUG } from "@/shared/lib/codesense-docs";
import type { Org, OrgSummary } from "@/shared/types/domain";

import { endLocalSetup } from "../lib/local-setup-completion";
import { MissingContextRepos, useMissingContextRepos } from "./missing-context-repos";

const api = vi.hoisted(() => ({
  list: vi.fn(),
  attach: vi.fn(),
  detach: vi.fn(),
  open: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: api.open }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => ({ principal: "1", github_user_id: 1 }),
  useCurrentUser: () => ({ principal: "1", github_user_id: 1 }),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  listContextAttachments: api.list,
  attachContextRepository: api.attach,
  detachContextRepository: api.detach,
}));

const acme: OrgSummary = {
  slug: "acme/context",
  display_name: "Acme",
  org_root: "/Users/me/old/acme",
  has_remote: false,
  remote_url: null,
  availability_error: "Attached context acme/context is unavailable",
} as OrgSummary;

describe("missing context repos (SW-OC-16)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    endLocalSetup();
    useOrgStore.setState({
      orgs: [{ slug: "acme/context", display_name: "Acme" } as Org],
      activeOrgSlug: null,
    });
  });

  it("lists only unavailable connections, never the pinned docs, and only while active", async () => {
    api.list.mockResolvedValue([
      acme,
      { ...acme, slug: CODESENSE_DOCS_SLUG, display_name: "CodeSense Docs" },
      { ...acme, slug: "fine/repo", availability_error: null },
    ]);
    const closed = renderHook(() => useMissingContextRepos(false));
    expect(api.list).not.toHaveBeenCalled();
    closed.unmount();
    const { result } = renderHook(() => useMissingContextRepos(true));
    await waitFor(() => expect(result.current.map((row) => row.slug)).toEqual(["acme/context"]));
  });

  it("says where it can't be found and offers Locate folder… and Disconnect", () => {
    render(<MissingContextRepos rows={[acme]} />);
    expect(screen.getByTestId("missing-context-repo-acme/context")).toHaveTextContent(
      "Acme can't be found at /Users/me/old/acme.",
    );
    expect(screen.getByRole("button", { name: "Locate folder…" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Disconnect" })).toBeInTheDocument();
  });

  it("Locate folder… reconnects the moved folder", async () => {
    api.open.mockResolvedValue("/Users/me/new/acme");
    api.detach.mockResolvedValue(undefined);
    api.attach.mockResolvedValue({
      loaded_org: { org: { slug: "acme/context", display_name: "Acme", teams: [] } },
    });
    render(<MissingContextRepos rows={[acme]} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Locate folder…" }));
    });
    await waitFor(() => expect(api.attach).toHaveBeenCalledWith("/Users/me/new/acme"));
    expect(api.detach).toHaveBeenCalledWith("acme/context");
    expect(useOrgStore.getState().activeOrgSlug).toBe("acme/context");
  });

  it("a failed Locate says the stale connection is gone and how to recover", async () => {
    api.open.mockResolvedValue("/Users/me/wrong");
    api.detach.mockResolvedValue(undefined);
    api.attach.mockRejectedValue(new Error("Selected folder is not a Git checkout"));
    render(<MissingContextRepos rows={[acme]} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Locate folder…" }));
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Acme was disconnected, but that folder couldn't be connected: Selected folder is not a Git checkout",
    );
  });

  it("Disconnect drops the registration and the context repo from the list", async () => {
    api.detach.mockResolvedValue(undefined);
    render(<MissingContextRepos rows={[acme]} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Disconnect" }));
    });
    expect(api.detach).toHaveBeenCalledWith("acme/context");
    expect(useOrgStore.getState().orgs).toEqual([]);
  });
});

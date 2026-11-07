// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RepoSection } from "./repo-section";
import { useOrgStore } from "@/shared/stores/org-store";
import { useConfigStore } from "@/shared/stores/config-store";
import type { Org } from "@/shared/types/domain";
const org: Org = {
  slug: "acme",
  display_name: "Acme",
  org_root: "/plans",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [
    { name: "planning", source_repos: [] },
    { name: "other", source_repos: [] },
  ],
};
afterEach(cleanup);
it("offers the convention without a source and retains captured team ownership during save", async () => {
  useOrgStore.setState({ orgs: [org], discoveryComplete: true, workspaceRepos: {} });
  const commit = vi.fn().mockResolvedValue(undefined);
  useConfigStore.setState({
    teamMainDocPreferences: {
      "acme:planning": { kind: "ready", mainDoc: "Epic.md" },
      "acme:other": { kind: "ready", mainDoc: "" },
    },
    setTeamMainDoc: commit,
  });
  const { rerender } = render(<RepoSection repo={null} query="" teamKey="acme:planning" />);
  fireEvent.change(screen.getByLabelText("Team main doc"), { target: { value: "Plan.md" } });
  fireEvent.blur(screen.getByLabelText("Team main doc"));
  await waitFor(() => expect(commit).toHaveBeenCalledWith("acme:planning", "Plan.md"));
  rerender(<RepoSection repo={null} query="" teamKey="acme:other" />);
  expect(screen.getByLabelText("Team main doc")).toHaveValue("");
  expect(useOrgStore.getState().workspaceRepos).toEqual({});
});
it("does not overwrite a field before asynchronous preferences have loaded", async () => {
  useOrgStore.setState({ orgs: [org], discoveryComplete: true });
  const load = vi.fn().mockResolvedValue(undefined);
  useConfigStore.setState({ teamMainDocPreferences: {}, ensureTeamMainDocPreference: load });
  render(<RepoSection repo={null} query="" teamKey="acme:planning" />);
  expect(screen.queryByLabelText("Team main doc")).toBeNull();
  act(() =>
    useConfigStore.setState({
      teamMainDocPreferences: { "acme:planning": { kind: "ready", mainDoc: "Epic.md" } },
    }),
  );
  expect(screen.getByLabelText("Team main doc")).toHaveValue("Epic.md");
});

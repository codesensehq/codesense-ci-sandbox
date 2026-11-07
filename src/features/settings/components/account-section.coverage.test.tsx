// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { AccountSection } from "./account-section";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
const select = vi.fn();
beforeEach(() => {
  select.mockReset();
  useLocalProfileStore.setState({
    busy: false,
    error: null,
    select,
    status: {
      active: null,
      privateAuthority: null,
      profiles: [
        { principal: "42", displayName: "Work", machineOwner: null },
        { principal: "999999999999", displayName: "Local", machineOwner: null },
      ],
    },
  });
});
it("offers explicit local profile selection without assuming an account", () => {
  render(<AccountSection />);
  expect(select).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: /Work/ }));
  expect(select).toHaveBeenCalledWith("42");
  expect(screen.queryByRole("button", { name: /sign in|connect|sign out/i })).toBeNull();
});
it("keeps native persistence failures visible", () => {
  useLocalProfileStore.setState({ error: "Cannot read local profile metadata" });
  render(<AccountSection />);
  expect(screen.getByRole("alert")).toHaveTextContent("Cannot read local profile metadata");
});

it("offers explicit same-profile machine setup only when the selected profile has no owner", () => {
  const state = useLocalProfileStore.getState().status!;
  useLocalProfileStore.setState({ status: { ...state, active: state.profiles[0] } });
  const view = render(<AccountSection />);
  expect(select).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Set up machine access" }));
  expect(select).toHaveBeenCalledWith("42");
  useLocalProfileStore.setState({
    status: {
      ...state,
      active: { ...state.profiles[0], machineOwner: { kind: "local", id: "owner" } },
    },
  });
  view.rerender(<AccountSection />);
  expect(screen.queryByRole("button", { name: "Set up machine access" })).toBeNull();
});
it("shows metadata recovery failure without disabling local profile choices", () => {
  const state = useLocalProfileStore.getState().status!;
  useLocalProfileStore.setState({
    status: { ...state, metadataRecovery: { pending: true, error: "Metadata unavailable" } },
  });
  render(<AccountSection />);
  expect(screen.getByRole("status")).toHaveTextContent("Metadata unavailable");
  expect(screen.queryByRole("alert")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /Work/ }));
  expect(select).toHaveBeenCalledWith("42");
});

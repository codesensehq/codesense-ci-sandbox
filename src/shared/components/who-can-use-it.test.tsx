// SPDX-License-Identifier: MIT
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ONLY_ME, WhoCanUseItField, defaultAudience, resolveAudience } from "./who-can-use-it";

const TEAMS = [{ value: "acme/ctx:default", label: "default" }];

describe("WhoCanUseItField — one field for scratch vs team (SLW-PT-21)", () => {
  it("defaults to the team; Only me is the alternative", () => {
    expect(defaultAudience(TEAMS)).toBe("acme/ctx:default");
    expect(defaultAudience([])).toBe(ONLY_ME);
    const onChange = vi.fn();
    render(<WhoCanUseItField value={defaultAudience(TEAMS)} onChange={onChange} teams={TEAMS} />);
    const select = screen.getByRole("combobox", { name: "Who can use it" });
    expect(select).toHaveValue("acme/ctx:default");
    expect(screen.getByRole("option", { name: "Only me" })).toBeInTheDocument();
    expect(screen.getByTestId("who-can-use-it-hint")).toHaveTextContent(
      "Everyone on default can use it",
    );
    fireEvent.change(select, { target: { value: ONLY_ME } });
    expect(onChange).toHaveBeenCalledWith(ONLY_ME);
  });

  it("Only me says where it goes", () => {
    render(<WhoCanUseItField value={ONLY_ME} onChange={() => {}} teams={TEAMS} />);
    expect(screen.getByTestId("who-can-use-it-hint")).toHaveTextContent(
      "Kept in Private — only you see it",
    );
  });

  it("the hint is the select's description (aria-describedby)", () => {
    render(<WhoCanUseItField value={ONLY_ME} onChange={() => {}} teams={TEAMS} />);
    const select = screen.getByRole("combobox", { name: "Who can use it" });
    expect(select).toHaveAccessibleDescription(/only you see it/);
  });

  it("says when the team doesn't use scratch items, so an Only me item won't apply there", () => {
    render(
      <WhoCanUseItField
        value={ONLY_ME}
        onChange={() => {}}
        teams={TEAMS}
        scratchOffFor="default"
      />,
    );
    expect(screen.getByTestId("who-can-use-it-hint")).toHaveTextContent(
      "private items are off for default, so it won't apply there",
    );
  });

  it("where Only me can't go, the option is disabled with a one-line reason", () => {
    render(
      <WhoCanUseItField
        value="acme/ctx:default"
        onChange={() => {}}
        teams={TEAMS}
        onlyMeUnavailable="This folder belongs to one repo's files."
      />,
    );
    expect(screen.getByRole("option", { name: "Only me" })).toBeDisabled();
    expect(screen.getByTestId("who-can-use-it-only-me-reason")).toHaveTextContent(
      "This folder belongs to one repo's files.",
    );
  });

  it("with no default it asks for a pick instead of guessing", () => {
    expect(resolveAudience(null, TEAMS, null)).toBe("");
    expect(resolveAudience(null, TEAMS, "acme/ctx:default")).toBe("acme/ctx:default");
    expect(resolveAudience("missing", TEAMS)).toBe("acme/ctx:default");
    render(<WhoCanUseItField value="" onChange={() => {}} teams={TEAMS} />);
    expect(screen.getByRole("combobox", { name: "Who can use it" })).toHaveValue("");
    expect(screen.getByTestId("who-can-use-it-hint")).toHaveTextContent("Pick who can use it");
  });
});

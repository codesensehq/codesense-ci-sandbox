// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { fireEvent, render, screen } from "@/test-utils";

import type { Person } from "@/shared/types/domain";
import { PersonAvatar, personInitials } from "./person-avatar";

function person(over?: Partial<Person>): Person {
  return {
    github_user_id: 1,
    github_username: "octocat",
    display_name: "Octo Cat",
    avatar_url: null,
    ...over,
  };
}

describe("personInitials", () => {
  it("takes the first letter of the first two words of a display name", () => {
    expect(personInitials(person({ display_name: "Octo Cat" }), "octocat")).toBe("OC");
  });

  it("takes the first two letters of a single-word display name", () => {
    expect(personInitials(person({ display_name: "Cher" }), "cher")).toBe("CH");
  });

  it("falls back to the first two letters of the login when there is no name", () => {
    expect(personInitials(null, "@Sam")).toBe("SA");
  });

  it("uses the person's github_username when display_name is blank", () => {
    expect(personInitials(person({ display_name: "  ", github_username: "zed" }), null)).toBe("ZE");
  });

  it("is empty for no person and no login", () => {
    expect(personInitials(null, null)).toBe("");
  });
});

describe("PersonAvatar", () => {
  it("renders the avatar image when the person has an avatar_url", () => {
    const { container } = render(
      <PersonAvatar
        person={person({ avatar_url: "https://avatars.example/u/1" })}
        login="octocat"
      />,
    );
    // alt="" (decorative — the tooltip names the person), so query by tag.
    const img = container.querySelector("img")!;
    expect(img).toHaveAttribute("src", "https://avatars.example/u/1");
    // End-user-safety hardening (SW-ID-6): async, no reflow, no-referrer, no drag.
    expect(img).toHaveAttribute("loading", "lazy");
    expect(img).toHaveAttribute("referrerpolicy", "no-referrer");
    expect(img).toHaveAttribute("width", "20");
  });

  it("falls back to initials when the resolved person has no avatar_url", () => {
    const { container } = render(
      <PersonAvatar person={person({ display_name: "Ada Lovelace" })} login="ada" />,
    );
    expect(screen.getByText("AL")).toBeInTheDocument();
    expect(container.querySelector("img")).toBeNull();
  });

  it("renders initials from the login when the login does not resolve to a person", () => {
    render(<PersonAvatar person={null} login="mystery" />);
    expect(screen.getByText("MY")).toBeInTheDocument();
  });

  it("degrades a failed avatar image to initials (onError)", () => {
    const { container } = render(
      <PersonAvatar
        person={person({ display_name: "Ada Lovelace", avatar_url: "https://bad/url" })}
        login="ada"
      />,
    );
    fireEvent.error(container.querySelector("img")!);
    expect(screen.getByText("AL")).toBeInTheDocument();
    expect(container.querySelector("img")).toBeNull();
  });

  it("re-attempts a new avatar_url after a prior one failed (failure is not sticky)", () => {
    const { container, rerender } = render(
      <PersonAvatar
        person={person({ display_name: "Ada Lovelace", avatar_url: "https://bad/url" })}
        login="ada"
      />,
    );
    fireEvent.error(container.querySelector("img")!);
    expect(screen.getByText("AL")).toBeInTheDocument(); // fell back to initials

    // A roster reload supplies a working url on the SAME instance — must re-attempt,
    // not stay stuck on initials.
    rerender(
      <PersonAvatar
        person={person({ display_name: "Ada Lovelace", avatar_url: "https://good/url" })}
        login="ada"
      />,
    );
    expect(container.querySelector("img")).toHaveAttribute("src", "https://good/url");
  });

  it("shows a neutral unassigned placeholder (not a blank) when there is no login", () => {
    const { container } = render(<PersonAvatar person={null} login={null} />);
    // Neutral state: the lucide User glyph, never an empty box or stray initials.
    expect(container.querySelector("svg")).toBeInTheDocument();
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toBe("");
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, within } from "@/test-utils";

import type { Person } from "@/shared/types/domain";
import { buildPeopleByLogin } from "@/shared/lib/people-by-login";

// The picker reads the roster itself via usePeopleByLogin — mock it with an
// in-memory index built from a fixed roster (the real builder, so the folding
// contract is exercised end-to-end).
// avatar_url set so PersonAvatar renders an <img alt=""> (no initials text) —
// keeps option textContent assertions clean.
const DEFAULT_ROSTER: Person[] = [
  {
    github_user_id: 1,
    github_username: "AdaLovelace",
    display_name: "Ada Lovelace",
    avatar_url: "https://avatars.example/1",
  },
  {
    github_user_id: 2,
    github_username: "octocat",
    display_name: "Octo Cat",
    avatar_url: "https://avatars.example/2",
  },
];

// Mutable so a test can simulate an empty roster (no active org / loading).
let roster: Person[] = DEFAULT_ROSTER;

vi.mock("@/shared/stores/use-people-by-login", () => ({
  usePeopleByLogin: () => buildPeopleByLogin(roster),
}));
vi.mock("@/shared/stores/use-roster-people", () => ({
  useRosterPeople: () => roster,
}));

import { AssigneePicker } from "./assignee-picker";

function open() {
  fireEvent.click(screen.getByRole("button", { name: "Assignee" }));
}

// Option rows are plain buttons (matching the sibling MetadataOptionSelect /
// RepoSelectorPopover pattern); everything except the aria-labelled trigger.
function optionButtons() {
  return screen.getAllByRole("button").filter((b) => b.getAttribute("aria-label") !== "Assignee");
}

describe("AssigneePicker (SW-ID-5)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    roster = DEFAULT_ROSTER;
  });

  it("lists Unassigned + every roster person when opened", () => {
    render(<AssigneePicker value={null} onChange={() => {}} />);
    open();
    expect(optionButtons().map((o) => o.textContent)).toEqual([
      "Unassigned",
      "Ada Lovelace@AdaLovelace",
      "Octo Cat@octocat",
    ]);
  });

  it("fires onChange with the roster login when a person is picked", () => {
    const onChange = vi.fn();
    render(<AssigneePicker value={null} onChange={onChange} />);
    open();
    fireEvent.click(screen.getByRole("button", { name: /Octo Cat/ }));
    expect(onChange).toHaveBeenCalledWith("person:2");
  });

  it("fires onChange(null) for Unassigned", () => {
    const onChange = vi.fn();
    render(<AssigneePicker value="octocat" onChange={onChange} />);
    open();
    fireEvent.click(screen.getByRole("button", { name: "Unassigned" }));
    expect(onChange).toHaveBeenCalledWith(null);
  });

  it("matches + highlights the current selection by usernameKey-folded login (case-insensitive)", () => {
    // Stored value differs in case from the roster login — must still match.
    render(<AssigneePicker value="adalovelace" onChange={() => {}} />);
    open();
    expect(screen.getByRole("button", { name: /Ada Lovelace/ })).toHaveClass("font-semibold");
  });

  it("does NOT re-write when re-picking the same person under a different case (no redundant write)", () => {
    const onChange = vi.fn();
    render(<AssigneePicker value="ADALOVELACE" onChange={onChange} />);
    open();
    fireEvent.click(screen.getByRole("button", { name: /Ada Lovelace/ }));
    expect(onChange).not.toHaveBeenCalled();
  });

  it("typeahead filters the list by name and login", () => {
    render(<AssigneePicker value={null} onChange={() => {}} />);
    open();
    fireEvent.change(screen.getByRole("textbox", { name: "Filter people" }), {
      target: { value: "octo" },
    });
    // Unassigned always stays; only Octo Cat matches.
    expect(optionButtons().map((o) => o.textContent)).toEqual(["Unassigned", "Octo Cat@octocat"]);
  });

  it("lists a handle-less person and writes their stable reference", () => {
    roster = [
      ...DEFAULT_ROSTER,
      {
        id: "p-grace",
        identities: [],
        github_user_id: 0,
        github_username: null,
        display_name: "Grace Hopper",
        avatar_url: null,
      },
    ];
    const onChange = vi.fn();
    render(<AssigneePicker value={null} onChange={onChange} />);
    open();
    const grace = screen.getByRole("button", { name: /Grace Hopper/ });
    expect(grace).toHaveTextContent("No code host linked");
    fireEvent.click(grace);
    expect(onChange).toHaveBeenCalledWith("person:p-grace");
  });

  it("finds people by every linked identity handle", () => {
    roster = [
      {
        id: "p-grace",
        identities: [{ provider: "GitLab", handle: "amazing-grace" }],
        github_user_id: 0,
        github_username: null,
        display_name: "Grace Hopper",
        avatar_url: null,
      },
    ];
    render(<AssigneePicker value={null} onChange={() => {}} />);
    open();
    fireEvent.change(screen.getByRole("textbox", { name: "Filter people" }), {
      target: { value: "amazing" },
    });
    expect(screen.getByRole("button", { name: /Grace Hopper/ })).toBeInTheDocument();
  });

  it("shows an empty state when nothing matches", () => {
    render(<AssigneePicker value={null} onChange={() => {}} />);
    open();
    fireEvent.change(screen.getByRole("textbox", { name: "Filter people" }), {
      target: { value: "zzz" },
    });
    expect(screen.getByText(/No people match/)).toBeInTheDocument();
  });

  it("marks an off-roster current assignee with a bolded 'not in roster' row", () => {
    // Assignee left the org / typo login: still show + highlight the current
    // value in the open list, not just on the trigger label.
    render(<AssigneePicker value="ghost" onChange={() => {}} />);
    open();
    const orphan = screen.getByRole("button", { name: /@ghost/ });
    expect(orphan).toHaveTextContent("not in roster");
    expect(orphan).toHaveClass("font-semibold");
    // No roster person is falsely highlighted.
    expect(screen.getByRole("button", { name: /Octo Cat/ })).not.toHaveClass("font-semibold");
  });

  it("treats a blank stored value as Unassigned (highlights Unassigned)", () => {
    // A hand-authored `assigned_to: ""` must not leave the list with nothing marked.
    render(<AssigneePicker value="  " onChange={() => {}} />);
    open();
    expect(screen.getByRole("button", { name: "Unassigned" })).toHaveClass("font-semibold");
  });

  it("shows no 'No people match' message for an empty roster with an empty filter", () => {
    roster = [];
    render(<AssigneePicker value={null} onChange={() => {}} />);
    open();
    // Only the Unassigned row; the empty-state message is gated on a typed query.
    expect(screen.queryByText(/No people match/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Unassigned" })).toBeInTheDocument();
  });

  it("stops click propagation so a card/node host is not triggered", () => {
    const hostClick = vi.fn();
    render(
      <div onClick={hostClick}>
        <AssigneePicker value={null} onChange={() => {}} />
      </div>,
    );
    open();
    expect(hostClick).not.toHaveBeenCalled();
  });

  it("renders a custom trigger (avatar) when children are provided", () => {
    render(
      <AssigneePicker value="octocat" onChange={() => {}}>
        <span data-testid="avatar-trigger">av</span>
      </AssigneePicker>,
    );
    const trigger = screen.getByRole("button", { name: "Assignee" });
    expect(within(trigger).getByTestId("avatar-trigger")).toBeInTheDocument();
  });

  it("forces the wrapper fully opaque while open (host hover-reveal can't hide the list)", () => {
    // The pipeline node reveals the unassigned trigger via `opacity-0` on this
    // wrapper; an open popover must not fade out if hover/focus is lost.
    render(<AssigneePicker value={null} onChange={() => {}} className="opacity-0" />);
    const wrapper = screen.getByRole("button", { name: "Assignee" }).parentElement!;
    expect(wrapper.style.opacity).toBe(""); // closed: host class governs
    open();
    expect(wrapper.style.opacity).toBe("1"); // open: forced visible inline
  });

  it("closes the list (commits nothing) if disabled flips true while open", () => {
    const onChange = vi.fn();
    const { rerender } = render(<AssigneePicker value={null} onChange={onChange} />);
    open();
    expect(screen.getByRole("button", { name: "Unassigned" })).toBeInTheDocument();
    // Host flips read-only while the popover is open — the list must disappear so
    // a lingering option click can't commit a write.
    rerender(<AssigneePicker value={null} onChange={onChange} disabled />);
    expect(screen.queryByRole("button", { name: "Unassigned" })).not.toBeInTheDocument();
  });
});

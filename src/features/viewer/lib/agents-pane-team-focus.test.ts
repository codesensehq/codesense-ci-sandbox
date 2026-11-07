// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { focusedTeam, toggleTeamFocus } from "./agents-pane-team-focus";

const TEAMS = ["a:one", "b:two", "c:three"];
const ALL = { shownTeams: TEAMS, allTeams: TEAMS };

describe("focusedTeam", () => {
  it("is the one team left showing when there are at least two", () => {
    expect(focusedTeam(TEAMS, ["b:two", "c:three"])).toBe("a:one");
    expect(focusedTeam(TEAMS, ["b:two"])).toBeNull();
    expect(focusedTeam(TEAMS, [])).toBeNull();
    expect(focusedTeam(TEAMS, TEAMS)).toBeNull();
    expect(focusedTeam(["a:one"], [])).toBeNull();
  });
});

describe("toggleTeamFocus", () => {
  it("on: hides every other team and keeps what was hidden before as the restore point", () => {
    expect(toggleTeamFocus("b:two", ALL, ["c:three"], null)).toEqual({
      hidden: ["a:one", "c:three"],
      restore: { teamKey: "b:two", hidden: ["c:three"] },
    });
  });

  it("off: puts back exactly what was hidden before", () => {
    const on = toggleTeamFocus("b:two", ALL, ["c:three"], null);
    expect(toggleTeamFocus("b:two", ALL, on.hidden, on.restore)).toEqual({
      hidden: ["c:three"],
      restore: null,
    });
  });

  it("off without a restore point for that team (set from the checklist) shows every team", () => {
    expect(toggleTeamFocus("a:one", ALL, ["b:two", "c:three"], null)).toEqual({
      hidden: [],
      restore: null,
    });
    expect(
      toggleTeamFocus("a:one", ALL, ["b:two", "c:three"], {
        teamKey: "b:two",
        hidden: ["c:three"],
      }),
    ).toEqual({ hidden: [], restore: null });
  });

  it("keeps a hidden team the pane no longer lists hidden, and never hides the focused team", () => {
    const on = toggleTeamFocus("a:one", ALL, ["gone:team", "a:one"], null);
    expect(on.hidden).toEqual(["gone:team", "b:two", "c:three"]);
    expect(toggleTeamFocus("a:one", ALL, on.hidden, on.restore).hidden).toEqual(["gone:team"]);
  });

  it("another team's funnel moves the focus there", () => {
    const on = toggleTeamFocus("a:one", ALL, [], null);
    const moved = toggleTeamFocus("c:three", ALL, on.hidden, on.restore);
    expect(moved.hidden).toEqual(["a:one", "b:two"]);
    expect(focusedTeam(TEAMS, moved.hidden)).toBe("c:three");
  });
});

describe("teams with no agents yet", () => {
  const TEAMS2 = { shownTeams: ["a:one", "b:two"], allTeams: ["a:one", "b:two", "e:empty"] };

  it("are hidden with the rest, so none appears under the funnel, and restored with them", () => {
    const on = toggleTeamFocus("a:one", TEAMS2, [], null);
    expect(on.hidden).toEqual(["b:two", "e:empty"]);
    expect(focusedTeam(TEAMS2.shownTeams, on.hidden)).toBe("a:one");
    expect(toggleTeamFocus("a:one", TEAMS2, on.hidden, on.restore).hidden).toEqual([]);
  });

  it("do not count toward whether a funnel is on", () => {
    expect(focusedTeam(["a:one"], ["e:empty"])).toBeNull();
    expect(focusedTeam(["a:one", "b:two"], ["e:empty"])).toBeNull();
  });
});

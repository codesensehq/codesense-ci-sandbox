// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import { isInHiddenTeam, queryAllShown, queryShown } from "./hidden-team";
import { isScopeActive } from "./use-scope-tier";
import { warmScopes } from "./warm-scopes";

afterEach(() => {
  document.body.innerHTML = "";
  warmScopes.forget("ht:a");
  warmScopes.forget("ht:b");
});

describe("hidden-team DOM rule", () => {
  it("skips matches inside an inline display:none ancestor", () => {
    document.body.innerHTML = `<div style="display:none"><i id="a" data-x></i></div><i id="b" data-x></i>`;
    expect(isInHiddenTeam(document.getElementById("a")!)).toBe(true);
    expect(queryShown("[data-x]")?.id).toBe("b");
    expect(queryAllShown("[data-x]").map((e) => e.id)).toEqual(["b"]);
  });

  it("is null when every match is hidden", () => {
    document.body.innerHTML = `<div style="display:none"><i data-x></i></div>`;
    expect(queryShown("[data-x]")).toBeNull();
  });
});

describe("isScopeActive", () => {
  it("is true only for the active scope, and for no scope", () => {
    warmScopes.activate("ht:a");
    warmScopes.activate("ht:b");
    expect(isScopeActive("ht:b")).toBe(true);
    expect(isScopeActive("ht:a")).toBe(false);
    expect(isScopeActive(undefined)).toBe(true);
  });
});

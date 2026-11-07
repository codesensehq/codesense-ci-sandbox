// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { lucideIconName, paneIcon } from "./pane-chrome";

describe("pane icon names", () => {
  it("accepts kebab-case and PascalCase, numbered names included", () => {
    expect(lucideIconName("git-pull-request")).toBe("git-pull-request");
    expect(lucideIconName("GitPullRequest")).toBe("git-pull-request");
    expect(lucideIconName("Building2")).toBe("building-2");
    expect(lucideIconName("Clock4")).toBe("clock-4");
  });

  it("gives one component per name, and the same '?' for an unknown one", () => {
    expect(paneIcon("Building2")).toBe(paneIcon("building-2"));
    expect(paneIcon("NotAnIcon")).toBe(paneIcon("NotAnIcon"));
    expect(paneIcon("NotAnIcon")).not.toBe(paneIcon("building-2"));
  });
});

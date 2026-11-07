// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { effectiveOpenCodeRules, wildcardCopy } from "./agent-permissions";

describe("wildcardCopy", () => {
  it("follows the wildcard rule's decision", () => {
    expect(wildcardCopy([{ pattern: "*", decision: "allow" }])).toBe("run without asking");
    expect(wildcardCopy([{ pattern: "*", decision: "deny" }])).toBe("are blocked");
    expect(wildcardCopy([{ pattern: "*", decision: "ask" }])).toBe("ask before running");
    expect(wildcardCopy([{ pattern: "bash", decision: "allow" }])).toBe("ask before running");
  });
});

describe("effectiveOpenCodeRules", () => {
  const generated = { "*": "ask", "codesense_*": "deny", codesense_who_am_i: "allow" };

  it("shows an allowlisted bridge tool as allowed beside the generated deny", () => {
    const rules = effectiveOpenCodeRules(generated, ["notes_read"]);
    expect(rules.codesense_notes_read).toBe("allow");
    expect(rules["codesense_*"]).toBe("deny");
    expect(rules.codesense_who_am_i).toBe("allow");
    expect(rules.codesense_notes_write).toBeUndefined();
  });

  it("adds nothing for an unrestricted bridge", () => {
    expect(effectiveOpenCodeRules({ "*": "ask", codesense_who_am_i: "allow" }, null)).toEqual({
      "*": "ask",
      codesense_who_am_i: "allow",
    });
  });

  it("lets the agent's own decision win over the scope grant", () => {
    const rules = effectiveOpenCodeRules({ codesense_notes_read: "deny" }, ["notes_read"]);
    expect(rules.codesense_notes_read).toBe("deny");
  });
});

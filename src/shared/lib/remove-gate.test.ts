// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  planRemoveGate,
  removeGateCopy,
  removeOutcomeToast,
  removeSuccessToast,
} from "@/shared/lib/remove-gate";
import type { ContextId } from "@/shared/lib/context-model";

const PERSONAL = "personal" as ContextId;
const TEAM = "team:acme/repo:default" as ContextId;

const BANNED = /un-?mint|overlay|\bprovider\b|\blayer\b|personal team|back ?up/i;

describe("planRemoveGate", () => {
  it("a shared team item is always a normal confirm", () => {
    expect(planRemoveGate(TEAM).kind).toBe("normal");
  });

  it("a scratch item gates on the sole-copy warning (§J)", () => {
    expect(planRemoveGate(PERSONAL).kind).toBe("soleCopyBlock");
  });
});

describe("removeGateCopy", () => {
  it("the sole-copy warning names the file and the only discarding path", () => {
    const copy = removeGateCopy("secret.md", { kind: "soleCopyBlock" });
    expect(copy.title).toContain("only on this machine");
    expect(copy.body).toContain("secret.md");
    expect(copy.confirmLabel).toBe("Delete permanently");
  });

  it("the normal confirm is a plain Remove", () => {
    const copy = removeGateCopy("note.md", { kind: "normal" });
    expect(copy.confirmLabel).toBe("Remove");
  });

  it("no gate copy leaks a banned word", () => {
    for (const kind of ["normal", "soleCopyBlock"] as const) {
      const copy = removeGateCopy("x.md", { kind });
      expect(`${copy.title} ${copy.body} ${copy.confirmLabel}`).not.toMatch(BANNED);
    }
  });
});

describe("removeOutcomeToast", () => {
  it("removed → success naming the file", () => {
    const t = removeOutcomeToast("removed", "note.md");
    expect(t.kind).toBe("success");
    expect(t.message).toBe(removeSuccessToast("note.md"));
  });

  it("source-missing reads as an informative confirmation, not an error", () => {
    const t = removeOutcomeToast("rejectedSourceMissing", "note.md");
    expect(t.kind).toBe("info");
    expect(t.message).toContain("already gone");
  });

  it("unbacked → a retry, never a dead-end", () => {
    const t = removeOutcomeToast("rejectedUnbacked", "note.md");
    expect(t.kind).toBe("error");
    expect(t.message).toContain("try again");
  });
});

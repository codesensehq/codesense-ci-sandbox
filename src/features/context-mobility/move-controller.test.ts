// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  personalContextId,
  teamAllContextId,
  teamRepoContextId,
  viewOnlyRepoContextId,
  type Context,
} from "@/shared/lib/context-model";
import {
  endpointForContextId,
  keepBothName,
  moveFailureToast,
  moveHeader,
  moveOutcomeToast,
  planMove,
  privacyGateCopy,
} from "@/features/context-mobility/move-controller";

const personalCtx: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Private",
};
const teamAllCtx: Context = {
  id: teamAllContextId("acme/repo", "default"),
  kind: "team-all",
  sharing: "shared",
  label: "All of default",
  teamName: "default",
};

// Banned internal words that must NEVER appear in any user-facing string (§M).
const BANNED =
  /\b(provider|overlay|layer|scratch pack|personal team|TeamWorkspace|TeamRepo|Workspace|mint)\b/i;

describe("endpointForContextId", () => {
  it("maps each writable kind to its backend endpoint", () => {
    expect(endpointForContextId(personalContextId())).toEqual({ kind: "personal" });
    expect(endpointForContextId(teamAllContextId("acme/repo", "default"))).toEqual({
      kind: "teamAll",
      orgSlug: "acme/repo",
      teamName: "default",
    });
    expect(
      endpointForContextId(
        teamRepoContextId("acme/repo", "default", { owner: "acme", repo: "api" }),
      ),
    ).toEqual({
      kind: "teamRepo",
      orgSlug: "acme/repo",
      teamName: "default",
      codeRepoSlug: { owner: "acme", repo: "api" },
    });
  });

  it("returns null for a view-only repo (never a move endpoint)", () => {
    expect(endpointForContextId(viewOnlyRepoContextId({ owner: "acme", repo: "api" }))).toBeNull();
  });
});

describe("moveHeader — persistent op restatement (§P)", () => {
  it("names the file and the target label; adds Step N of M only for multi-gate flows", () => {
    expect(moveHeader("note.md", teamAllCtx, 0, 1)).toBe("Moving 'note.md' → All of default");
    expect(moveHeader("note.md", teamAllCtx, 1, 3)).toBe(
      "Moving 'note.md' → All of default — Step 2 of 3",
    );
  });

  it("uses only the human label — no banned internal word", () => {
    expect(moveHeader("note.md", teamAllCtx, 0, 3)).not.toMatch(BANNED);
  });
});

describe("privacy gate copy (§I)", () => {
  it("says it becomes visible AND leaves the scratch space, with a Share it confirm", () => {
    const { body, confirmLabel } = privacyGateCopy("note.md", teamAllCtx);
    expect(body).toContain("visible to everyone on All of default");
    expect(body).toContain("takes it out of Private");
    expect(confirmLabel).toBe("Share it");
    expect(body).not.toMatch(BANNED);
  });
});

describe("keepBothName", () => {
  it("inserts -2 before the extension when the base collides", () => {
    expect(keepBothName("notes.md", (c) => c === "notes.md")).toBe("notes-2.md");
  });

  it("increments past existing -N siblings", () => {
    const taken = new Set(["notes-2.md", "notes-3.md"]);
    expect(keepBothName("notes.md", (c) => taken.has(c))).toBe("notes-4.md");
  });

  it("preserves the directory prefix and handles no-extension names", () => {
    expect(keepBothName("a/b/skill", (c) => c === "a/b/skill-2")).toBe("a/b/skill-3");
    expect(keepBothName("a/b/skill", () => false)).toBe("a/b/skill-2");
  });

  it("treats a leading-dot dotfile as having no extension", () => {
    // `.gitignore` -> `.gitignore-2`, not `-2.gitignore`.
    expect(keepBothName(".gitignore", () => false)).toBe(".gitignore-2");
  });
});

describe("planMove — the ladder through the controller", () => {
  it("shared->Personal clean no-collision plans zero gates", () => {
    expect(planMove(teamAllCtx.id, personalCtx.id, { targetCollision: false }).steps).toEqual([]);
  });

  it("Personal->Team colliding plans privacy then collision (a move never asks about a sole copy)", () => {
    expect(
      planMove(personalCtx.id, teamAllCtx.id, {
        targetCollision: true,
      }).steps.map((s) => s.kind),
    ).toEqual(["privacy", "collision"]);
  });
});

describe("moveOutcomeToast", () => {
  it("plain move names file and target", () => {
    const t = moveOutcomeToast({ kind: "moved" }, { fileName: "note.md", target: teamAllCtx });
    expect(t.kind).toBe("success");
    expect(t.message).toBe("Moved 'note.md' to All of default");
  });

  it("keep-both surfaces the renamed result by name (never silent)", () => {
    const t = moveOutcomeToast(
      { kind: "moved" },
      { fileName: "notes.md", target: teamAllCtx, renamedTo: "a/b/notes-2.md" },
    );
    expect(t.message).toBe("Moved to All of default as 'notes-2.md'");
  });

  it("every outcome message is free of banned internal words", () => {
    const outcomes = [
      { kind: "moved" as const },
      { kind: "rejectedSameContext" as const },
      { kind: "rejectedSourceMissing" as const },
      { kind: "rejectedTargetExists" as const },
      { kind: "rejectedUnbacked" as const },
    ];
    for (const o of outcomes) {
      expect(moveOutcomeToast(o, { fileName: "note.md", target: teamAllCtx }).message).not.toMatch(
        BANNED,
      );
    }
  });
});

describe("moveFailureToast — §N non-destructive retry", () => {
  it("reassures the source is intact and invites retry", () => {
    const m = moveFailureToast("note.md");
    expect(m).toContain("still where it was");
    expect(m).toContain("Try again");
    expect(m).not.toMatch(BANNED);
  });

  it("COPY variant says 'copy' not 'move'", () => {
    const m = moveFailureToast("note.md", "copy");
    expect(m).toContain("Couldn't copy 'note.md'");
    expect(m).not.toMatch(BANNED);
  });
});

describe("SLW-PT-13 COPY — controller op variants", () => {
  it("header uses the Copying verb for a copy", () => {
    expect(moveHeader("note.md", teamAllCtx, 0, 1, "copy")).toBe(
      "Copying 'note.md' → All of default",
    );
    expect(moveHeader("note.md", teamAllCtx, 1, 3, "copy")).toBe(
      "Copying 'note.md' → All of default — Step 2 of 3",
    );
  });

  it("privacy gate for a COPY omits the 'takes it out' clause but still confirms Share it", () => {
    const { body, confirmLabel } = privacyGateCopy("note.md", teamAllCtx, "copy");
    expect(body).toContain("visible to everyone on All of default");
    expect(body).not.toContain("takes it out of Private");
    expect(confirmLabel).toBe("Share it");
    expect(body).not.toMatch(BANNED);
  });

  it("copy outcome toast names file and target; keep-both surfaces the renamed copy", () => {
    const plain = moveOutcomeToast({ kind: "copied" }, { fileName: "note.md", target: teamAllCtx });
    expect(plain.kind).toBe("success");
    expect(plain.message).toBe("Copied 'note.md' to All of default");

    const renamed = moveOutcomeToast(
      { kind: "copied" },
      { fileName: "notes.md", target: teamAllCtx, renamedTo: "a/b/notes-2.md" },
    );
    expect(renamed.message).toBe("Copied to All of default as 'notes-2.md'");
    expect(renamed.message).not.toMatch(BANNED);
  });

  it("COPY Personal->Team never plans sole_copy through the controller", () => {
    expect(
      planMove(personalCtx.id, teamAllCtx.id, {
        targetCollision: true,
        op: "copy",
      }).steps.map((s) => s.kind),
    ).toEqual(["privacy", "collision"]);
  });
});

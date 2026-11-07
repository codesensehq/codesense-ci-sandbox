// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  DomFocusId,
  PtySessionId,
  StoryWorkspaceId,
  TerminalTabId,
  ViewportId,
  identityValue,
  type ViewportId as ViewportIdT,
} from "./identities";

/**
 * OC-4 AC1 — the nine identities are distinct types. The load-bearing assertions
 * here are the `@ts-expect-error` lines: they only compile (i.e. the suite only
 * typechecks) if the substitution they mark is genuinely rejected by the
 * compiler. If a brand were dropped and any string flowed anywhere, the
 * `@ts-expect-error` would become unused and `tsc` would fail — so this file is a
 * compile-time proof that invalid substitutions cannot happen.
 */

function attachViewportToPty(viewport: ViewportIdT, session: PtySessionId): string {
  return `${identityValue(viewport)}@${identityValue(session)}`;
}

describe("OC-4 identities are nominally distinct (AC1)", () => {
  it("rejects passing one identity where another is required", () => {
    const viewport = ViewportId("vp-1");
    const session = PtySessionId("pty-1");
    const tab = TerminalTabId("tab-1");
    const workspace = StoryWorkspaceId("ws-1");

    // Correct order compiles.
    expect(attachViewportToPty(viewport, session)).toBe("vp-1@pty-1");

    // @ts-expect-error a PtySessionId is not a ViewportId
    attachViewportToPty(session, session);
    // @ts-expect-error a TerminalTabId is not a PtySessionId
    attachViewportToPty(viewport, tab);
    // @ts-expect-error a StoryWorkspaceId is not a ViewportId
    attachViewportToPty(workspace, session);
    // @ts-expect-error a raw string cannot be laundered into an identity
    attachViewportToPty("vp-1", session);
  });

  it("constructors and identityValue round-trip the underlying string", () => {
    expect(identityValue(DomFocusId("focus-1"))).toBe("focus-1");
    expect(identityValue(PtySessionId("pty-9"))).toBe("pty-9");
  });

  it("same-brand values remain freely assignable", () => {
    const a = ViewportId("vp-a");
    const b: ViewportIdT = a;
    expect(identityValue(b)).toBe("vp-a");
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render } from "@testing-library/react";

// Replace the builder with a call-counting stub returning a valid empty view, so
// we can assert the component's `useMemo` skips the recompute on an
// unchanged-input re-render (AC-10). Isolated to this file (vi.mock is per-file),
// so the real-output component suites are unaffected. We assert call COUNT, not
// output — the pure builder's correctness is covered by build-connectivity-view.test.ts.
const EMPTY_VIEW = {
  scope: "",
  rows: [],
  orphans: [],
  summary: {
    storyCount: 0,
    contextDesertCount: 0,
    thinCount: 0,
    healthyCount: 0,
    orphanCount: 0,
    totalContextLinks: 0,
    unresolvedCount: 0,
  },
};
vi.mock("@/shared/lib/connectivity/build-connectivity-view", () => ({
  buildConnectivityView: vi.fn(() => EMPTY_VIEW),
}));

import { ConnectivityView } from "./connectivity-view";
import { buildConnectivityView } from "@/shared/lib/connectivity/build-connectivity-view";
import type { DocSummary } from "@/shared/lib/doc-query/types";

const A = "/ctx/epic-a";
const spy = vi.mocked(buildConnectivityView);

function doc(path: string): DocSummary {
  const fileName = path.split("/").pop()!;
  return {
    id: fileName,
    path,
    fileName,
    tags: {},
    status: null,
    documentType: "story",
    mtime: null,
    sourceRepo: null,
    sprint: null,
    points: null,
    assignedTo: null,
    relationships: { supportingDocs: [], backlinks: [], dependsOn: [] },
  };
}

describe("ConnectivityView — memoization (AC-10)", () => {
  beforeEach(() => spy.mockClear());

  it("does NOT recompute buildConnectivityView on a same-input re-render", () => {
    const docs = [doc(`${A}/a1.md`)];
    const props = { docs, scopeRoot: A, scopeLabel: "epic-a", scopeFound: true };
    const { rerender } = render(<ConnectivityView {...props} />);
    const callsAfterFirstRender = spy.mock.calls.length;
    expect(callsAfterFirstRender).toBeGreaterThan(0);

    // Re-render with the SAME references — the memo key is unchanged.
    rerender(<ConnectivityView {...props} />);
    expect(spy.mock.calls.length).toBe(callsAfterFirstRender); // no extra call
  });

  it("recomputes when the docs identity changes", () => {
    const props = { scopeRoot: A, scopeLabel: "epic-a", scopeFound: true };
    const { rerender } = render(<ConnectivityView docs={[doc(`${A}/a1.md`)]} {...props} />);
    const before = spy.mock.calls.length;
    // A new docs array (new identity) invalidates the memo.
    rerender(<ConnectivityView docs={[doc(`${A}/a1.md`)]} {...props} />);
    expect(spy.mock.calls.length).toBeGreaterThan(before);
  });
});

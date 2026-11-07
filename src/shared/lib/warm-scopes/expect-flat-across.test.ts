// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { expectFlatAcross } from "./__test-utils__/expect-flat-across";
import { WarmScopes } from "./warm-scopes";

const A = "org1:a";
const B = "org1:b";
const C = "org2:c";

describe("expectFlatAcross on the owner", () => {
  it("50x A<->B and cross-org A<->C leave holds, tiers and activations bounded", async () => {
    const ws = new WarmScopes({ warmCapacity: 3, suspendedAfterSwitches: 4 });
    // Each visit re-acquires the same watch: replace-on-reacquire keeps one entry per scope.
    const visit = (scope: string) => {
      ws.activate(scope);
      ws.hold(scope, "watch", "paneSources", () => {});
    };
    visit(A);
    const roundTripAB = () => {
      visit(B);
      visit(A);
    };
    const roundTripAC = () => {
      visit(C);
      visit(A);
    };
    const read = () => ({
      holds: ws.debugCounts().holdsTotal,
      tracked: Object.keys(ws.debugCounts().tiers).length,
    });
    await expectFlatAcross(roundTripAB, read);
    await expectFlatAcross(roundTripAC, read);
    expect(ws.debugCounts().holdsTotal).toBeLessThanOrEqual(3);
  });

  it("fails when a counter grows", async () => {
    let n = 0;
    await expect(
      expectFlatAcross(
        () => void n++,
        () => ({ n }),
        { repeat: 3 },
      ),
    ).rejects.toThrow();
  });

  it("allows a named counter to grow by its allowance", async () => {
    let n = 0;
    await expectFlatAcross(
      () => void n++,
      () => ({ n }),
      { repeat: 3, allowed: { n: 3 } },
    );
  });
});

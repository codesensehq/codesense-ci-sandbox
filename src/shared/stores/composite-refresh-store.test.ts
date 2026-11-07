// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";

import { useCompositeRefreshStore } from "./composite-refresh-store";

beforeEach(() => {
  useCompositeRefreshStore.setState({ nonces: {} });
});

describe("composite-refresh-store (SLW-NC-5)", () => {
  it("an unbumped team reads as 0", () => {
    expect(useCompositeRefreshStore.getState().nonces["alderco:default"] ?? 0).toBe(0);
  });

  it("bumps a team's nonce monotonically", () => {
    const { bumpCompositeRefresh } = useCompositeRefreshStore.getState();
    bumpCompositeRefresh("alderco:default");
    expect(useCompositeRefreshStore.getState().nonces["alderco:default"]).toBe(1);
    bumpCompositeRefresh("alderco:default");
    expect(useCompositeRefreshStore.getState().nonces["alderco:default"]).toBe(2);
  });

  it("isolates nonces per team — bumping one leaves others at 0", () => {
    useCompositeRefreshStore.getState().bumpCompositeRefresh("alderco:default");
    expect(useCompositeRefreshStore.getState().nonces["alderco:other"] ?? 0).toBe(0);
    expect(useCompositeRefreshStore.getState().nonces["alderco:default"]).toBe(1);
  });
});

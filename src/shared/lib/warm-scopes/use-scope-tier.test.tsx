// SPDX-License-Identifier: MIT
import { act, render, renderHook } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { warmScopes } from "./warm-scopes";
import { useScopeTier } from "./use-scope-tier";
import { useScopedResource } from "./use-scoped-resource";
import { useMountedScopes } from "./use-mounted-scopes";

const A = "tier:a";
const B = "tier:b";
const C = "tier:c";
const D = "tier:d";

afterEach(() => {
  for (const k of [A, B, C, D]) warmScopes.forget(k);
});

describe("useScopeTier", () => {
  it("follows activation and is gone for a null scope", () => {
    const { result } = renderHook(({ scope }) => useScopeTier(scope), {
      initialProps: { scope: A as string | null },
    });
    expect(result.current).toBe("gone");
    act(() => warmScopes.activate(A));
    expect(result.current).toBe("active");
    act(() => warmScopes.activate(B));
    expect(result.current).toBe("warm");
  });
});

describe("useScopedResource", () => {
  it("acquires on mount and releases on unmount, exactly once", () => {
    act(() => warmScopes.activate(A));
    const release = vi.fn();
    const acquire = vi.fn(() => release);
    const { unmount } = render(<Probe scope={A} acquire={acquire} />);
    expect(acquire).toHaveBeenCalledTimes(1);
    expect(warmScopes.debugCounts().held[A]).toEqual({ watch: 1 });
    unmount();
    expect(release).toHaveBeenCalledTimes(1);
    expect(warmScopes.debugCounts().held[A]).toBeUndefined();
  });

  it("supports a Disposable and under StrictMode keeps a single live entry", () => {
    act(() => warmScopes.activate(A));
    let live = 0;
    const acquire = () => {
      live += 1;
      return { dispose: () => void (live -= 1) };
    };
    const { unmount } = render(
      <StrictMode>
        <Probe scope={A} acquire={acquire} />
      </StrictMode>,
    );
    expect(live).toBe(1);
    expect(warmScopes.debugCounts().held[A]).toEqual({ watch: 1 });
    unmount();
    expect(live).toBe(0);
  });

  it("is released by the owner when the scope goes gone, not again on unmount", () => {
    act(() => {
      for (const k of [A, B, C, D]) warmScopes.activate(k);
    });
    const release = vi.fn();
    const { unmount } = render(<Probe scope={A} acquire={() => release} />);
    act(() => warmScopes.forget(A));
    expect(release).toHaveBeenCalledTimes(1);
    unmount();
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("a rerender with an inline acquire does not re-acquire", () => {
    act(() => warmScopes.activate(A));
    const calls = vi.fn();
    const { rerender } = render(<Probe scope={A} acquire={() => (calls(), () => {})} />);
    rerender(<Probe scope={A} acquire={() => (calls(), () => {})} />);
    expect(calls).toHaveBeenCalledTimes(1);
  });
});

function Probe({
  scope,
  acquire,
}: {
  scope: string;
  acquire: () => (() => void) | { dispose(): void };
}) {
  useScopedResource(scope, "watch", "k", acquire);
  return null;
}

describe("useMountedScopes", () => {
  it("returns the active and warm candidates in the candidates' order, following the owner", () => {
    const candidates = [A, B, C, D];
    const { result } = renderHook(() => useMountedScopes(candidates));
    expect(result.current).toEqual([]);
    act(() => {
      for (const k of [A, B, C]) warmScopes.activate(k);
    });
    expect(result.current).toEqual([A, B, C]);
    act(() => warmScopes.activate(D));
    expect(result.current).toEqual([B, C, D]);
  });
});

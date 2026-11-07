// SPDX-License-Identifier: MIT
import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { warmScopes } from "./warm-scopes";
import { useFocusWhenShown } from "./use-focus-when-shown";

const A = "focus:a";
const B = "focus:b";

afterEach(() => {
  for (const k of [A, B]) warmScopes.forget(k);
});

function Probe({
  scope,
  enabled = true,
  refocus,
}: {
  scope: string | null;
  enabled?: boolean;
  refocus: () => void;
}) {
  useFocusWhenShown(scope, enabled, refocus);
  return null;
}

describe("useFocusWhenShown", () => {
  it("refocuses when the scope returns to active, not on first mount or while hidden", () => {
    const refocus = vi.fn();
    act(() => warmScopes.activate(A));
    render(<Probe scope={A} refocus={refocus} />);
    expect(refocus).not.toHaveBeenCalled();
    act(() => warmScopes.activate(B));
    expect(refocus).not.toHaveBeenCalled();
    act(() => warmScopes.activate(A));
    expect(refocus).toHaveBeenCalledTimes(1);
  });

  it("stays quiet while disabled, and for a component with no scope", () => {
    const refocus = vi.fn();
    act(() => warmScopes.activate(A));
    render(<Probe scope={A} enabled={false} refocus={refocus} />);
    render(<Probe scope={null} refocus={refocus} />);
    act(() => warmScopes.activate(B));
    act(() => warmScopes.activate(A));
    expect(refocus).not.toHaveBeenCalled();
  });
});

// SPDX-License-Identifier: MIT
import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { OwningScopeProvider, ScopeGate, ScopedPortal, useOwnsScope } from "./owning-scope";
import { warmScopes } from "./warm-scopes";

const A = "owning:a";
const B = "owning:b";

afterEach(() => {
  for (const k of [A, B]) warmScopes.forget(k);
});

function Owns() {
  return <span data-testid="owns">{String(useOwnsScope())}</span>;
}

describe("owning scope", () => {
  it("owns outside any team, and only while the owning team is on screen inside one", () => {
    act(() => warmScopes.activate(A));
    render(
      <>
        <Owns />
        <OwningScopeProvider scope={B}>
          <span data-testid="inner">
            <Owns />
          </span>
        </OwningScopeProvider>
      </>,
    );
    const [outer, inner] = screen.getAllByTestId("owns");
    expect(outer.textContent).toBe("true");
    expect(inner.textContent).toBe("false");
    act(() => warmScopes.activate(B));
    expect(screen.getAllByTestId("owns")[1].textContent).toBe("true");
  });

  it("ScopeGate and ScopedPortal render nothing while the owning team is hidden", () => {
    act(() => warmScopes.activate(A));
    render(
      <OwningScopeProvider scope={A}>
        <ScopeGate>
          <p data-testid="gated" />
        </ScopeGate>
        <ScopedPortal container={document.body}>
          <p data-testid="portaled" />
        </ScopedPortal>
      </OwningScopeProvider>,
    );
    expect(screen.getByTestId("gated")).toBeInTheDocument();
    expect(screen.getAllByTestId("portaled")).toHaveLength(1);
    act(() => warmScopes.activate(B));
    expect(screen.queryByTestId("gated")).not.toBeInTheDocument();
    expect(screen.queryByTestId("portaled")).not.toBeInTheDocument();
    act(() => warmScopes.activate(A));
    expect(screen.getAllByTestId("portaled")).toHaveLength(1);
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { useState } from "react";

const errorSpy = vi.fn<(tag: string, msg: string) => void>();
vi.mock("@/shared/lib/logger", () => ({
  logger: { error: (tag: string, msg: string) => errorSpy(tag, msg) },
}));

import { ErrorBoundary } from "./error-boundary";

// A named component so the captured React component stack includes "Boom".
function Boom({ message = "boom" }: { message?: string }): never {
  throw new Error(message);
}

beforeEach(() => {
  errorSpy.mockReset();
  // React logs caught boundary errors to console.error; silence the noise.
  vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("ErrorBoundary", () => {
  it("renders children when nothing throws", () => {
    render(
      <ErrorBoundary scope="t" fallback={<div>fallback</div>}>
        <div>ok</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText("ok")).toBeInTheDocument();
    expect(errorSpy).not.toHaveBeenCalled();
  });

  it("degrades to the fallback and logs scope + error + component stack on a render throw", () => {
    render(
      <ErrorBoundary scope="root" fallback={<div>fallback</div>}>
        <Boom message="kaboom-xyz" />
      </ErrorBoundary>,
    );

    expect(screen.getByText("fallback")).toBeInTheDocument();
    expect(errorSpy).toHaveBeenCalledTimes(1);
    const [tag, msg] = errorSpy.mock.calls[0];
    expect(tag).toBe("root");
    expect(msg).toContain("render error:");
    expect(msg).toContain("kaboom-xyz");
    // The component-stack capture (SF-11) — names the component that threw.
    expect(msg).toContain("component stack:");
    expect(msg).toContain("Boom");
  });

  it("stays failed while the element key is stable (no componentDidUpdate reset)", () => {
    const { rerender } = render(
      <ErrorBoundary key="k1" scope="t" fallback={<div>fallback</div>}>
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByText("fallback")).toBeInTheDocument();

    // Same key, child no longer throws — mirrors "switch away and back" on a
    // display:none-kept-alive team. The boundary must NOT reset without a remount.
    rerender(
      <ErrorBoundary key="k1" scope="t" fallback={<div>fallback</div>}>
        <div>recovered</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText("fallback")).toBeInTheDocument();
    expect(screen.queryByText("recovered")).not.toBeInTheDocument();
  });

  it("resets when the element key changes (remount)", () => {
    const { rerender } = render(
      <ErrorBoundary key="k1" scope="t" fallback={<div>fallback</div>}>
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByText("fallback")).toBeInTheDocument();

    rerender(
      <ErrorBoundary key="k2" scope="t" fallback={<div>fallback</div>}>
        <div>recovered</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText("recovered")).toBeInTheDocument();
  });

  it("recovers a crashed subtree via a per-team reset nonce carried in the key (AC-4)", () => {
    // Mirrors Workbench's per-team pattern: a nonce-in-key boundary whose fallback
    // bumps the nonce to remount just that slot.
    let shouldThrow = true;
    function Conditional() {
      if (shouldThrow) throw new Error("team-boom");
      return <div>team-ok</div>;
    }
    function Harness() {
      const [nonce, setNonce] = useState<Record<string, number>>({});
      const tk = "org:team";
      return (
        <ErrorBoundary
          key={`team-eb:${tk}:${nonce[tk] ?? 0}`}
          scope={`team:${tk}`}
          fallback={
            <button onClick={() => setNonce((m) => ({ ...m, [tk]: (m[tk] ?? 0) + 1 }))}>
              retry
            </button>
          }
        >
          <Conditional />
        </ErrorBoundary>
      );
    }

    render(<Harness />);
    expect(screen.getByText("retry")).toBeInTheDocument();

    // The crash cleared; Retry bumps the nonce → remount → clean subtree.
    shouldThrow = false;
    fireEvent.click(screen.getByText("retry"));
    expect(screen.getByText("team-ok")).toBeInTheDocument();
  });
});

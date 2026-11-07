// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { PermissionGate, permissionSatisfies } from "./permission-gate";

describe("permissionSatisfies (rule table)", () => {
  it("null passes every requirement (local-only = full control)", () => {
    expect(permissionSatisfies(null, "admin")).toBe(true);
    expect(permissionSatisfies(null, "any-writer")).toBe(true);
  });

  it("admin satisfies admin and any-writer", () => {
    expect(permissionSatisfies("admin", "admin")).toBe(true);
    expect(permissionSatisfies("admin", "any-writer")).toBe(true);
  });

  it("write does NOT satisfy admin but satisfies any-writer", () => {
    expect(permissionSatisfies("write", "admin")).toBe(false);
    expect(permissionSatisfies("write", "any-writer")).toBe(true);
  });

  it("read satisfies neither admin nor any-writer", () => {
    expect(permissionSatisfies("read", "admin")).toBe(false);
    expect(permissionSatisfies("read", "any-writer")).toBe(false);
  });
});

// SW-OB-7 (open access): the gate is neutralized to always-allow. It renders
// its children unconditionally and never consults the workspace permission, so
// `required`/`fallback`/permission tier no longer affect rendering. The
// `permissionSatisfies` rule table above is retained (inert) for a future
// re-add and is still validated directly.
describe("<PermissionGate> (always-allow)", () => {
  it("renders children regardless of the required tier", () => {
    render(
      <PermissionGate required="admin">
        <span data-testid="gated">ok</span>
      </PermissionGate>,
    );
    expect(screen.getByTestId("gated")).toBeInTheDocument();
  });

  it("renders children even when a restrictive permission is set (no denial)", () => {
    render(
      <PermissionGate required="admin">
        <span data-testid="gated">ok</span>
      </PermissionGate>,
    );
    expect(screen.getByTestId("gated")).toBeInTheDocument();
  });

  it("ignores the fallback — children always win", () => {
    render(
      <PermissionGate required="admin" fallback={<span data-testid="fallback">ro</span>}>
        <span data-testid="gated">ok</span>
      </PermissionGate>,
    );
    expect(screen.getByTestId("gated")).toBeInTheDocument();
    expect(screen.queryByTestId("fallback")).not.toBeInTheDocument();
  });
});

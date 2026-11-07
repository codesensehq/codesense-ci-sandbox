// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { homeBaseHandlers } from "./home-base";
import type { ReconcileReport } from "@/tauri/commands";

describe("homeBaseHandlers", () => {
  it("reconciles required providers to an all-empty report (no gaps on the happy path)", () => {
    const h = homeBaseHandlers();
    const report = h.reconcile_required_providers({
      orgRoot: "/mock/.codesense/users/4242/orgs/acme/widgets",
    }) as ReconcileReport;
    expect(report.gaps).toEqual([]);
    expect(report.comingSoon).toEqual([]);
    expect(report.unsupported).toEqual([]);
  });
});

// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";

import {
  acquireInstance,
  snapshotInstanceCounts,
  __resetInstanceCountsForTest,
} from "./fe-instance-registry";

afterEach(() => __resetInstanceCountsForTest());

describe("fe-instance-registry", () => {
  it("starts at zero for every kind", () => {
    expect(snapshotInstanceCounts()).toEqual({
      editors: 0,
      iframes: 0,
      webglTerminals: 0,
      teams: 0,
    });
  });

  it("increments on acquire and decrements on release, isolated per kind", () => {
    const releaseEditor = acquireInstance("editor");
    acquireInstance("editor");
    acquireInstance("iframe");
    expect(snapshotInstanceCounts()).toMatchObject({ editors: 2, iframes: 1 });

    releaseEditor();
    expect(snapshotInstanceCounts()).toMatchObject({ editors: 1, iframes: 1 });
  });

  it("release is single-shot — a double call decrements only once", () => {
    acquireInstance("team");
    const release = acquireInstance("team");
    release();
    release();
    expect(snapshotInstanceCounts().teams).toBe(1);
  });

  it("maps each kind to its distinct snapshot field", () => {
    acquireInstance("iframe");
    acquireInstance("webglTerminal");
    expect(snapshotInstanceCounts()).toEqual({
      editors: 0,
      iframes: 1,
      webglTerminals: 1,
      teams: 0,
    });
  });
});

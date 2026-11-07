// SPDX-License-Identifier: MIT
import { readFileSync } from "fs";
import { resolve } from "path";
import { beforeEach, describe, expect, it } from "vitest";

import { readPendingJoinUrl, setPendingJoinUrl } from "@/shared/lib/pending-join";
import {
  LOCAL_GUEST_USER_ID,
  readPersistedFirstRun,
  resetFirstRunStoreForTest,
  useFirstRunStore,
} from "./first-run-store";

beforeEach(() => {
  globalThis.localStorage?.clear();
  resetFirstRunStoreForTest();
});

describe("first-run store — local guest", () => {
  it("a fresh install is not a guest", () => {
    expect(useFirstRunStore.getState().guest).toBe(false);
  });

  it("entering as a guest needs no account and survives a reload", () => {
    useFirstRunStore.getState().enterAsGuest();
    expect(useFirstRunStore.getState().guest).toBe(true);
    expect(readPersistedFirstRun()).toMatchObject({ guest: true });
  });
});

describe("LOCAL_GUEST_USER_ID parity with the backend", () => {
  it("matches the Rust guest principal, so both sides key the same users/<id>/ workspace", () => {
    const source = readFileSync(
      resolve(__dirname, "../../../src-tauri/src/domain/principal.rs"),
      "utf-8",
    );
    const match = /pub const LOCAL_GUEST_USER_ID: u64 = ([\d_]+);/.exec(source);
    expect(match).not.toBeNull();
    expect(Number(match![1].replace(/_/g, ""))).toBe(LOCAL_GUEST_USER_ID);
  });
});

describe("first-run store — a join survives sign-in (SW-FU-B6)", () => {
  it("a guest signing in with a join form open keeps the join pending", () => {
    useFirstRunStore.getState().enterAsGuest();
    setPendingJoinUrl("https://github.com/acme/team");
    useFirstRunStore.getState().clearGuest();
    expect(readPersistedFirstRun()).toMatchObject({ guest: false, joinPending: true });
    expect(readPendingJoinUrl()).toBe("https://github.com/acme/team");
  });

  it("a guest signing in with no join open has nothing pending", () => {
    useFirstRunStore.getState().enterAsGuest();
    useFirstRunStore.getState().clearGuest();
    expect(readPersistedFirstRun().joinPending).toBe(false);
  });

  it("landing keeps the URL; ending the join forgets both", () => {
    useFirstRunStore.setState({ joinPending: true });
    setPendingJoinUrl("https://github.com/acme/team");
    useFirstRunStore.getState().landJoinPending();
    expect(readPersistedFirstRun().joinPending).toBe(false);
    expect(readPendingJoinUrl()).toBe("https://github.com/acme/team");
    useFirstRunStore.getState().endJoin();
    expect(readPendingJoinUrl()).toBeNull();
  });
});

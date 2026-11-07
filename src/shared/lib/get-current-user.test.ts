// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import {
  useLocalProfileStore,
  numericPrincipal,
  type LocalProfile,
} from "@/shared/stores/local-profile-store";
import { buildCurrentUser, currentGithubUserId, useCurrentUser } from "./get-current-user";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const profile: LocalProfile = {
  principal: "999999999999",
  displayName: "Local",
  machineOwner: null,
};
beforeEach(() => useLocalProfileStore.setState({ status: null, initialized: false }));
it("preserves the local namespace for personal context without provider authentication", () => {
  useLocalProfileStore.setState({
    status: { active: profile, profiles: [profile], privateAuthority: null },
    initialized: true,
  });
  expect(currentGithubUserId()).toBe(999999999999);
  expect(buildCurrentUser(profile)?.principal).toBe(profile.principal);
});
it.each(["0", "../42", "042", "9007199254740993", "1e3"])(
  "rejects unsafe numeric namespace %s",
  (value) => expect(() => numericPrincipal(value)).toThrow(),
);
it("keeps ambiguous selection empty and reacts to the selected profile", () => {
  const { result } = renderHook(() => useCurrentUser());
  expect(result.current).toBeNull();
  act(() =>
    useLocalProfileStore.setState({
      status: { active: profile, profiles: [profile], privateAuthority: null },
    }),
  );
  expect(result.current?.github_user_id).toBe(999999999999);
});

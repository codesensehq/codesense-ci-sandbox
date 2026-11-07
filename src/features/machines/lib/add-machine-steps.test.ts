// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { fixFor, nextFromReport } from "./add-machine-steps";

describe("fixFor", () => {
  // The messages as private_account.rs sends them (OTHER_PROFILE and TIMED_OUT there).
  it("a login set up for a different CodeSense identity asks for another user, not a retry", () => {
    const f = fixFor("this machine login is already set up for a different CodeSense identity");
    expect(f.fix).toMatch(/another CodeSense profile/);
    expect(f.actions).toEqual(["editAddress"]);
  });

  it("a private setup timeout is not taken for an unreachable address", () => {
    const f = fixFor(
      "private account setup timed out after 30 seconds while staging the private bootstrap helper",
    );
    expect(f.fix).toMatch(/did not finish in time/);
    expect(f.actions).toEqual(["retry", "editAddress"]);
  });

  it("each kind of failed private setup step gets its own fix", () => {
    const step = "uploading the private bootstrap helper";
    expect(
      fixFor(`private account setup couldn't sign in while ${step}: authentication failed`).fix,
    ).toMatch(/refused your key/);
    expect(
      fixFor(`private account setup failed while ${step}: the connection closed before it finished`)
        .fix,
    ).toMatch(/connection dropped/);
    expect(
      fixFor(
        `private account setup failed while ${step} (exit 1); the CodeSense log has the details`,
      ).fix,
    ).toMatch(/SSH to the machine from Terminal/);
  });

  it("a connection timeout still gets the address advice", () => {
    expect(fixFor("ssh: connect to host 192.0.2.4 port 22: Operation timed out").fix).toMatch(
      /firewall/,
    );
  });
});

it("keeps a runtime prerequisite warning advisory for machine connectivity", () => {
  expect(
    nextFromReport({
      steps: [
        {
          id: "agentPrerequisite",
          status: "warn",
          detail: "Sandbox needs user namespaces",
          nextSteps: [],
        },
      ],
      installPlan: null,
      fileViewOnly: false,
    }),
  ).toEqual({ kind: "ready" });
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { machineHandlers } from "@/app/mock/handlers/machines";
import type { HandlerRegistry } from "@/app/mock/mock-router";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { MachineDraftInput, MachineTestReport, MachineTestStep } from "@/tauri/commands";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
  ZERO_LATENCY,
} from "../__test-utils__/machines-backend";
import { useMachinesStore } from "../stores/machines-store";
import { MachineReserve } from "./machine-reserve";

/**
 * The add flow against a fake fresh Ubuntu host that answers like the real
 * private-account backend: a first test shows the host key; with it trusted, a
 * test asks for enrollment, then for the terminal server, then passes; save
 * refuses until both are done; connect connects; the shell opens through the
 * same remote-tab command the New terminal chooser uses.
 */
const FINGERPRINT = "SHA256:Zq0dropletHostKeyFp4e";

function freshHost(overrides: (base: HandlerRegistry) => HandlerRegistry = () => ({})) {
  const base = machineHandlers("none", ZERO_LATENCY);
  const host = { enrolled: false, installed: false };
  const report = (step: MachineTestStep): MachineTestReport => ({
    steps: [step],
    installPlan: null,
    fileViewOnly: false,
  });
  const account = (status: MachineTestStep["status"], detail: string, next: string[]) =>
    report({
      id: "private-account",
      status,
      detail,
      nextSteps: next as MachineTestStep["nextSteps"],
    });
  const handlers: HandlerRegistry = {
    ...base,
    test_machine_connection: ({ input }) => {
      const draft = input as MachineDraftInput;
      if (!draft.trustedHostKeyFp)
        return report({
          id: "hostKey",
          status: "needsTrust",
          detail: `First time connecting to ${draft.host}. Check its fingerprint before trusting it.`,
          nextSteps: [],
          fingerprint: FINGERPRINT,
        });
      if (draft.trustedHostKeyFp !== FINGERPRINT) throw new Error("the machine's host key changed");
      if (!host.enrolled)
        return account("warn", "This Unix account needs explicit private enrollment", [
          "enroll-private-account",
        ]);
      if (!host.installed)
        return account("warn", "Private account enrolled; install its terminal server", [
          "install-terminal-server",
        ]);
      return account("ok", "Private account and terminal server verified", []);
    },
    enroll_private_machine_account: () => {
      host.enrolled = true;
      return account("warn", "Private account enrolled", ["install-terminal-server"]);
    },
    install_terminal_server: () => {
      if (!host.enrolled) throw new Error("Explicit private account enrollment is required");
      host.installed = true;
      return { ok: true, version: "0.0.15", logoutWarning: null };
    },
    save_machine: (args) => {
      if (args.connect && !(host.enrolled && host.installed))
        throw new Error("Install the private terminal server first");
      return base.save_machine!(args);
    },
    terminal_remote_shell_tab: () => ({ status: "pending" }),
  };
  Object.assign(handlers, overrides(base));
  return { host, ...installMachinesBackend("none", handlers) };
}

beforeEach(() => {
  resetMachinesUi();
  useViewerStore.setState({
    workspaces: { [TEAM_KEY]: [{ id: "ws-1", kind: "root", name: "repo", path: "/repo" }] },
    activeWorkspace: { [TEAM_KEY]: "ws-1" },
  } as never);
});

async function openAdd(onClose = vi.fn()) {
  render(<MachineReserve panel={{ mode: "add" }} teamKey={TEAM_KEY} onClose={onClose} />);
  await screen.findByTestId("add-machine-panel");
  return onClose;
}

async function typeAddress(value: string) {
  const input = screen.getByLabelText("Address");
  await userEvent.clear(input);
  await userEvent.type(input, value);
}

const step = (id: string) => screen.getByTestId(`add-machine-step-${id}`);
/** The machine added for `host` (its id is minted per add). */
const added = (host: string) =>
  useMachinesStore.getState().byTeam[TEAM_KEY]?.find((m) => m.declaration?.host === host);
const MINTED = /^203-0-113-10-[0-9a-f]{6}$/;

describe("Add a machine in one step", () => {
  it("shows a runtime prerequisite warning and still connects the machine", async () => {
    const warning = vi.spyOn(toast, "warning");
    const message = "Codex sandbox needs user namespaces; run the explicit setup fix";
    const { host } = freshHost(() => ({
      test_machine_connection: () => ({
        steps: [
          { id: "private-account", status: "ok", detail: "Verified", nextSteps: [] },
          { id: "agentPrerequisite", status: "warn", detail: message, nextSteps: [] },
        ],
        installPlan: null,
        fileViewOnly: false,
      }),
    }));
    host.enrolled = true;
    host.installed = true;
    const close = await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await waitFor(() => expect(warning).toHaveBeenCalledWith(message));
    await waitFor(() => expect(close).toHaveBeenCalled());
    warning.mockRestore();
  });

  it("address → Continue → one fingerprint → Trust & connect → every step → connected → a shell in home", async () => {
    const { calls } = freshHost();
    const onClose = await openAdd();
    expect(screen.queryByLabelText("Session privacy")).toBeNull();
    expect(await screen.findByTestId("machine-signin-summary")).toHaveTextContent(
      "Signs in with your ssh-agent key ed25519",
    );
    await typeAddress("root@203.0.113.10");

    // Click 1.
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    const confirm = await screen.findByTestId("add-machine-fingerprint");
    expect(confirm).toHaveTextContent(
      `First time connecting to 203.0.113.10. Its fingerprint is ${FINGERPRINT}.`,
    );
    // Nothing written before the person trusts the key.
    expect(
      calls
        .map((c) => c.cmd)
        .filter(
          (c) => !["list_ssh_sources", "list_machines"].includes(c) && !c.startsWith("plugin:"),
        ),
    ).toEqual(["test_machine_connection"]);

    // Click 2.
    await userEvent.click(screen.getByTestId("add-machine-trust-connect"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());

    const machine = added("203.0.113.10");
    expect(machine?.status.state).toBe("connected");
    expect(machine?.binding?.autoConnect).toBe(true);
    const sent = calls
      .map((c) => c.cmd)
      .filter((c) =>
        [
          "test_machine_connection",
          "enroll_private_machine_account",
          "install_terminal_server",
          "save_machine",
          "set_machine_auto_connect",
          "connect_machine",
          "terminal_remote_shell_tab",
        ].includes(c),
      );
    expect(sent).toEqual([
      "test_machine_connection",
      "test_machine_connection",
      "enroll_private_machine_account",
      "install_terminal_server",
      "save_machine",
      "connect_machine",
      "terminal_remote_shell_tab",
    ]);
    const tested = calls.filter((c) => c.cmd === "test_machine_connection")[1]?.args;
    expect(tested).toMatchObject({
      machineId: null,
      input: {
        mode: "private-account",
        host: "203.0.113.10",
        username: "root",
        trustedHostKeyFp: FINGERPRINT,
      },
    });
    expect(calls.find((c) => c.cmd === "save_machine")?.args).toMatchObject({
      machineId: expect.stringMatching(MINTED),
      share: { kind: "personal" },
      connect: true,
    });
    // The key shown is the key used: the agent key, through a public key
    // file naming only it.
    expect(tested).toMatchObject({
      input: { signIn: { method: "keyFile", keyPath: "/Users/dev/.ssh/id_ed25519.pub" } },
    });
    expect(calls.find((c) => c.cmd === "terminal_remote_shell_tab")?.args).toMatchObject({
      request: {
        operation: "create",
        machineId: expect.stringMatching(MINTED),
        workspaceId: "ws-1",
        teamKey: TEAM_KEY,
        launch: "shell",
      },
    });
  });

  it("Enter in the address field is Continue", async () => {
    freshHost();
    await openAdd();
    await typeAddress("root@203.0.113.10{Enter}");
    expect(await screen.findByTestId("add-machine-fingerprint")).toBeInTheDocument();
  });

  it("an enrolled host with its server already installed skips both, saying so", async () => {
    let hold: (() => void) | null = null;
    const { host } = freshHost((base) => ({
      connect_machine: (args) =>
        new Promise((resolve) => {
          hold = () => resolve(base.connect_machine!(args));
        }),
    }));
    host.enrolled = true;
    host.installed = true;
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    await waitFor(() => expect(step("connect")).toHaveAttribute("data-status", "running"));
    expect(step("trust")).toHaveTextContent(FINGERPRINT);
    expect(step("signIn")).toHaveTextContent("Sign in as root");
    expect(step("enroll")).toHaveTextContent("already set up");
    expect(step("install")).toHaveTextContent("already installed");
    expect(step("save")).toHaveAttribute("data-status", "done");
    expect(hold).not.toBeNull();
  });

  it("an ~/.ssh/config host resolves to its address, user and port, and names the machine", async () => {
    const { calls } = freshHost();
    await openAdd();
    await screen.findByTestId("machine-signin-summary");
    await waitFor(() => expect(calls.some((c) => c.cmd === "list_ssh_sources")).toBe(true));
    await typeAddress("flaky-vm");
    expect(screen.getByTestId("machine-alias-summary")).toHaveTextContent(
      "dev@flaky-vm.int:2222 from ~/.ssh/config",
    );
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await screen.findByTestId("add-machine-fingerprint");
    expect(calls.find((c) => c.cmd === "test_machine_connection")?.args).toMatchObject({
      input: { host: "flaky-vm.int", port: 2222, username: "dev", label: "flaky-vm" },
    });
  });

  it("a team machine is shared only when asked, and connects on select by default", async () => {
    const { calls } = freshHost();
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-more"));
    expect(screen.getByTestId("machine-auto-connect")).toBeChecked();
    await userEvent.click(screen.getByTestId("machine-share-team"));
    expect(screen.getByTestId("machine-auto-connect")).not.toBeChecked();
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    await waitFor(() => expect(added("203.0.113.10")?.status.state).toBe("connected"));
    expect(calls.find((c) => c.cmd === "save_machine")?.args).toMatchObject({
      share: { kind: "team" },
    });
    expect(added("203.0.113.10")?.binding?.autoConnect).toBe(false);
  });
});

describe("Every failure says the next step", () => {
  it("an unreachable host → check the address / jump host / Retry; jump host reveals its field", async () => {
    freshHost(() => ({
      test_machine_connection: () => {
        throw new Error("Couldn't reach 192.0.2.9:22: Operation timed out");
      },
    }));
    await openAdd();
    await typeAddress("root@192.0.2.9");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    const error = await screen.findByTestId("add-machine-error");
    expect(error).toHaveTextContent("Couldn't reach 192.0.2.9:22: Operation timed out");
    expect(error).toHaveTextContent("firewall allows SSH (port 22)");
    for (const fix of ["editAddress", "useJumpHost", "retry"])
      expect(within(error).getByTestId(`add-machine-fix-${fix}`)).toBeInTheDocument();
    await userEvent.click(within(error).getByTestId("add-machine-fix-useJumpHost"));
    expect(screen.getByLabelText("Jump host")).toHaveFocus();
    expect(screen.queryByTestId("add-machine-error")).toBeNull();
  });

  it("a refused key → copy my public key / choose a key; Choose a key opens the key file", async () => {
    freshHost(() => ({
      test_machine_connection: ({ input }) => {
        if (!(input as MachineDraftInput).trustedHostKeyFp)
          return {
            steps: [
              {
                id: "hostKey",
                status: "needsTrust",
                detail: "",
                nextSteps: [],
                fingerprint: FINGERPRINT,
              },
            ],
            installPlan: null,
            fileViewOnly: false,
          };
        throw new Error("root@203.0.113.10: Permission denied (publickey).");
      },
    }));
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    const error = await screen.findByTestId("add-machine-step-signIn-error");
    expect(error).toHaveTextContent("Permission denied (publickey)");
    expect(error).toHaveTextContent("~/.ssh/authorized_keys");
    expect(within(error).getByTestId("add-machine-fix-copyPublicKey")).toBeInTheDocument();
    await userEvent.click(within(error).getByTestId("add-machine-fix-chooseKey"));
    expect(screen.getByTestId("machine-signin-keyFile")).toHaveAttribute("aria-checked", "true");
    expect(screen.getByLabelText("Key file")).toBeInTheDocument();
  });

  it("a key that doesn't match the team's pin is blocked, with no way to trust it", async () => {
    installMachinesBackend("none");
    await openAdd();
    await typeAddress("dev@mitm.int");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    const error = await screen.findByTestId("add-machine-error");
    expect(error).toHaveTextContent("There's no way around that here");
    expect(within(error).queryByRole("button")).toBeNull();
    expect(screen.queryByTestId("add-machine-trust-connect")).toBeNull();
  });

  it("an interrupted install: Retry picks up there, without enrolling again", async () => {
    let installs = 0;
    const fresh = freshHost(() => ({
      install_terminal_server: () => {
        installs += 1;
        if (installs === 1)
          return { ok: false, code: "interrupted", message: "Upload interrupted at 7.4 of 12 MB" };
        fresh.host.installed = true;
        return { ok: true, version: "0.0.15", logoutWarning: null };
      },
    }));
    const { calls } = fresh;
    const onClose = await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    const error = await screen.findByTestId("add-machine-step-install-error");
    expect(error).toHaveTextContent("Upload interrupted");
    expect(error).toHaveTextContent("Retry picks up where it stopped");
    await userEvent.click(within(error).getByTestId("add-machine-fix-retry"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(installs).toBe(2);
    expect(calls.filter((c) => c.cmd === "enroll_private_machine_account")).toHaveLength(1);
  });

  it("a connect that fails after saving retries the connect, never saving a twin", async () => {
    let attempts = 0;
    const { calls } = freshHost((base) => ({
      connect_machine: (args) => {
        attempts += 1;
        if (attempts === 1) throw new Error("connect timed out after 45s");
        return base.connect_machine!(args);
      },
    }));
    const onClose = await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    const error = await screen.findByTestId("add-machine-step-connect-error");
    expect(error).toHaveTextContent("connect timed out");
    expect(error).toHaveTextContent("It's saved");
    expect(within(error).queryByTestId("add-machine-fix-editAddress")).toBeNull();
    await userEvent.click(within(error).getByTestId("add-machine-fix-retry"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(calls.filter((c) => c.cmd === "save_machine")).toHaveLength(1);
  });
});

describe("The key and the alias settings are the ones used", () => {
  it("picking another agent key signs in with it alone, and Copy copies that key", async () => {
    const copied: string[] = [];
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: (text: string) => (copied.push(text), Promise.resolve()) },
    });
    const { calls } = freshHost(() => ({
      test_machine_connection: ({ input }) => {
        if (!(input as MachineDraftInput).trustedHostKeyFp)
          return {
            steps: [
              {
                id: "hostKey",
                status: "needsTrust",
                detail: "",
                nextSteps: [],
                fingerprint: FINGERPRINT,
              },
            ],
            installPlan: null,
            fileViewOnly: false,
          };
        throw new Error("root@203.0.113.10: Permission denied (publickey).");
      },
    }));
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-more"));
    await userEvent.click(screen.getByTestId("machine-agent-key-dev@work"));
    expect(screen.getByTestId("machine-signin-summary")).toHaveTextContent(
      "Signs in with your ssh-agent key ed25519 SHA256:w0rk…77c2 (dev@work), offering no other key.",
    );
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    const error = await screen.findByTestId("add-machine-step-signIn-error");
    expect(calls.filter((c) => c.cmd === "test_machine_connection")[1]?.args).toMatchObject({
      input: {
        signIn: { method: "keyFile", keyPath: "/Users/dev/.codesense/agent-keys/5c1a.pub" },
      },
    });
    await userEvent.click(within(error).getByTestId("add-machine-fix-copyPublicKey"));
    await waitFor(() => expect(copied).toEqual([expect.stringContaining("dev@work")]));
  });

  it("an ~/.ssh/config host brings its jump host and key file", async () => {
    const { calls } = freshHost();
    await openAdd();
    await waitFor(() => expect(calls.some((c) => c.cmd === "list_ssh_sources")).toBe(true));
    await typeAddress("gpu-behind");
    expect(screen.getByTestId("machine-alias-summary")).toHaveTextContent(
      "ubuntu@192.0.2.12 via bastion.acme.io with ~/.ssh/work_ed25519 from ~/.ssh/config",
    );
    expect(screen.getByTestId("machine-signin-summary")).toHaveTextContent(
      "Signs in with ~/.ssh/work_ed25519.",
    );
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await screen.findByTestId("add-machine-fingerprint");
    expect(calls.find((c) => c.cmd === "test_machine_connection")?.args).toMatchObject({
      input: {
        host: "192.0.2.12",
        username: "ubuntu",
        jumpHost: "bastion.acme.io",
        signIn: { method: "keyFile", keyPath: "~/.ssh/work_ed25519" },
        label: "gpu-behind",
      },
    });
  });

  it("an ~/.ssh/config host with a ProxyCommand connects through it", async () => {
    const { calls } = freshHost();
    await openAdd();
    await waitFor(() => expect(calls.some((c) => c.cmd === "list_ssh_sources")).toBe(true));
    await typeAddress("tunnel-vm");
    expect(screen.getByTestId("machine-alias-summary")).toHaveTextContent(
      "dev@tunnel-vm through cloudflared access ssh --hostname %h from ~/.ssh/config",
    );
    expect(screen.getByTestId("machine-alias-summary")).not.toHaveTextContent("Teammates");
    expect(screen.queryByTestId("machine-proxy-command")).toBeNull();
    await userEvent.click(screen.getByTestId("add-machine-more"));
    await userEvent.click(screen.getByTestId("machine-share-team"));
    expect(screen.getByTestId("machine-alias-summary")).toHaveTextContent(
      "from ~/.ssh/config. Teammates connect through their own ~/.ssh/config entry for tunnel-vm.",
    );
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await screen.findByTestId("add-machine-fingerprint");
    // The alias itself, so ssh matches its Host block and runs the ProxyCommand.
    expect(calls.find((c) => c.cmd === "test_machine_connection")?.args).toMatchObject({
      input: { host: "tunnel-vm", username: "dev", label: "tunnel-vm" },
    });
  });
});

describe("A host key that changes mid-checklist", () => {
  it("offers Check again, which forgets the trusted key and shows the one presented now", async () => {
    let presented = FINGERPRINT;
    const { calls } = freshHost(() => ({
      test_machine_connection: ({ input }) => {
        const fp = (input as MachineDraftInput).trustedHostKeyFp;
        if (!fp || fp !== presented)
          return {
            steps: [
              {
                id: "hostKey",
                status: "needsTrust",
                detail: fp
                  ? `203.0.113.10 now presents a different key than the ${fp} you trusted. Check it before trusting it.`
                  : "",
                nextSteps: [],
                fingerprint: presented,
              },
            ],
            installPlan: null,
            fileViewOnly: false,
          };
        return {
          steps: [{ id: "private-account", status: "ok", detail: "", nextSteps: [] }],
          installPlan: null,
          fileViewOnly: false,
        };
      },
    }));
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await screen.findByTestId("add-machine-fingerprint");
    presented = "SHA256:rebuiltDropletKey";
    await userEvent.click(screen.getByTestId("add-machine-trust-connect"));
    const error = await screen.findByTestId("add-machine-step-signIn-error");
    expect(error).toHaveTextContent("different host key than the one you trusted");
    expect(within(error).getByTestId("add-machine-fix-editAddress")).toBeInTheDocument();
    await userEvent.click(within(error).getByTestId("add-machine-fix-checkAgain"));
    expect(await screen.findByTestId("add-machine-fingerprint")).toHaveTextContent(
      "Its fingerprint is SHA256:rebuiltDropletKey.",
    );
    const last = calls.filter((c) => c.cmd === "test_machine_connection").at(-1)?.args;
    expect(last).toMatchObject({ input: { trustedHostKeyFp: null } });
  });
});

describe("Saving", () => {
  it("a save that wrote and then failed is retried under the same id: no twin", async () => {
    let saves = 0;
    const { calls } = freshHost((base) => ({
      save_machine: (args) => {
        saves += 1;
        const saved = base.save_machine!(args);
        if (saves === 1) throw new Error("private account unavailable; check account enrollment");
        return saved;
      },
    }));
    const onClose = await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    const error = await screen.findByTestId("add-machine-step-save-error");
    await userEvent.click(within(error).getByTestId("add-machine-fix-retry"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    const ids = calls.filter((c) => c.cmd === "save_machine").map((c) => c.args.machineId);
    expect(ids).toHaveLength(2);
    expect(ids[0]).toMatch(MINTED);
    expect(ids[1]).toBe(ids[0]);
    expect(
      useMachinesStore
        .getState()
        .byTeam[TEAM_KEY]?.filter((m) => m.declaration?.host === "203.0.113.10"),
    ).toHaveLength(1);
  });

  it("a connect-at-launch setting that can't be kept says so", async () => {
    const shown: string[] = [];
    const { toast } = await import("sonner");
    const spy = vi.spyOn(toast, "error").mockImplementation((m) => (shown.push(String(m)), ""));
    freshHost(() => ({
      set_machine_auto_connect: () => {
        throw new Error("Set up this private machine first");
      },
    }));
    const onClose = await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-more"));
    await userEvent.click(screen.getByTestId("machine-auto-connect"));
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(shown).toEqual([
      expect.stringContaining("Couldn't turn off connecting 203.0.113.10 when CodeSense opens"),
    ]);
    spy.mockRestore();
  });
});

describe("Closing, passphrases and drafts", () => {
  it("closing before it's saved keeps a Needs setup row; Cancel and an empty form keep nothing", async () => {
    const { calls } = freshHost();
    let onClose = await openAdd();
    await userEvent.click(screen.getByLabelText("Close add-machine panel"));
    expect(onClose).toHaveBeenCalled();
    cleanup();

    onClose = await openAdd();
    await typeAddress("root@half-done.int");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
    expect(calls.some((c) => c.cmd === "save_machine")).toBe(false);
    cleanup();

    // Typed but never reached: nothing worth keeping.
    onClose = await openAdd();
    await typeAddress("root@half-done.int");
    await userEvent.click(screen.getByLabelText("Close add-machine panel"));
    expect(onClose).toHaveBeenCalled();
    expect(calls.some((c) => c.cmd === "save_machine")).toBe(false);
    cleanup();

    onClose = await openAdd();
    await typeAddress("root@half-done.int");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await screen.findByTestId("add-machine-fingerprint");
    await userEvent.click(screen.getByLabelText("Close add-machine panel"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(added("half-done.int")?.status.state).toBe("needsCredential");
    expect(calls.find((c) => c.cmd === "save_machine")?.args).toMatchObject({
      share: { kind: "personal" },
      connect: false,
    });
  });

  it("a passphrase never enters the panel; its machine connects only when picked", async () => {
    const { calls } = freshHost();
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-more"));
    await userEvent.click(screen.getByTestId("machine-signin-keychainPassphrase"));
    expect(document.querySelector('input[type="password"]')).toBeNull();
    expect(screen.getByTestId("add-machine-continue")).toBeDisabled();
    expect(screen.getByTestId("machine-auto-connect")).toBeDisabled();
    expect(screen.getByText(/connects when you pick it, never at launch/)).toBeInTheDocument();

    await userEvent.click(screen.getByTestId("machine-save-passphrase"));
    await screen.findByTestId("machine-passphrase-saved");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.click(await screen.findByTestId("add-machine-trust-connect"));
    await waitFor(() => expect(added("203.0.113.10")?.status.state).toBe("connected"));
    expect(calls.find((c) => c.cmd === "save_machine")?.args).toMatchObject({
      input: {
        signIn: {
          method: "keychainPassphrase",
          credentialRef: { provider: "ssh", account: "mock-passphrase-1" },
        },
      },
    });
    for (const call of calls) expect(JSON.stringify(call.args)).not.toContain('"passphrase"');
    expect(calls.find((c) => c.cmd === "set_machine_auto_connect")?.args).toMatchObject({
      autoConnect: false,
    });

    // Set up again reopens on the saved key and kept passphrase.
    cleanup();
    render(
      <MachineReserve
        panel={{ mode: "setUp", machineId: added("203.0.113.10")!.machineId }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    await screen.findByText("Edit 203.0.113.10");
    expect(screen.getByTestId("machine-signin-keychainPassphrase")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByTestId("machine-passphrase-saved")).toBeInTheDocument();
  });

  it("a key file needs a path before Continue", async () => {
    freshHost();
    await openAdd();
    await typeAddress("root@203.0.113.10");
    await userEvent.click(screen.getByTestId("add-machine-more"));
    await userEvent.click(screen.getByTestId("machine-signin-keyFile"));
    await userEvent.clear(screen.getByLabelText("Key file"));
    expect(screen.getByTestId("add-machine-continue")).toBeDisabled();
  });

  it("a host key check still running when the address changes can't ask to trust it", async () => {
    let release: () => void = () => {};
    freshHost(() => ({
      test_machine_connection: () =>
        new Promise((resolve) => {
          release = () =>
            resolve({
              steps: [
                {
                  id: "hostKey",
                  status: "needsTrust",
                  detail: "",
                  nextSteps: [],
                  fingerprint: FINGERPRINT,
                },
              ],
              installPlan: null,
              fileViewOnly: false,
            });
        }),
    }));
    await openAdd();
    await typeAddress("root@old-host.int");
    await userEvent.click(screen.getByTestId("add-machine-continue"));
    await userEvent.type(screen.getByLabelText("Address"), "x");
    release();
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByTestId("add-machine-fingerprint")).toBeNull();
  });
});

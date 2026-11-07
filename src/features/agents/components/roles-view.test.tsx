// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { agentReadRoles, rolesList, roleSave } from "@/tauri/commands";
import { BUILTIN_AVATAR_PACKS, BUILTIN_FACE_SOURCE } from "@/shared/lib/avatar-packs";
import { useOrgStore } from "@/shared/stores/org-store";
import { useRoleConfigsStore } from "@/shared/hooks/use-role-configs";
import { QueueRole } from "@/shared/components/queue-role";
import { useRoleEditorStore } from "../stores/role-editor-store";
import { useAgentsStore } from "../stores/agents-store";
import { RoleLeaveGuard, RolesView } from "./roles-view";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  rolesList: vi.fn(),
  roleSave: vi.fn(),
  agentReadRoles: vi.fn(async () => ({
    roles: [
      {
        role: "orphan",
        registrations: [],
        counts: { pending: 2, claimed: 0, accepted: 0, acknowledged: 0 },
      },
    ],
  })),
}));
/** The team's running agents as the agents list draws them, by handle; a test sets what it has. */
const identities = vi.hoisted(() => ({ current: [] as unknown[] }));
vi.mock("@/features/viewer/hooks/use-team-agent-identities", () => ({
  useTeamAgentIdentities: () => identities.current,
}));
vi.mock("@/shared/hooks/use-avatar-packs", () => ({
  useTeamAvatarPacks: () => () => ({ packs: BUILTIN_AVATAR_PACKS, faces: BUILTIN_FACE_SOURCE }),
}));
const config = { id: "reviewer", name: "Reviewer", icon: null, defaultHandler: "deleted-agent" };
beforeEach(() => {
  identities.current = [];
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useRoleEditorStore.setState({ teamKey: null, role: null, dirty: false, pendingLeave: null });
  useRoleConfigsStore.setState({ byTeam: { "acme:backend": [config] }, errors: {} });
  useAgentsStore.setState({ rows: [] });
  vi.mocked(rolesList).mockResolvedValue([config]);
  vi.mocked(roleSave).mockImplementation(async (_team, next) => {
    vi.mocked(rolesList).mockResolvedValue([next]);
    return next;
  });
});
afterEach(cleanup);
describe("team Roles", () => {
  it("shows unconfigured mail and missing handlers, and saves icon choice/reset without changing queue identity", async () => {
    render(<RolesView />);
    expect(await screen.findAllByText("Handler not found", { exact: false })).toBeTruthy();
    expect(await screen.findByText("orphan")).toBeTruthy();
    const editor = screen.getByTestId("role-editor");
    fireEvent.click(within(editor).getByRole("radio", { name: "Knight" }));
    fireEvent.click(within(editor).getByRole("button", { name: "Save role" }));
    await waitFor(() =>
      expect(roleSave).toHaveBeenCalledWith("acme:backend", {
        ...config,
        icon: "builtin-chess:knight",
      }),
    );
    await waitFor(() =>
      expect(within(editor).getByRole("button", { name: "Revert" })).not.toBeEnabled(),
    );
    fireEvent.click(within(editor).getByTestId("launch-face-auto"));
    fireEvent.click(within(editor).getByRole("button", { name: "Save role" }));
    await waitFor(() => expect(roleSave).toHaveBeenLastCalledWith("acme:backend", config));
  });
  it("keeps the automatically selected draft until a team switch is confirmed", async () => {
    render(
      <>
        <RolesView />
        <RoleLeaveGuard />
      </>,
    );
    const name = screen.getByRole("textbox", { name: /^Name/ });
    fireEvent.change(name, { target: { value: "Unsaved review" } });
    act(() => useOrgStore.setState({ activeTeamName: "design" }));
    expect(name).toHaveValue("Unsaved review");
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("textbox", { name: /^Name/ })).toHaveValue("Unsaved review");
    expect(useRoleEditorStore.getState().dirty).toBe(true);
    act(() => useOrgStore.setState({ activeTeamName: "mobile" }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Discard" }),
    );
    await waitFor(() => expect(screen.getByTestId("role-editor")).toHaveTextContent("acme:mobile"));
    expect(screen.getByRole("textbox", { name: /^Name/ })).toHaveValue("Reviewer");
  });
  it("selecting the displayed role keeps the draft and the next leave warning", () => {
    render(
      <>
        <RolesView />
        <RoleLeaveGuard />
      </>,
    );
    fireEvent.change(screen.getByRole("textbox", { name: /^Name/ }), {
      target: { value: "Unsaved review" },
    });
    fireEvent.click(screen.getByRole("button", { name: /Queue role: Reviewer/ }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(useRoleEditorStore.getState().dirty).toBe(true);
    act(() => useRoleEditorStore.getState().requestLeave(() => {}));
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByDisplayValue("Unsaved review")).toBeTruthy();
  });
  it("keeps an unconfigured queue with acknowledged mail and no live holders discoverable", async () => {
    vi.mocked(agentReadRoles).mockResolvedValueOnce({
      roles: [
        {
          role: "completed",
          registrations: [],
          roleName: null,
          deliveries: [],
          deliveriesTruncated: false,
          counts: { pending: 0, claimed: 0, accepted: 0, acknowledged: 2 },
        },
      ],
    } as Awaited<ReturnType<typeof agentReadRoles>>);
    render(<RolesView />);
    expect(await screen.findByText("completed")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Queue role: completed/ }));
    expect(screen.getByTestId("role-editor")).toHaveTextContent("queue id (completed)");
  });
  it("draws the same configured icon and accessible queue id on queue and holder surfaces", () => {
    useRoleConfigsStore.setState({
      byTeam: { "acme:backend": [{ ...config, icon: "builtin-chess:rook" }] },
    });
    render(
      <>
        <div data-testid="queue">
          <QueueRole role="reviewer" teamKey="acme:backend" />
        </div>
        <div data-testid="holder">
          <QueueRole role="reviewer" teamKey="acme:backend" />
        </div>
      </>,
    );
    const q = screen.getByTestId("queue");
    const h = screen.getByTestId("holder");
    expect(q.querySelector("img")?.getAttribute("src")).toBe(
      h.querySelector("img")?.getAttribute("src"),
    );
    expect(within(q).getByLabelText("Queue role: Reviewer (role:reviewer)")).toBeTruthy();
    expect(within(h).getByLabelText("Queue role: Reviewer (role:reviewer)")).toBeTruthy();
  });
});

describe("Roles: who holds a queue and what waits, in plain words", () => {
  const holder = (id: string, name: string) => ({
    instanceId: id,
    instanceName: name,
    generation: 1,
    registeredAtMs: 0,
    lastRenewedAtMs: 0,
    expiresAtMs: 1,
    live: true,
  });

  it("shows the holders' faces in the list, and counts with the right plurals", async () => {
    vi.mocked(agentReadRoles).mockResolvedValue({
      roles: [
        {
          role: "reviewer",
          registrations: [holder("h1", "Mira"), holder("h2", "Bram")],
          counts: { pending: 1, claimed: 0, accepted: 0, acknowledged: 0 },
        },
      ],
    } as never);
    render(<RolesView />);
    const row = await screen.findByRole("button", { name: /Queue role: Reviewer/ });
    await waitFor(() => expect(within(row).getAllByTestId("role-holder-face")).toHaveLength(2));
    expect(row.textContent).toContain("2 hold it now");
    expect(row.textContent).toContain("1 message waiting");
    expect(row.textContent).not.toMatch(/holders/i);
    const editor = screen.getByTestId("role-editor");
    expect(editor.textContent).toContain("2 hold it now");
    expect(editor.textContent).toContain("1 message waiting");
  });

  it("says one holder and several messages in the singular and plural", async () => {
    vi.mocked(agentReadRoles).mockResolvedValue({
      roles: [
        {
          role: "reviewer",
          registrations: [holder("h1", "Mira")],
          counts: { pending: 3, claimed: 0, accepted: 0, acknowledged: 0 },
        },
      ],
    } as never);
    render(<RolesView />);
    const row = await screen.findByRole("button", { name: /Queue role: Reviewer/ });
    await waitFor(() => expect(row.textContent).toContain("1 holds it now"));
    expect(row.textContent).toContain("3 messages waiting");
  });

  it("titles the role in the chrome type size, without a monospace id line", async () => {
    render(<RolesView />);
    const editor = await screen.findByTestId("role-editor");
    const title = within(editor).getByRole("heading", { level: 2 });
    expect(title.className).toContain("text-lg");
    expect(editor.querySelector(".font-mono")).toBeNull();
  });

  it("keeps the icon cells compact", async () => {
    render(<RolesView />);
    const editor = await screen.findByTestId("role-editor");
    expect(within(editor).getByRole("radio", { name: "Knight" }).className).toContain(
      "ls-face-compact",
    );
  });
});

describe("Roles: a holder wears the face its session shows elsewhere", () => {
  it("draws the session's own face, not a name-based fallback", async () => {
    vi.mocked(agentReadRoles).mockResolvedValue({
      roles: [
        {
          role: "reviewer",
          registrations: [
            {
              instanceId: "h1",
              instanceName: "Mira",
              generation: 1,
              registeredAtMs: 0,
              lastRenewedAtMs: 0,
              expiresAtMs: 1,
              live: true,
            },
          ],
          counts: { pending: 0, claimed: 0, accepted: 0, acknowledged: 0 },
        },
      ],
    } as never);
    identities.current = [
      {
        row: { handle: "h1" },
        face: { kind: "image", ref: "pack:chosen", src: "chosen.png" },
      },
    ];
    render(<RolesView />);
    const row = await screen.findByRole("button", { name: /Queue role: Reviewer/ });
    const face = await within(row).findByTestId("role-holder-face");
    expect(face.querySelector("[data-face]")?.getAttribute("data-face")).toBe("pack:chosen");
  });
});

// SPDX-License-Identifier: MIT
import type { ComponentType, CSSProperties, ReactNode } from "react";

import { toast } from "sonner";

import {
  ContextMenuItem,
  ContextMenuShortcut,
  ContextMenuLabel,
  ContextMenuRadioGroup,
  ContextMenuRadioItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
} from "@/components/ui/context-menu";
import {
  DropdownMenuItem,
  DropdownMenuShortcut,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@/components/ui/dropdown-menu";
import { AgentAvatar, AgentFaceView } from "@/features/viewer/components/agent-identity";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import {
  AGENT_ROLE_CHOICES,
  NO_MANAGER,
  type AgentIdentityChoice,
  type AgentIdentityPatch,
} from "@/shared/lib/agent-identity-choice";
import { BUILTIN_FUNNY_PACK } from "@/shared/lib/avatar-packs";
import { userFacingErrorMessage } from "@/shared/lib/error-message";
import type { AvatarPacks } from "@/shared/hooks/use-avatar-packs";

/**
 * The menu parts the Role and Face choices are drawn with: the agents list's right-click
 * menu and the tile tab's menu are different Radix menus with the same shape.
 */
export interface IdentityMenuKit {
  Shortcut: ComponentType<{ children?: ReactNode; "aria-hidden"?: boolean }>;
  Sub: ComponentType<{ children?: ReactNode }>;
  SubTrigger: ComponentType<{
    children?: ReactNode;
    className?: string;
    "data-checked"?: boolean;
  }>;
  SubContent: ComponentType<{ children?: ReactNode; className?: string }>;
  RadioGroup: ComponentType<{
    children?: ReactNode;
    value?: string;
    onValueChange?: (value: string) => void;
  }>;
  RadioItem: ComponentType<{
    children?: ReactNode;
    value: string;
    className?: string;
    style?: CSSProperties;
  }>;
  Separator: ComponentType<object>;
  Label: ComponentType<{ children?: ReactNode; className?: string }>;
  Item: ComponentType<{
    children?: ReactNode;
    onSelect?: () => void;
    disabled?: boolean;
    title?: string;
    variant?: "default" | "destructive";
    "aria-keyshortcuts"?: string;
  }>;
}

export const CONTEXT_MENU_KIT: IdentityMenuKit = {
  Sub: ContextMenuSub,
  SubTrigger: ContextMenuSubTrigger,
  SubContent: ContextMenuSubContent,
  RadioGroup: ContextMenuRadioGroup,
  RadioItem: ContextMenuRadioItem,
  Separator: ContextMenuSeparator,
  Label: ContextMenuLabel,
  Item: ContextMenuItem,
  Shortcut: ContextMenuShortcut,
};

export const DROPDOWN_MENU_KIT: IdentityMenuKit = {
  Sub: DropdownMenuSub,
  SubTrigger: DropdownMenuSubTrigger,
  SubContent: DropdownMenuSubContent,
  RadioGroup: DropdownMenuRadioGroup,
  RadioItem: DropdownMenuRadioItem,
  Separator: DropdownMenuSeparator,
  Label: DropdownMenuLabel,
  Item: DropdownMenuItem,
  Shortcut: DropdownMenuShortcut,
};

const AUTO = "auto";

/** What a person can make one agent look like, and how to change it. Display only. */
export interface AgentLook {
  choice: AgentIdentityChoice | undefined;
  avatars: AvatarPacks;
  onChoose: (patch: AgentIdentityPatch) => void;
}

/** The Role choices: Auto, then each role. `onDone` runs after a pick. */
export function RoleChoices({
  kit,
  choice,
  onChoose,
  onDone,
}: {
  kit: IdentityMenuKit;
  choice: AgentIdentityChoice | undefined;
  onChoose: (patch: AgentIdentityPatch) => void;
  onDone?: () => void;
}): React.JSX.Element {
  const { RadioGroup, RadioItem } = kit;
  return (
    <RadioGroup
      value={choice?.role ?? AUTO}
      onValueChange={(v) => {
        onChoose({ role: v === AUTO ? null : AGENT_ROLE_CHOICES.find((c) => c.id === v)?.id });
        onDone?.();
      }}
    >
      <RadioItem value={AUTO}>Auto</RadioItem>
      {AGENT_ROLE_CHOICES.map((c) => (
        <RadioItem key={c.id} value={c.id}>
          {c.label}
        </RadioItem>
      ))}
    </RadioGroup>
  );
}

/** The Face choices: Auto (the face the agent would have by default),
 *  every pack's faces, a way to share one of your own packs with the team, and which
 *  pack the team deals faces from. `onDone` runs after a pick. */
export function FaceChoices({
  kit,
  choice,
  avatars,
  onChoose,
  onDone,
}: {
  kit: IdentityMenuKit;
  choice: AgentIdentityChoice | undefined;
  avatars: AvatarPacks;
  onChoose: (patch: AgentIdentityPatch) => void;
  onDone?: () => void;
}): React.JSX.Element {
  const { Sub, SubTrigger, SubContent, RadioGroup, RadioItem, Separator, Label, Item } = kit;
  const face = choice?.face ?? AUTO;
  const teamDefault = avatars.defaultPack ?? BUILTIN_FUNNY_PACK;
  const pick = (patch: AgentIdentityPatch) => {
    onChoose(patch);
    onDone?.();
  };
  const share = (pack: AvatarPacks["packs"][number]) => {
    avatars.sharePack(pack.id).then(
      () => toast.success(`Shared ${pack.name} with the team`),
      (e: unknown) => toast.error(`Could not share ${pack.name}: ${userFacingErrorMessage(e)}`),
    );
    onDone?.();
  };
  const setDefault = (pack: string) => {
    avatars
      .setDefaultPack(pack === BUILTIN_FUNNY_PACK ? null : pack)
      .catch((e) => toast.error(`Could not set the team's default pack: ${String(e)}`));
    onDone?.();
  };
  return (
    <>
      <RadioGroup value={face} onValueChange={() => pick({ face: null })}>
        <RadioItem value={AUTO}>Auto</RadioItem>
      </RadioGroup>
      <Separator />
      {avatars.packs.map((pack) => (
        <Sub key={pack.id}>
          <SubTrigger data-checked={face.startsWith(`${pack.id}:`) || undefined}>
            {pack.name}
            {pack.scope === "personal" && (
              <span className="ml-1 text-[var(--ag-faint)]">· only you</span>
            )}
          </SubTrigger>
          <SubContent className="max-h-80 overflow-y-auto">
            <RadioGroup value={face} onValueChange={(v) => pick({ face: v })}>
              {pack.faces.map((f) => (
                <RadioItem key={f.ref} value={f.ref} className="gap-2">
                  <AgentFaceView
                    face={{ kind: "image", ref: f.ref, src: f.src }}
                    size={16}
                    className="cs-agent-identity"
                  />
                  {f.label ?? f.id}
                </RadioItem>
              ))}
            </RadioGroup>
            {pack.scope === "personal" && avatars.canShare && (
              <>
                <Separator />
                <Item onSelect={() => share(pack)}>Share with team</Item>
              </>
            )}
            {pack.personal_copy && (
              <>
                <Separator />
                <Item disabled>Shared with team ✓</Item>
              </>
            )}
          </SubContent>
        </Sub>
      ))}
      {avatars.canShare && (
        <>
          <Separator />
          <Sub>
            <SubTrigger>Team default pack</SubTrigger>
            <SubContent>
              <Label className="max-w-56 text-[11px] font-normal text-[var(--ag-faint)]">
                Agents nobody gave a face get one from this pack, for the whole team.
              </Label>
              <RadioGroup value={teamDefault} onValueChange={setDefault}>
                {avatars.packs
                  .filter((pack) => pack.scope !== "personal")
                  .map((pack) => (
                    <RadioItem key={pack.id} value={pack.id}>
                      {pack.name}
                    </RadioItem>
                  ))}
              </RadioGroup>
            </SubContent>
          </Sub>
        </>
      )}
    </>
  );
}

const AS_LAUNCHED = "auto";

/** The Manager choices: show an agent under another running agent, at the top level, or
 *  (As launched) under whoever launched it. Display only: the agent keeps working as it was.
 *  `onDone` runs after a pick. */
export function ManagerChoices({
  kit,
  choice,
  launchedBy,
  options,
  onChoose,
  onDone,
}: {
  kit: IdentityMenuKit;
  choice: AgentIdentityChoice | undefined;
  launchedBy: string | null;
  options: readonly TeamAgentIdentity[];
  onChoose: (manager: string | null) => void;
  onDone?: () => void;
}): React.JSX.Element {
  const { RadioGroup, RadioItem, Separator } = kit;
  const launcher = options.find(
    (o) =>
      o.row.handle === launchedBy || (o.row.sessionId !== null && o.row.sessionId === launchedBy),
  );
  return (
    <RadioGroup
      value={choice?.manager ?? AS_LAUNCHED}
      onValueChange={(v) => {
        onChoose(v === AS_LAUNCHED ? null : v);
        onDone?.();
      }}
    >
      <RadioItem value={AS_LAUNCHED}>
        <span className="flex min-w-0 flex-col">
          <span>As launched</span>
          <span className="truncate text-[11px] text-muted-foreground">
            {launcher ? `Under ${launcher.label}` : "At the top level"}
          </span>
        </span>
      </RadioItem>
      <RadioItem value={NO_MANAGER}>Top level</RadioItem>
      {options.length > 0 && <Separator />}
      {options.map((o) => (
        <RadioItem
          key={o.row.handle}
          value={o.row.handle}
          className="gap-2"
          style={{ paddingLeft: 32 + Math.min(o.depth, 4) * 12 }}
        >
          <AgentAvatar
            face={o.face}
            role={o.role}
            pieceTone={null}
            size={18}
            className="cs-agent-identity"
            tag={o.tag}
          />
          <span className="truncate">{o.label}</span>
        </RadioItem>
      ))}
    </RadioGroup>
  );
}

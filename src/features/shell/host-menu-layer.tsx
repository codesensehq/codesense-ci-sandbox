// SPDX-License-Identifier: MIT
import { appPopoverBounds } from "@/shared/lib/popover-bounds";
import { StageRailShape } from "@/shared/components/stage-rail";
import { StatusMark } from "@/shared/components/status-mark";
import { FileKindIcon } from "@/shared/lib/icons/file-kind";
import { TextEditMenu } from "@/components/ui/text-edit-menu";
import { buildInboxMenu, type InboxMenu } from "./host-inbox-menu";
import { NativeTextMenu } from "./native-text-menu";
import { openNativeMenu } from "./native-menu-openers";
import { useMenuOwnership } from "@/shared/hooks/use-menu-ownership";
import { createElement, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuCheckboxItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
} from "@/components/ui/dropdown-menu";
import { RoleChoices, DROPDOWN_MENU_KIT } from "@/features/viewer/components/agent-identity-menus";
import { paneIcon } from "./right-pane/panes/pane-chrome";
import {
  BUILTIN_MENU_ITEMS,
  DASHBOARD_MENU_ITEMS,
  dashboardMenuAction,
  isDashboardMenuId,
  builtinMenuId,
  builtinDisabledReason,
  runBuiltinMenuItem,
  validateMenuItems,
} from "./builtin-menu-items";
import {
  HostMenuContext,
  hostAnchorRect,
  type HostMenuItem,
  type HostMenuGlyph,
  type HostMenuRequest,
  type HostMenuResult,
} from "./host-menu-context";
import {
  HostAgentInboxChat,
  HostAgentPopover,
  HostAgentRowMenu,
  useHostAgentMenu,
} from "./host-agent-menu";

function MenuGlyph({ glyph }: { glyph: HostMenuGlyph }) {
  if (typeof glyph === "string") {
    return createElement(paneIcon(glyph), { "aria-hidden": true });
  }
  switch (glyph.kind) {
    case "stageRail":
      return <StageRailShape role={glyph.role} ready={glyph.ready} />;
    case "statusMark":
      return glyph.still ? (
        <span className="cs-mark-still inline-flex">
          <StatusMark mark={glyph.mark} size={glyph.size ?? 8} />
        </span>
      ) : (
        <StatusMark mark={glyph.mark} size={glyph.size ?? 7} />
      );
    case "fileKindIcon":
      return <FileKindIcon kind={glyph.fileKind} />;
  }
}
function MenuLabel({ item }: { item: HostMenuItem }) {
  return (
    <>
      {item.icon && typeof item.icon !== "string" ? (
        // Native glyphs keep their intrinsic SVG size, as the native filter menu does.
        <span className="contents [&_svg]:size-auto!">
          <MenuGlyph glyph={item.icon} />
        </span>
      ) : (
        item.icon && <MenuGlyph glyph={item.icon} />
      )}
      {item.label}
      {item.count !== undefined && (
        <span className="ml-auto text-[11.5px] tabular-nums text-muted-foreground">
          {item.count}
        </span>
      )}
    </>
  );
}

/** The popovers a page can ask the host for: the quick view and the inbox chat. */
const POPOVER_KINDS = new Set(["agent-quick-send", "agent-inbox-chat"]);

type ActiveRequest = HostMenuRequest & {
  id: number;
  settled: boolean;
  superseded?: boolean;
  outcome?: {
    result: HostMenuResult;
    error?: unknown;
    enteredFrame?: boolean;
    afterClose?: () => void;
  };
  resolve: (result: HostMenuResult) => void;
  reject: (error: unknown) => void;
};
type Finish = (
  request: ActiveRequest,
  result: HostMenuResult,
  error?: unknown,
  enteredFrame?: boolean,
  superseded?: boolean,
  afterClose?: () => void,
) => void;
function focusReturn(request: HostMenuRequest) {
  if (!request.frame.isConnected || request.signal?.aborted) return;
  if (request.returnFocus) {
    request.returnFocus();
    return;
  }
  request.frame.focus();
  request.frame.contentWindow?.postMessage(
    { type: "codesense:focus-return", anchorId: request.anchorId },
    request.origin,
  );
}
export function HostMenuLayer({ children }: { children: ReactNode }) {
  const [active, setActive] = useState<ActiveRequest | null>(null);
  const current = useRef<ActiveRequest | null>(null);
  const nextId = useRef(0);
  const settle = useCallback((request: ActiveRequest) => {
    if (request.settled) return;
    request.settled = true;
    const outcome: NonNullable<ActiveRequest["outcome"]> = request.outcome ?? {
      result: { closed: true },
    };
    // A click into a frame dismisses the overlay and keeps focus on what was clicked.
    if (
      !outcome.afterClose &&
      !request.superseded &&
      (!outcome.enteredFrame || !(document.activeElement instanceof HTMLIFrameElement))
    )
      focusReturn(request);
    if (outcome.error) request.reject(outcome.error);
    else {
      request.resolve(outcome.result);
      outcome.afterClose?.();
    }
  }, []);
  const finish = useCallback<Finish>(
    (request, result, error, enteredFrame, superseded, afterClose) => {
      if (current.current !== request || request.outcome) return;
      request.superseded = superseded;
      request.outcome = { result, error, enteredFrame, afterClose };
      setActive(null);
      requestAnimationFrame(() => {
        settle(request);
        if (current.current === request) current.current = null;
      });
    },
    [settle],
  );
  const show = useCallback(
    async (request: HostMenuRequest) => {
      if (request.type === "popover" && !POPOVER_KINDS.has(request.kind ?? ""))
        throw new Error(`unknown popover "${request.kind}"`);
      validateMenuItems(request.items ?? []);
      const previous = current.current;
      return new Promise<HostMenuResult>((resolve, reject) => {
        const next = { ...request, resolve, reject, id: ++nextId.current, settled: false };
        current.current = next;
        if (previous) {
          previous.superseded = true;
          setActive(null);
          requestAnimationFrame(() => {
            settle(previous);
            requestAnimationFrame(() => {
              if (current.current === next) setActive(next);
            });
          });
        } else setActive(next);
      });
    },
    [settle],
  );
  useEffect(
    () => () => {
      current.current?.resolve({ closed: true });
      current.current = null;
    },
    [],
  );
  return (
    <HostMenuContext.Provider value={show}>
      {children}
      <NativeTextMenu />
      {active && <RequestedMenu key={active.id} request={active} finish={finish} />}
    </HostMenuContext.Provider>
  );
}
function openFileDialog(
  request: HostMenuRequest,
  id: "builtin:rename" | "builtin:delete" | "builtin:new-file",
) {
  openNativeMenu({
    action: id === "builtin:rename" ? "rename" : id === "builtin:delete" ? "delete" : "new-file",
    row: request.row ?? { path: request.contentRoot, kind: "folder" },
    teamKey: request.teamKey ?? "",
    contentRoot: request.contentRoot ?? "",
    anchorRect: hostAnchorRect(request),
    owner: request,
    onClose: () => {},
  });
}

function RequestedMenu({ request, finish }: { request: ActiveRequest; finish: Finish }) {
  const [open, setOpen] = useState(!request.builtinAction);
  const [presentation, setPresentation] = useState(request.type);
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const busy = useRef(false);
  const [inboxMenu, setInboxMenu] = useState<InboxMenu | null | undefined>();
  useEffect(() => {
    if (request.type !== "inbox") return;
    let cancelled = false;
    void buildInboxMenu(request)
      .catch(() => null)
      .then((menu) => {
        if (!cancelled) setInboxMenu(menu);
      });
    return () => {
      cancelled = true;
    };
  }, [request]);
  const target = useHostAgentMenu(
    request.row?.sessionId ?? request.sessionId,
    request.row?.agentId,
  );
  const [nativeOpened, setNativeOpened] = useState(request.type === "native");
  useEffect(() => {
    if (request.type !== "native") return;
    const rect = hostAnchorRect(request);
    const row = { ...(request.row ?? {}) };
    if (
      request.nativeSurface === "nav-status" &&
      typeof row.path === "string" &&
      !row.path.startsWith("/")
    ) {
      row.path = `${request.contentRoot ?? ""}/${row.path}`;
    }
    const closeNative = openNativeMenu({
      row,
      surface: request.nativeSurface,
      teamKey: request.teamKey ?? "",
      contentRoot: request.contentRoot ?? "",
      anchorRect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
      owner: request,
      onClose: () => finish(request, { closed: true }),
    });
    if (!closeNative) queueMicrotask(() => setNativeOpened(false));
    return () => closeNative?.();
  }, [request, finish]);
  useEffect(() => {
    if (!request.builtinAction) return;
    openFileDialog(request, request.builtinAction);
    finish(request, { itemId: request.builtinAction, row: request.row, builtin: true });
  }, [request, finish]);
  useEffect(() => {
    const abort = () => finish(request, { closed: true });
    request.signal?.addEventListener("abort", abort, { once: true });
    if (request.signal?.aborted) abort();
    return () => request.signal?.removeEventListener("abort", abort);
  }, [request, finish]);
  useMenuOwnership(request, true, () => {
    finish(request, { closed: true }, undefined, false, true);
  });
  const close = () => finish(request, { closed: true });
  useEffect(() => {
    const onFrameEntry = () => {
      if (presentation === "popover" || !busy.current)
        finish(request, { closed: true }, undefined, true);
    };
    window.addEventListener("blur", onFrameEntry);
    return () => window.removeEventListener("blur", onFrameEntry);
  }, [request, finish, presentation]);
  const choose = async (itemId: string) => {
    if (!inboxMenu) {
      finish(request, { itemId, row: request.row });
      return;
    }
    busy.current = true;
    setOpen(false);
    try {
      const afterClose = await inboxMenu.choose(itemId);
      finish(
        request,
        { itemId, row: request.row },
        undefined,
        false,
        false,
        afterClose || undefined,
      );
    } catch (error) {
      finish(request, { closed: true }, error);
    }
  };
  const chooseBuiltin = async (
    id: Exclude<keyof typeof BUILTIN_MENU_ITEMS, "builtin:set-role">,
  ) => {
    const dashboardAction = dashboardMenuAction(id, request.dashboard);
    if (isDashboardMenuId(id)) {
      if (!dashboardAction) return;
      busy.current = true;
      setOpen(false);
      finish(request, { itemId: id, row: request.row, builtin: true });
      await dashboardAction();
      return;
    }
    if (id === "builtin:rename" || id === "builtin:delete" || id === "builtin:new-file") {
      openFileDialog(request, id);
      finish(request, { itemId: id, row: request.row, builtin: true });
      return;
    }
    if (!target?.row || builtinDisabledReason(id, target)) return;
    busy.current = true;
    setOpen(false);
    try {
      await runBuiltinMenuItem(
        id as "builtin:stop" | "builtin:close-tab",
        { row: target.row, tab: target.tab },
        request.run,
      );
      finish(request, { itemId: id, row: request.row, builtin: true });
    } catch (error) {
      finish(request, { closed: true }, error);
    }
  };
  const hasIcons = (items: Array<HostMenuItem | string>): boolean =>
    items.some(
      (item) =>
        typeof item !== "string" &&
        !builtinMenuId(item.id) &&
        (!!item.icon || (!!item.submenu && hasIcons(item.submenu))),
    );
  const fallback =
    request.type === "frame" ||
    (request.type === "inbox" && inboxMenu === null) ||
    (request.type === "default" && !target) ||
    (request.type === "native" && !nativeOpened);
  const items = fallback
    ? DASHBOARD_MENU_ITEMS.filter(
        (item) => typeof item === "string" || !!request.dashboard?.editSource,
      )
    : (inboxMenu?.items ?? request.items ?? []);
  const icons = fallback || hasIcons(items);
  const renderItems = (items: Array<HostMenuItem | string>): ReactNode =>
    items.map((raw, index) => {
      const item = typeof raw === "string" ? { id: raw } : raw;
      const id = builtinMenuId(item.id);
      if (item.separator) return <DropdownMenuSeparator key={`separator:${index}`} />;
      if (id) {
        if (isDashboardMenuId(id) && !dashboardMenuAction(id, request.dashboard)) return null;
        const spec = BUILTIN_MENU_ITEMS[id];
        const reason = builtinDisabledReason(id, target);
        const disabled = item.enabled === false || !!reason;
        const Icon = spec.icon;
        const label = (
          <>
            {icons && <Icon aria-hidden />}
            {id === "builtin:reload-pane" && request.dashboard?.centre ? "Reload tab" : spec.label}
          </>
        );
        if (id === "builtin:set-role")
          return (
            <DropdownMenuSub key={id}>
              <DropdownMenuSubTrigger
                disabled={disabled}
                title={reason}
                className="data-disabled:opacity-50"
              >
                {label}
              </DropdownMenuSubTrigger>
              <DropdownMenuSubContent>
                <RoleChoices
                  kit={DROPDOWN_MENU_KIT}
                  choice={target?.choice}
                  onChoose={(patch) => target?.choose(patch)}
                  onDone={() => finish(request, { itemId: id, row: request.row, builtin: true })}
                />
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          );
        return (
          <DropdownMenuItem
            key={id}
            disabled={disabled}
            title={reason}
            className={reason ? "data-disabled:pointer-events-auto" : undefined}
            variant={id === "builtin:stop" ? "destructive" : "default"}
            onSelect={() => void chooseBuiltin(id)}
          >
            {label}
          </DropdownMenuItem>
        );
      }
      const label = <MenuLabel item={item} />;
      const choices =
        item.segmented ??
        (item.radioGroup
          ? items.filter(
              (candidate): candidate is HostMenuItem =>
                typeof candidate !== "string" && candidate.radioGroup === item.radioGroup,
            )
          : undefined);
      if (choices) {
        if (
          !item.segmented &&
          items.find(
            (candidate) =>
              typeof candidate !== "string" && candidate.radioGroup === item.radioGroup,
          ) !== raw
        )
          return null;
        return (
          <DropdownMenuRadioGroup
            key={item.id}
            aria-label={item.segmented ? item.label : item.radioGroup}
            value={choices.find((choice) => choice.checked)?.id}
            onValueChange={choose}
            className={item.segmented ? "flex gap-0.5 px-1 py-0.5" : undefined}
          >
            {choices.map((choice) => (
              <DropdownMenuRadioItem
                key={choice.id}
                value={choice.id}
                disabled={(!!item.segmented && item.enabled === false) || choice.enabled === false}
                className={
                  item.segmented
                    ? "flex-1 justify-center pl-2 [&>span:first-child]:hidden data-[state=checked]:bg-accent data-[state=checked]:font-medium data-[state=checked]:text-accent-foreground"
                    : undefined
                }
              >
                <MenuLabel item={choice} />
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        );
      }
      if (item.submenu)
        return (
          <DropdownMenuSub key={item.id}>
            <DropdownMenuSubTrigger disabled={item.enabled === false}>
              {label}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent>{renderItems(item.submenu)}</DropdownMenuSubContent>
          </DropdownMenuSub>
        );
      if (item.checked !== undefined)
        return (
          <DropdownMenuCheckboxItem
            key={item.id}
            checked={item.checked}
            disabled={item.enabled === false}
            onSelect={() => choose(item.id)}
          >
            {label}
          </DropdownMenuCheckboxItem>
        );
      return (
        <DropdownMenuItem
          key={item.id}
          disabled={item.enabled === false}
          onSelect={() => choose(item.id)}
        >
          {label}
        </DropdownMenuItem>
      );
    });
  const virtualAnchor = useCallback(
    (node: HTMLElement | null) => {
      // Radix positions DropdownMenu against its trigger; measure that anchor in the frame.
      if (node) node.getBoundingClientRect = () => hostAnchorRect(request);
      setAnchor(node);
    },
    [request],
  );
  const rect = hostAnchorRect(request);
  const anchorStyle = {
    position: "fixed" as const,
    left: rect.x,
    top: rect.y,
    width: rect.width,
    height: rect.height,
    opacity: 0,
    pointerEvents: "none" as const,
  };
  if (
    (request.type === "native" && nativeOpened) ||
    (request.type === "inbox" && inboxMenu === undefined)
  )
    return null;
  if (presentation === "popover")
    return createPortal(
      <>
        <span ref={virtualAnchor} style={anchorStyle} aria-hidden />
        {anchor &&
          (request.kind === "agent-inbox-chat" ? (
            <HostAgentInboxChat target={target} anchor={anchor} onClose={close} />
          ) : (
            <HostAgentPopover
              target={target}
              anchor={anchor}
              bounds={appPopoverBounds}
              onClose={close}
            />
          ))}
      </>,
      document.body,
    );
  // A menu left mounted for its exit animation refocuses itself on pointer leave,
  // which steals focus from the confirmation dialog that replaced it.
  if (!open) return null;
  return (
    <DropdownMenu
      open
      onOpenChange={(next) => {
        setOpen(next);
        if (!next && !busy.current) close();
      }}
    >
      <DropdownMenuTrigger ref={virtualAnchor} style={anchorStyle} tabIndex={-1} aria-hidden />
      <DropdownMenuContent
        side="bottom"
        align="start"
        className={request.type === "text" ? "min-w-44" : undefined}
        onCloseAutoFocus={(event) => event.preventDefault()}
      >
        {request.type === "text" && request.text ? (
          <TextEditMenu
            state={request.text}
            onChoose={(action) => {
              busy.current = true;
              setOpen(false);
              void request.editText?.(action).then(
                () => finish(request, { itemId: action, row: request.row }),
                (error) => finish(request, { closed: true }, error),
              );
            }}
          />
        ) : request.type === "default" && target ? (
          <HostAgentRowMenu
            target={target}
            onQuickView={() => {
              busy.current = true;
              setOpen(false);
              setPresentation("popover");
            }}
            onClose={close}
            onStop={() => void chooseBuiltin("builtin:stop")}
          />
        ) : (
          renderItems(items)
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

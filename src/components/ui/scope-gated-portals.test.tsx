// SPDX-License-Identifier: MIT
import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";

import { AlertDialog, AlertDialogContent, AlertDialogTitle } from "./alert-dialog";
import { Dialog, DialogContent, DialogTitle } from "./dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuTrigger } from "./dropdown-menu";
import { Popover, PopoverContent } from "./popover";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "./tooltip";

afterEach(() => {
  for (const k of ["gate:a", "gate:b"]) warmScopes.forget(k);
});

/** A body portal escapes the team wrapper's `display:none`, so every portaled primitive renders
 *  nothing while its owning team is hidden. */
describe("portaled primitives in a hidden team", () => {
  it.each([
    [
      "Dialog",
      <Dialog open key="d">
        <DialogContent>
          <DialogTitle>probe</DialogTitle>
        </DialogContent>
      </Dialog>,
    ],
    [
      "AlertDialog",
      <AlertDialog open key="ad">
        <AlertDialogContent>
          <AlertDialogTitle>probe</AlertDialogTitle>
        </AlertDialogContent>
      </AlertDialog>,
    ],
    [
      "Popover",
      <Popover open key="p">
        <PopoverContent>probe</PopoverContent>
      </Popover>,
    ],
    [
      "DropdownMenu",
      <DropdownMenu open key="dm">
        <DropdownMenuTrigger>t</DropdownMenuTrigger>
        <DropdownMenuContent>probe</DropdownMenuContent>
      </DropdownMenu>,
    ],
    [
      "Tooltip",
      <TooltipProvider key="t">
        <Tooltip open>
          <TooltipTrigger>t</TooltipTrigger>
          <TooltipContent>probe</TooltipContent>
        </Tooltip>
      </TooltipProvider>,
    ],
  ])("%s portals only while its team is shown", (_name, tree) => {
    act(() => warmScopes.activate("gate:a"));
    render(<OwningScopeProvider scope="gate:a">{tree}</OwningScopeProvider>);
    expect(screen.getAllByText("probe").length).toBeGreaterThan(0);
    act(() => warmScopes.activate("gate:b"));
    expect(screen.queryByText("probe")).not.toBeInTheDocument();
    act(() => warmScopes.activate("gate:a"));
    expect(screen.getAllByText("probe").length).toBeGreaterThan(0);
  });
});

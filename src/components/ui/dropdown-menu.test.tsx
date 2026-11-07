// SPDX-License-Identifier: MIT
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "./dropdown-menu";

afterEach(cleanup);
it("marks a destructive action with the native destructive variant", async () => {
  render(
    <DropdownMenu open>
      <DropdownMenuTrigger>Actions</DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuItem variant="destructive">Stop…</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>,
  );
  expect(await screen.findByRole("menuitem", { name: "Stop…" })).toHaveAttribute(
    "data-variant",
    "destructive",
  );
});

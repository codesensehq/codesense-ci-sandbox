// SPDX-License-Identifier: MIT
import { fireEvent, screen, within } from "@testing-library/react";

/** Choose `option` in the kit select named `label` (a `PortaledSelect`: open it, pick the row). */
export async function pickOption(
  scope: Pick<ReturnType<typeof within>, "getByRole"> = screen,
  label: string,
  option: string | RegExp,
): Promise<void> {
  fireEvent.click(scope.getByRole("combobox", { name: label }));
  fireEvent.click(await screen.findByRole("option", { name: option }));
}

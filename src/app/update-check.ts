// SPDX-License-Identifier: MIT
import { isBetaChannel, readBuildChannel, type BuildChannelInfo } from "@/features/license/channel";

type UpdateToastModule = Pick<typeof import("@/app/update-toast"), "runUpdateCheck">;

/**
 * Starts the internal build's update check (the GitHub path, with its toast). In the beta
 * channels Rust checks the channel's static manifest itself (launch, daily, on a wake;
 * SV-PBF-278), and the app learns of a staged build from `license://state`. Returns null when
 * nothing was started.
 */
export function startUpdateCheck(
  info: BuildChannelInfo = readBuildChannel(),
  load: () => Promise<UpdateToastModule> = () => import("@/app/update-toast"),
): Promise<void> | null {
  if (isBetaChannel(info)) return null;
  return load().then(({ runUpdateCheck }) => runUpdateCheck());
}

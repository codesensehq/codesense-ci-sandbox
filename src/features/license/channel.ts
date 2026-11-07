// SPDX-License-Identifier: MIT
/**
 * The build channel. The Rust side injects it before any app code runs
 * (`app_channel.rs`), so this read is synchronous. Anything other than a beta
 * channel, including a page with no injection, is the internal build.
 */
export type BuildChannel = "internal" | "beta" | "beta-staging";

export interface BuildChannelInfo {
  channel: BuildChannel;
  publicHost: string;
}

declare global {
  interface Window {
    __CODESENSE_CHANNEL__?: string;
    __CODESENSE_PUBLIC_HOST__?: string;
  }
}

export function readBuildChannel(): BuildChannelInfo {
  const channel = typeof window === "undefined" ? undefined : window.__CODESENSE_CHANNEL__;
  if (channel === "beta" || channel === "beta-staging") {
    return { channel, publicHost: window.__CODESENSE_PUBLIC_HOST__ || "codesense.ai" };
  }
  return { channel: "internal", publicHost: "codesense.ai" };
}

/** True in the beta channels, where the update commands exist. */
export function isBetaChannel(info: BuildChannelInfo): boolean {
  return info.channel !== "internal";
}

/**
 * SV-PBF-278: where a person downloads CodeSense by hand. One constant until the public download
 * page's address is confirmed.
 */
export const PUBLIC_DOWNLOAD_URL = "https://codesense.ai/download";

// SPDX-License-Identifier: MIT
declare global {
  interface Window {
    __CODESENSE_DASHBOARD_PUBLISH__?: boolean;
  }
}
export function dashboardPublishingEnabled(): boolean {
  return typeof window !== "undefined" && window.__CODESENSE_DASHBOARD_PUBLISH__ === true;
}

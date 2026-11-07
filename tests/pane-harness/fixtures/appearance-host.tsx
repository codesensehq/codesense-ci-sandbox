import "../../../src/index.css";
import React from "react";
import { createRoot } from "react-dom/client";
import { ThemeProvider } from "next-themes";
import { Toaster } from "sonner";
import { installFakeTauri } from "../../frame-security/fixtures/fake-tauri";
import { AppearanceSection } from "../../../src/features/settings/components/appearance-section";
import { AppThemeProvider } from "../../../src/themes/theme-provider";
import { InfoTip, TooltipProvider } from "../../../src/components/ui/tooltip";
import { TerminalTileChrome } from "../../../src/features/viewer/components/terminal-tile-chrome";
import { useTerminalThemeConfig } from "../../../src/themes/use-terminal-theme";
import { PaneStartupOverlay } from "../../../src/features/viewer/components/pane-startup-overlay";
import { InboxMessageContent } from "../../../src/features/weft/components/inbox-message-content";
import { Input } from "../../../src/components/ui/input";
const noop = () => {};
function TerminalPreview() {
  const config = useTerminalThemeConfig();
  return (
    <div data-surface="terminal">
      <div data-surface="canvas" className="bg-background p-3" data-testid="app-surface-preview">
        <InboxMessageContent text="[Inbox link](https://example.test)" contentType="markdown" />
        <InboxMessageContent
          text={'<a href="https://example.test">HTML inbox link</a>'}
          contentType="html"
        />
        <Input aria-label="Repository URL" placeholder="https://github.com/owner/repo" />
      </div>
      <TerminalTileChrome
        workspace={{
          id: "preview",
          kind: "story",
          cwd: "/theme-preview",
          status: "ready",
          storyId: "Preview",
        }}
        isFocused
        tabs={[{ id: "preview-tab", name: "Shell", sessionId: "preview-session" }]}
        activeTabId="preview-tab"
        chromeBg={config.chromeBg}
        terminalBg={config.theme.background}
        onSelectTab={noop}
        onCloseTab={noop}
        onHideTab={noop}
        onEndTab={noop}
        onEndOtherTabs={noop}
        onEndAllTabs={noop}
        onNewShell={noop}
        onReorderTab={noop}
        onRenameTab={noop}
        onFocusTile={noop}
      />
      <div className="relative h-40" data-testid="startup-failed-preview">
        <PaneStartupOverlay
          phase="failed"
          failure={{ reason: "error", detail: "Could not launch the shell." }}
          started={false}
          background={config.theme.background!}
          onRetry={noop}
        />
      </div>
      <div className="relative h-24" data-testid="startup-starting-preview">
        <PaneStartupOverlay
          phase="starting"
          failure={null}
          started={false}
          background={config.theme.background!}
          onRetry={noop}
        />
      </div>
    </div>
  );
}
installFakeTauri({});
createRoot(document.getElementById("root")!).render(
  <ThemeProvider attribute="class" defaultTheme="light" enableSystem>
    <AppThemeProvider />
    <main
      style={{
        maxWidth: 760,
        margin: "0 auto",
        padding: "0 24px 32px",
        height: "100dvh",
        overflowY: "auto",
      }}
    >
      <AppearanceSection query="" />
      <TooltipProvider>
        <TerminalPreview />
        <InfoTip label="Theme help">
          <button>Show theme help</button>
        </InfoTip>
      </TooltipProvider>
    </main>
    <Toaster />
  </ThemeProvider>,
);

// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { useTheme } from "next-themes";
import { Toaster as Sonner, type ToasterProps } from "sonner";
import {
  CircleCheckIcon,
  InfoIcon,
  TriangleAlertIcon,
  OctagonXIcon,
  Loader2Icon,
} from "lucide-react";

import { collapsePersistentToasts, persistentToastCss } from "./persistent-toast";
import { TOASTER_OFFSET, TOASTER_POSITION } from "./toaster-placement";

const PERSISTENT_TOAST_CSS = persistentToastCss();

const Toaster = ({ ...props }: ToasterProps) => {
  const { theme = "system" } = useTheme();
  const hostRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const host = hostRef.current;
    return host ? collapsePersistentToasts(host) : undefined;
  }, []);

  return (
    <div ref={hostRef} className="contents">
      <style>{PERSISTENT_TOAST_CSS}</style>
      <Sonner
        position={TOASTER_POSITION}
        offset={TOASTER_OFFSET}
        theme={theme as ToasterProps["theme"]}
        className="toaster group"
        icons={{
          success: <CircleCheckIcon className="size-4" />,
          info: <InfoIcon className="size-4" />,
          warning: <TriangleAlertIcon className="size-4" />,
          error: <OctagonXIcon className="size-4" />,
          loading: <Loader2Icon className="size-4 animate-spin" />,
        }}
        style={
          {
            "--normal-bg": "var(--popover)",
            "--normal-text": "var(--popover-foreground)",
            "--normal-border": "var(--border)",
            "--border-radius": "var(--radius)",
          } as React.CSSProperties
        }
        toastOptions={{
          classNames: {
            toast: "cn-toast",
          },
        }}
        {...props}
      />
    </div>
  );
};

export { Toaster };

import { readDocTheme } from "../../../src/features/dashboard/components/doc-theme";
const frame = document.createElement("iframe");
frame.style.cssText =
  'width:800px;height:700px;color:#31435b;--background:#fff;--foreground:#1f2328;--cs-viewer-bg:#fff;--muted-foreground:#656d76;--accent:#0969da;font-family:"Geist Variable"';
frame.src = `http://127.0.0.1:${new URLSearchParams(location.search).get("port")}/editor-frame`;
const events: unknown[] = [];
Object.assign(window, {
  events,
  setScheme(dark: boolean) {
    frame.style.setProperty("--background", dark ? "#0d1117" : "#fff");
    frame.style.setProperty("--cs-viewer-bg", dark ? "#0d1117" : "#fff");
    frame.style.setProperty("--foreground", dark ? "#e6edf3" : "#1f2328");
    frame.style.setProperty("--muted-foreground", dark ? "#8b949e" : "#656d76");
    frame.style.setProperty("--accent", dark ? "#58a6ff" : "#0969da");
    frame.style.color = dark ? "#e6edf3" : "#31435b";
    frame.contentWindow!.postMessage({ type: "codesense:theme", theme: readDocTheme(frame) }, "*");
  },
});
addEventListener("message", (e) => {
  events.push(e.data);
  if (e.data?.type === "codesense:ready")
    frame.contentWindow!.postMessage({ type: "codesense:theme", theme: readDocTheme(frame) }, "*");
});
document.body.append(frame);

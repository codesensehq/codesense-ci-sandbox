/* CodeSense dashboard kit — window.codesense.kit.
 * Injected into every live doc (dashboard files, right-pane panes, html inbox
 * messages) right after the window.codesense bridge, with kit.css beside it.
 * Dependency-free. Icons are Lucide (ISC License, Copyright (c) Lucide Icons and
 * Contributors; https://lucide.dev/license), the same glyphs the app draws. */
(function () {
  "use strict";
  var cs = (window.codesense = window.codesense || {});
  if (cs.kit) return;
  var SVG_NS = "http://www.w3.org/2000/svg";

  var ICONS = /*@icons-start*/{
    "activity": "<path d=\"M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2\"/>",
    "alarm-clock": "<circle cx=\"12\" cy=\"13\" r=\"8\"/><path d=\"M12 9v4l2 2\"/><path d=\"M5 3 2 6\"/><path d=\"m22 6-3-3\"/><path d=\"M6.38 18.7 4 21\"/><path d=\"M17.64 18.67 20 21\"/>",
    "archive": "<rect width=\"20\" height=\"5\" x=\"2\" y=\"3\" rx=\"1\"/><path d=\"M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8\"/><path d=\"M10 12h4\"/>",
    "arrow-left": "<path d=\"m12 19-7-7 7-7\"/><path d=\"M19 12H5\"/>",
    "arrow-up-right": "<path d=\"M7 7h10v10\"/><path d=\"M7 17 17 7\"/>",
    "ban": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M4.929 4.929 19.07 19.071\"/>",
    "bot": "<path d=\"M12 8V4H8\"/><rect width=\"16\" height=\"12\" x=\"4\" y=\"8\" rx=\"2\"/><path d=\"M2 14h2\"/><path d=\"M20 14h2\"/><path d=\"M15 13v2\"/><path d=\"M9 13v2\"/>",
    "calendar": "<path d=\"M8 2v4\"/><path d=\"M16 2v4\"/><rect width=\"18\" height=\"18\" x=\"3\" y=\"4\" rx=\"2\"/><path d=\"M3 10h18\"/>",
    "calendar-clock": "<path d=\"M16 14v2.2l1.6 1\"/><path d=\"M16 2v4\"/><path d=\"M21 7.5V6a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h3.5\"/><path d=\"M3 10h5\"/><path d=\"M8 2v4\"/><circle cx=\"16\" cy=\"16\" r=\"6\"/>",
    "chart-network": "<path d=\"m13.11 7.664 1.78 2.672\"/><path d=\"m14.162 12.788-3.324 1.424\"/><path d=\"m20 4-6.06 1.515\"/><path d=\"M3 3v16a2 2 0 0 0 2 2h16\"/><circle cx=\"12\" cy=\"6\" r=\"2\"/><circle cx=\"16\" cy=\"12\" r=\"2\"/><circle cx=\"9\" cy=\"15\" r=\"2\"/>",
    "check": "<path d=\"M20 6 9 17l-5-5\"/>",
    "chevron-down": "<path d=\"m6 9 6 6 6-6\"/>",
    "chevron-right": "<path d=\"m9 18 6-6-6-6\"/>",
    "circle": "<circle cx=\"12\" cy=\"12\" r=\"10\"/>",
    "circle-alert": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><line x1=\"12\" x2=\"12\" y1=\"8\" y2=\"12\"/><line x1=\"12\" x2=\"12.01\" y1=\"16\" y2=\"16\"/>",
    "circle-check-big": "<path d=\"M21.801 10A10 10 0 1 1 17 3.335\"/><path d=\"m9 11 3 3L22 4\"/>",
    "circle-dashed": "<path d=\"M10.1 2.182a10 10 0 0 1 3.8 0\"/><path d=\"M13.9 21.818a10 10 0 0 1-3.8 0\"/><path d=\"M17.609 3.721a10 10 0 0 1 2.69 2.7\"/><path d=\"M2.182 13.9a10 10 0 0 1 0-3.8\"/><path d=\"M20.279 17.609a10 10 0 0 1-2.7 2.69\"/><path d=\"M21.818 10.1a10 10 0 0 1 0 3.8\"/><path d=\"M3.721 6.391a10 10 0 0 1 2.7-2.69\"/><path d=\"M6.391 20.279a10 10 0 0 1-2.69-2.7\"/>",
    "circle-dot": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><circle cx=\"12\" cy=\"12\" r=\"1\"/>",
    "circle-help": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3\"/><path d=\"M12 17h.01\"/>",
    "circle-play": "<path d=\"M9 9.003a1 1 0 0 1 1.517-.859l4.997 2.997a1 1 0 0 1 0 1.718l-4.997 2.997A1 1 0 0 1 9 14.996z\"/><circle cx=\"12\" cy=\"12\" r=\"10\"/>",
    "circle-x": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"m15 9-6 6\"/><path d=\"m9 9 6 6\"/>",
    "clock": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M12 6v6l4 2\"/>",
    "corner-left-up": "<path d=\"M14 9 9 4 4 9\"/><path d=\"M20 20h-7a4 4 0 0 1-4-4V4\"/>",
    "crosshair": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><line x1=\"22\" x2=\"18\" y1=\"12\" y2=\"12\"/><line x1=\"6\" x2=\"2\" y1=\"12\" y2=\"12\"/><line x1=\"12\" x2=\"12\" y1=\"6\" y2=\"2\"/><line x1=\"12\" x2=\"12\" y1=\"22\" y2=\"18\"/>",
    "crown": "<path d=\"M11.562 3.266a.5.5 0 0 1 .876 0L15.39 8.87a1 1 0 0 0 1.516.294L21.183 5.5a.5.5 0 0 1 .798.519l-2.834 10.246a1 1 0 0 1-.956.734H5.81a1 1 0 0 1-.957-.734L2.02 6.02a.5.5 0 0 1 .798-.519l4.276 3.664a1 1 0 0 0 1.516-.294z\"/><path d=\"M5 21h14\"/>",
    "external-link": "<path d=\"M15 3h6v6\"/><path d=\"M10 14 21 3\"/><path d=\"M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6\"/>",
    "eye": "<path d=\"M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0\"/><circle cx=\"12\" cy=\"12\" r=\"3\"/>",
    "eye-off": "<path d=\"M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49\"/><path d=\"M14.084 14.158a3 3 0 0 1-4.242-4.242\"/><path d=\"M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143\"/><path d=\"m2 2 20 20\"/>",
    "file": "<path d=\"M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z\"/><path d=\"M14 2v5a1 1 0 0 0 1 1h5\"/>",
    "file-code": "<path d=\"M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z\"/><path d=\"M14 2v5a1 1 0 0 0 1 1h5\"/><path d=\"M10 12.5 8 15l2 2.5\"/><path d=\"m14 12.5 2 2.5-2 2.5\"/>",
    "file-text": "<path d=\"M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z\"/><path d=\"M14 2v5a1 1 0 0 0 1 1h5\"/><path d=\"M10 9H8\"/><path d=\"M16 13H8\"/><path d=\"M16 17H8\"/>",
    "filter": "<path d=\"M10 20a1 1 0 0 0 .553.895l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .517-1.341L21.74 4.67A1 1 0 0 0 21 3H3a1 1 0 0 0-.742 1.67l7.225 7.989A2 2 0 0 1 10 14z\"/>",
    "folder": "<path d=\"M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z\"/>",
    "folder-open": "<path d=\"m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2\"/>",
    "git-branch": "<path d=\"M15 6a9 9 0 0 0-9 9V3\"/><circle cx=\"18\" cy=\"6\" r=\"3\"/><circle cx=\"6\" cy=\"18\" r=\"3\"/>",
    "git-commit-horizontal": "<circle cx=\"12\" cy=\"12\" r=\"3\"/><line x1=\"3\" x2=\"9\" y1=\"12\" y2=\"12\"/><line x1=\"15\" x2=\"21\" y1=\"12\" y2=\"12\"/>",
    "git-fork": "<circle cx=\"12\" cy=\"18\" r=\"3\"/><circle cx=\"6\" cy=\"6\" r=\"3\"/><circle cx=\"18\" cy=\"6\" r=\"3\"/><path d=\"M18 9v2c0 .6-.4 1-1 1H7c-.6 0-1-.4-1-1V9\"/><path d=\"M12 12v3\"/>",
    "git-pull-request": "<circle cx=\"18\" cy=\"18\" r=\"3\"/><circle cx=\"6\" cy=\"6\" r=\"3\"/><path d=\"M13 6h3a2 2 0 0 1 2 2v7\"/><line x1=\"6\" x2=\"6\" y1=\"9\" y2=\"21\"/>",
    "history": "<path d=\"M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8\"/><path d=\"M3 3v5h5\"/><path d=\"M12 7v5l4 2\"/>",
    "image": "<rect width=\"18\" height=\"18\" x=\"3\" y=\"3\" rx=\"2\" ry=\"2\"/><circle cx=\"9\" cy=\"9\" r=\"2\"/><path d=\"m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21\"/>",
    "inbox": "<polyline points=\"22 12 16 12 14 15 10 15 8 12 2 12\"/><path d=\"M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z\"/>",
    "info": "<circle cx=\"12\" cy=\"12\" r=\"10\"/><path d=\"M12 16v-4\"/><path d=\"M12 8h.01\"/>",
    "laptop": "<path d=\"M18 5a2 2 0 0 1 2 2v8.526a2 2 0 0 0 .212.897l1.068 2.127a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45l1.068-2.127A2 2 0 0 0 4 15.526V7a2 2 0 0 1 2-2z\"/><path d=\"M20.054 15.987H3.946\"/>",
    "layers": "<path d=\"M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z\"/><path d=\"M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12\"/><path d=\"M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17\"/>",
    "layout-list": "<rect width=\"7\" height=\"7\" x=\"3\" y=\"3\" rx=\"1\"/><rect width=\"7\" height=\"7\" x=\"3\" y=\"14\" rx=\"1\"/><path d=\"M14 4h7\"/><path d=\"M14 9h7\"/><path d=\"M14 15h7\"/><path d=\"M14 20h7\"/>",
    "link": "<path d=\"M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71\"/><path d=\"M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71\"/>",
    "list": "<path d=\"M3 5h.01\"/><path d=\"M3 12h.01\"/><path d=\"M3 19h.01\"/><path d=\"M8 5h13\"/><path d=\"M8 12h13\"/><path d=\"M8 19h13\"/>",
    "list-filter": "<path d=\"M2 5h20\"/><path d=\"M6 12h12\"/><path d=\"M9 19h6\"/>",
    "list-todo": "<path d=\"M13 5h8\"/><path d=\"M13 12h8\"/><path d=\"M13 19h8\"/><path d=\"m3 17 2 2 4-4\"/><rect x=\"3\" y=\"4\" width=\"6\" height=\"6\" rx=\"1\"/>",
    "list-tree": "<path d=\"M8 5h13\"/><path d=\"M13 12h8\"/><path d=\"M13 19h8\"/><path d=\"M3 10a2 2 0 0 0 2 2h3\"/><path d=\"M3 5v12a2 2 0 0 0 2 2h3\"/>",
    "loader-circle": "<path d=\"M21 12a9 9 0 1 1-6.219-8.56\"/>",
    "lock": "<rect width=\"18\" height=\"11\" x=\"3\" y=\"11\" rx=\"2\" ry=\"2\"/><path d=\"M7 11V7a5 5 0 0 1 10 0v4\"/>",
    "mail": "<path d=\"m22 7-8.991 5.727a2 2 0 0 1-2.009 0L2 7\"/><rect x=\"2\" y=\"4\" width=\"20\" height=\"16\" rx=\"2\"/>",
    "maximize": "<path d=\"M8 3H5a2 2 0 0 0-2 2v3\"/><path d=\"M21 8V5a2 2 0 0 0-2-2h-3\"/><path d=\"M3 16v3a2 2 0 0 0 2 2h3\"/><path d=\"M16 21h3a2 2 0 0 0 2-2v-3\"/>",
    "message-circle-question": "<path d=\"M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092 10 10 0 1 0-4.777-4.719\"/><path d=\"M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3\"/><path d=\"M12 17h.01\"/>",
    "minus": "<path d=\"M5 12h14\"/>",
    "monitor": "<rect width=\"20\" height=\"14\" x=\"2\" y=\"3\" rx=\"2\"/><line x1=\"8\" x2=\"16\" y1=\"21\" y2=\"21\"/><line x1=\"12\" x2=\"12\" y1=\"17\" y2=\"21\"/>",
    "network": "<rect x=\"16\" y=\"16\" width=\"6\" height=\"6\" rx=\"1\"/><rect x=\"2\" y=\"16\" width=\"6\" height=\"6\" rx=\"1\"/><rect x=\"9\" y=\"2\" width=\"6\" height=\"6\" rx=\"1\"/><path d=\"M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3\"/><path d=\"M12 12V8\"/>",
    "pause": "<rect x=\"14\" y=\"3\" width=\"5\" height=\"18\" rx=\"1\"/><rect x=\"5\" y=\"3\" width=\"5\" height=\"18\" rx=\"1\"/>",
    "pin": "<path d=\"M12 17v5\"/><path d=\"M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z\"/>",
    "play": "<path d=\"M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z\"/>",
    "plus": "<path d=\"M5 12h14\"/><path d=\"M12 5v14\"/>",
    "refresh-cw": "<path d=\"M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8\"/><path d=\"M21 3v5h-5\"/><path d=\"M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16\"/><path d=\"M8 16H3v5\"/>",
    "repeat": "<path d=\"m17 2 4 4-4 4\"/><path d=\"M3 11v-1a4 4 0 0 1 4-4h14\"/><path d=\"m7 22-4-4 4-4\"/><path d=\"M21 13v1a4 4 0 0 1-4 4H3\"/>",
    "rotate-ccw": "<path d=\"M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8\"/><path d=\"M3 3v5h5\"/>",
    "scroll-text": "<path d=\"M15 12h-5\"/><path d=\"M15 8h-5\"/><path d=\"M19 17V5a2 2 0 0 0-2-2H4\"/><path d=\"M8 21h12a2 2 0 0 0 2-2v-1a1 1 0 0 0-1-1H11a1 1 0 0 0-1 1v1a2 2 0 1 1-4 0V5a2 2 0 1 0-4 0v2a1 1 0 0 0 1 1h3\"/>",
    "search": "<path d=\"m21 21-4.34-4.34\"/><circle cx=\"11\" cy=\"11\" r=\"8\"/>",
    "send": "<path d=\"M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z\"/><path d=\"m21.854 2.147-10.94 10.939\"/>",
    "server": "<rect width=\"20\" height=\"8\" x=\"2\" y=\"2\" rx=\"2\" ry=\"2\"/><rect width=\"20\" height=\"8\" x=\"2\" y=\"14\" rx=\"2\" ry=\"2\"/><line x1=\"6\" x2=\"6.01\" y1=\"6\" y2=\"6\"/><line x1=\"6\" x2=\"6.01\" y1=\"18\" y2=\"18\"/>",
    "settings": "<path d=\"M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915\"/><circle cx=\"12\" cy=\"12\" r=\"3\"/>",
    "shield-check": "<path d=\"M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z\"/><path d=\"m9 12 2 2 4-4\"/>",
    "square": "<rect width=\"18\" height=\"18\" x=\"3\" y=\"3\" rx=\"2\"/>",
    "square-kanban": "<rect width=\"18\" height=\"18\" x=\"3\" y=\"3\" rx=\"2\"/><path d=\"M8 7v7\"/><path d=\"M12 7v4\"/><path d=\"M16 7v9\"/>",
    "terminal": "<path d=\"M12 19h8\"/><path d=\"m4 17 6-6-6-6\"/>",
    "timer": "<line x1=\"10\" x2=\"14\" y1=\"2\" y2=\"2\"/><line x1=\"12\" x2=\"15\" y1=\"14\" y2=\"11\"/><circle cx=\"12\" cy=\"14\" r=\"8\"/>",
    "triangle-alert": "<path d=\"m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3\"/><path d=\"M12 9v4\"/><path d=\"M12 17h.01\"/>",
    "user": "<path d=\"M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2\"/><circle cx=\"12\" cy=\"7\" r=\"4\"/>",
    "users": "<path d=\"M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2\"/><path d=\"M16 3.128a4 4 0 0 1 0 7.744\"/><path d=\"M22 21v-2a4 4 0 0 0-3-3.87\"/><circle cx=\"9\" cy=\"7\" r=\"4\"/>",
    "wifi-off": "<path d=\"M12 20h.01\"/><path d=\"M8.5 16.429a5 5 0 0 1 7 0\"/><path d=\"M5 12.859a10 10 0 0 1 5.17-2.69\"/><path d=\"M19 12.859a10 10 0 0 0-2.007-1.523\"/><path d=\"M2 8.82a15 15 0 0 1 4.177-2.643\"/><path d=\"M22 8.82a15 15 0 0 0-11.288-3.764\"/><path d=\"m2 2 20 20\"/>",
    "workflow": "<rect width=\"8\" height=\"8\" x=\"3\" y=\"3\" rx=\"2\"/><path d=\"M7 11v4a2 2 0 0 0 2 2h4\"/><rect width=\"8\" height=\"8\" x=\"13\" y=\"13\" rx=\"2\"/>",
    "x": "<path d=\"M18 6 6 18\"/><path d=\"m6 6 12 12\"/>",
    "zap": "<path d=\"M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z\"/>",
  }/*@icons-end*/;

  // The app's status-mark vocabulary for a sessions() row's `state`: the colours
  // are kit.css's --cs-state-* tokens (the app's status colours, light and dark).
  var AGENT_STATES = {
    running: { label: "Working", color: "var(--cs-state-running)", icon: "loader-circle" },
    idle: { label: "Ready", color: "var(--cs-state-idle)", icon: "circle" },
    "needs-you": { label: "Needs you", color: "var(--cs-state-needs-you)", icon: "message-circle-question" },
    exited: { label: "Ended", color: "var(--cs-state-exited)", icon: "square" },
    offline: { label: "Offline", color: "var(--cs-state-offline)", icon: "wifi-off" },
    unknown: { label: "Activity unknown", color: "var(--cs-state-unknown)", icon: "circle-dashed" },
  };
  // signal_progress phases, coloured by the story status each maps to.
  var PHASES = {
    planning: { label: "Planning", color: "#64748b", icon: "circle-dashed" },
    dev: { label: "Dev", color: "#0ea5e9", icon: "circle" },
    review: { label: "Review", color: "#8b5cf6", icon: "eye" },
    pr: { label: "PR", color: "#8b5cf6", icon: "git-pull-request" },
    blocked: { label: "Blocked", color: "#ef4444", icon: "ban" },
    done: { label: "Done", color: "#10b981", icon: "circle-check-big" },
  };
  // The default story statuses (pipeline STATUS_CONFIG). A team can define its
  // own: prefer codesense.taxonomy() colours when the page reads them.
  var STORY_STATUSES = {
    planning: { label: "Planning", color: "#64748b", icon: "circle-dashed" },
    todo: { label: "Todo", color: "#94a3b8", icon: "circle-dot" },
    "in-progress": { label: "In progress", color: "#0ea5e9", icon: "circle" },
    blocked: { label: "Blocked", color: "#ef4444", icon: "ban" },
    review: { label: "Review", color: "#8b5cf6", icon: "eye" },
    qa: { label: "QA", color: "#f59e0b", icon: "shield-check" },
    done: { label: "Done", color: "#10b981", icon: "circle-check-big" },
  };
  var NEUTRAL = { label: "", color: "#94a3b8", icon: "circle-help" };

  // ── DOM ─────────────────────────────────────────────────────────────────
  function append(node, child) {
    if (child == null || child === false) return;
    if (Array.isArray(child)) {
      child.forEach(function (c) { append(node, c); });
      return;
    }
    node.appendChild(typeof child === "object" ? child : document.createTextNode(String(child)));
  }
  function setProps(node, props, svg) {
    if (!props) return;
    Object.keys(props).forEach(function (k) {
      var v = props[k];
      if (v == null || v === false) return;
      if (k === "class" || k === "className") {
        if (svg) node.setAttribute("class", v);
        else node.className = v;
      } else if (k === "text") node.textContent = String(v);
      else if (k === "style" && typeof v === "object") {
        Object.keys(v).forEach(function (s) {
          if (s.indexOf("--") === 0) node.style.setProperty(s, v[s]);
          else node.style[s] = v[s];
        });
      } else if (k === "dataset" && typeof v === "object") {
        Object.keys(v).forEach(function (d) { node.dataset[d] = v[d]; });
      } else if (k.indexOf("on") === 0 && typeof v === "function") {
        node.addEventListener(k.slice(2).toLowerCase(), v);
      } else node.setAttribute(k, v === true ? "" : String(v));
    });
  }
  /** Build an element safely: h('div', {class, text, onClick, ...attrs}, ...children).
   *  Strings become text nodes, never HTML. */
  function h(tag, props) {
    var node = document.createElement(tag);
    setProps(node, props, false);
    append(node, Array.prototype.slice.call(arguments, 2));
    return node;
  }
  /** As h(), for SVG elements. */
  function s(tag, props) {
    var node = document.createElementNS(SVG_NS, tag);
    setProps(node, props, true);
    append(node, Array.prototype.slice.call(arguments, 2));
    return node;
  }

  // ── Icons ──────────────────────────────────────────────────────────────
  var missingIcons = new Set();
  function iconMarkup(name, opts) {
    if (!ICONS[name] && !missingIcons.has(name)) {
      missingIcons.add(name);
      console.error("CodeSense kit: unknown icon \"" + name + "\"");
    }
    opts = opts || {};
    var size = opts.size || 16;
    var body = ICONS[name] || ICONS["circle-help"];
    var stroke = opts.strokeWidth || 2;
    var cls = "cs-icon" + (opts.class ? " " + String(opts.class).replace(/["<>&]/g, "") : "");
    return (
      '<svg xmlns="' + SVG_NS + '" width="' + size + '" height="' + size +
      '" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="' + stroke +
      '" stroke-linecap="round" stroke-linejoin="round" class="' + cls + '" aria-hidden="true"' +
      (ICONS[name] ? "" : ' data-missing="' + String(name).replace(/[^a-z0-9-]/gi, "") + '"') + ">" + body + "</svg>"
    );
  }
  /** An inline Lucide icon as an <svg> element: icon('bot', {size, strokeWidth, class, title}).
   *  An unknown name logs once per frame, draws circle-help and carries data-missing. */
  function icon(name, opts) {
    var holder = document.createElement("span");
    holder.innerHTML = iconMarkup(name, opts); // markup is the kit's own constant
    var svg = holder.firstChild;
    if (opts && opts.title) {
      svg.setAttribute("role", "img");
      svg.removeAttribute("aria-hidden");
      svg.setAttribute("aria-label", opts.title);
    }
    return svg;
  }
  function iconNames() { return Object.keys(ICONS).sort(); }

  // ── Status ─────────────────────────────────────────────────────────────
  function lookup(table, key) {
    var hit = table[key];
    return hit ? { key: key, label: hit.label, color: hit.color, icon: hit.icon }
      : { key: key, label: key ? String(key) : "Unknown", color: NEUTRAL.color, icon: NEUTRAL.icon };
  }
  function agentState(state) { return lookup(AGENT_STATES, state); }
  function phase(p) { return lookup(PHASES, p); }
  function storyStatus(st) { return lookup(STORY_STATUSES, st); }

  /** A coloured dot for a sessions() state: statusDot('running', {title?}). */
  function statusDot(state, opts) {
    var info = agentState(state);
    return h("span", {
      class: "cs-dot",
      "data-state": info.key,
      title: (opts && opts.title) || info.label,
      style: { "--cs-dot-color": info.color },
      role: "img",
      "aria-label": info.label,
    });
  }
  function badgeFor(info, kind) {
    var b = h("span", { class: "cs-badge", style: { "--cs-badge-color": info.color } });
    b.dataset[kind] = info.key;
    b.append(icon(info.icon, { size: 12 }), document.createTextNode(info.label));
    return b;
  }
  /** A tinted badge with icon + label for a sessions() state. */
  function statusBadge(state) { return badgeFor(agentState(state), "state"); }
  /** A badge for a signal_progress phase. */
  function phaseBadge(p) { return badgeFor(phase(p), "phase"); }
  /** A badge for a story status; pass a taxonomy state ({label, color}) to use the team's own. */
  function storyBadge(status, taxonomyState) {
    var info = storyStatus(status);
    if (taxonomyState) {
      info.label = taxonomyState.label || info.label;
      info.color = taxonomyState.color || info.color;
    }
    return badgeFor(info, "status");
  }

  // ── Avatars ────────────────────────────────────────────────────────────
  var FACE_REF = /^(builtin-[a-z0-9-]+):([a-z0-9][a-z0-9-]{0,30})$/;
  function hue(text) {
    var n = 0;
    for (var i = 0; i < text.length; i++) n = (n * 31 + text.charCodeAt(i)) >>> 0;
    return n % 360;
  }
  function initials(name) {
    var parts = String(name || "?").replace(/[^\p{L}\p{N}\s_-]/gu, "").split(/[\s_-]+/).filter(Boolean);
    if (!parts.length) return "?";
    return (parts.length === 1 ? parts[0].slice(0, 2) : parts[0][0] + parts[1][0]).toUpperCase();
  }
  /** An avatar: avatar({face?, name, size?}). A built-in face ref ('builtin-funny:coffee-bot',
   *  a sessions() row's `avatar`) draws that face; anything else draws initials on a
   *  colour picked from the name. */
  function avatar(opts) {
    opts = typeof opts === "string" ? { name: opts } : opts || {};
    var size = opts.size || 24;
    var name = opts.name || opts.face || "?";
    var el = h("span", {
      class: "cs-avatar",
      title: name,
      style: { "--cs-avatar-size": size + "px", "--cs-avatar-hue": String(hue(name)) },
    });
    var m = FACE_REF.exec(String(opts.face || "").trim());
    if (m) {
      var img = h("img", { src: "/kit/v1/avatars/" + m[1] + "/" + m[2] + ".svg", alt: name });
      img.addEventListener("error", function () { img.replaceWith(document.createTextNode(initials(name))); });
      el.append(img);
    } else {
      el.textContent = initials(name);
    }
    return el;
  }

  // ── Small helpers ──────────────────────────────────────────────────────
  function toMs(t) {
    if (t == null) return NaN;
    if (typeof t === "number") return t;
    var n = Date.parse(t);
    return n;
  }
  /** '12s ago', '5m ago', '3h ago', '2d ago' from epoch ms or an ISO string. */
  function timeAgo(t, now) {
    var ms = toMs(t);
    if (!isFinite(ms)) return "";
    var sec = Math.max(0, Math.round(((now || Date.now()) - ms) / 1000));
    if (sec < 60) return sec + "s ago";
    if (sec < 3600) return Math.round(sec / 60) + "m ago";
    if (sec < 86400) return Math.round(sec / 3600) + "h ago";
    return Math.round(sec / 86400) + "d ago";
  }
  /** A centred empty state: empty('No sessions running.', 'bot'). */
  function empty(text, iconName) {
    return h("div", { class: "cs-empty" }, iconName ? icon(iconName, { size: 20 }) : null, h("div", { text: text }));
  }
  /** An inline notice: notice(text, 'info' | 'warning' | 'danger', {label, onClick}?).
   *  The optional action renders as a small button (e.g. Retry). */
  function notice(text, kind, action) {
    var k = kind || "info";
    return h("div", { class: "cs-notice is-" + k, role: k === "info" ? "status" : "alert" },
      icon(k === "info" ? "info" : "triangle-alert", { size: 14 }), h("span", { class: "cs-notice-text", text: text }),
      action ? h("button", { class: "cs-btn is-sm", type: "button", onClick: action.onClick, text: action.label }) : null);
  }
  /** A loading placeholder: skeleton({rows: 3, avatar?: true}) — shimmering bars
   *  shaped like list rows, shown until the first read lands. */
  function skeleton(opts) {
    opts = opts || {};
    var rows = opts.rows || 3;
    var out = h("div", { class: "cs-skeleton-group", "aria-busy": "true", "aria-label": "Loading" });
    for (var i = 0; i < rows; i++) {
      out.append(h("div", { class: "cs-skeleton-row" },
        opts.avatar ? h("span", { class: "cs-skeleton is-round" }) : null,
        h("span", { class: "cs-skeleton-lines" },
          h("span", { class: "cs-skeleton", style: { width: 40 + ((i * 37) % 45) + "%" } }),
          h("span", { class: "cs-skeleton is-thin", style: { width: 25 + ((i * 53) % 50) + "%" } }))));
    }
    return out;
  }
  /**
   * Render a list into `container` by key, reusing each item's element while its
   * version is unchanged, so a live re-read moves and patches rows instead of
   * redrawing them (no flicker, scroll and focus kept).
   * keyed(container, items, {key: (item) => id, render: (item) => element,
   *                          version?: (item) => string (default JSON),
   *                          update?: (element, item) => void})
   */
  function keyed(container, items, opts) { csCore.keyed(container, items, opts); }

  /** Whether this frame may use `name`: an SDK feature or a verb its grant admits.
   *  Delegates to codesense.has. Architecture: informational only; Rust checks every call. */
  function has(name) { return typeof cs.has === "function" && cs.has(name) === true; }
  function isUnavailable(err) {
    var m = String((err && err.message) || err || "");
    return /not allowlisted|not available|unknown (command|verb)|unsupported/i.test(m);
  }
  /** Call a bridge verb through its named helper when the app has one, else through
   *  invoke(). Rejects with err.unavailable = true when this app does not know it,
   *  so a page can show a notice instead of failing. */
  function call(verb, args) {
    var p;
    try {
      p = typeof cs[verb] === "function" ? cs[verb](args) : typeof cs.invoke === "function" ? cs.invoke(verb, args) : Promise.reject(new Error(verb + " is not available"));
    } catch (e) {
      p = Promise.reject(e);
    }
    return Promise.resolve(p).catch(function (err) {
      var e = err instanceof Error ? err : new Error(String(err));
      if (isUnavailable(e)) e.unavailable = true;
      throw e;
    });
  }
  /** Listen for a payloadless host event: on('routines-changed', cb) for the window
   *  message `codesense:routines-changed`. Returns an unsubscribe. */
  function on(type, cb) {
    var full = type.indexOf("codesense:") === 0 ? type : "codesense:" + type;
    function handler(e) {
      if (e.source === window.parent && e.data && e.data.type === full) cb(e.data);
    }
    window.addEventListener("message", handler);
    return function () { window.removeEventListener("message", handler); };
  }
  /** Keep a view live: run render() now, again (coalesced) on each named host event,
   *  and every pollMs while the page is on screen. render may be async; a stale
   *  run's result is the page's to ignore. Returns a stop function.
   *  live(render, {events: ['sessions-changed', 'agents-changed'], pollMs: 30000}) */
  function live(render, opts) {
    opts = opts || {};
    var queued = false, stopped = false, timer = null, offs = [];
    // A host that reports visibility starts the view on its first show (nothing runs before); later shows read nothing.
    var hasVisibility = typeof cs.onVisibility === "function", started = !hasVisibility;
    function run() {
      if (stopped || queued || !started) return;
      queued = true;
      Promise.resolve().then(function () {
        queued = false;
        if (!stopped) return render();
      }).catch(function (e) { if (opts.onError) opts.onError(e); });
    }
    var sessions = (opts.events || []).some(function (t) { return t === 'sessions-changed' || t === 'agents-changed'; });
    if (sessions) {
      // The hub resends its snapshot on a resync (after a show, a stale mark). A snapshot identical to the last one delivers nothing: no
      // render, no read. A delta, or a different snapshot, runs the render.
      var lastSig = null;
      var hubChanged = function (snap) {
        var sig = null;
        try { if (snap && typeof snap === "object" && "snapshot" in snap) sig = JSON.stringify(snap.snapshot); } catch (e) { /* not comparable: deliver */ }
        if (sig !== null && sig === lastSig) return;
        lastSig = sig;
        run();
      };
      cs.subscribe('sessions', {onChange:hubChanged}).catch(function(e){ if(opts.onError)opts.onError(e); });
      offs.push(function(){cs.unsubscribe('sessions',hubChanged);});
    }
    (opts.events || []).filter(function(t){return t !== 'sessions-changed';}).forEach(function (t) { offs.push(on(t, run)); });
    // Tabbing away and back never reloads: the poll timer pauses while hidden and starts again on show, with no read on return. A host
    // event runs the render whenever it arrives, hidden or not.
    if (hasVisibility) {
      offs.push(cs.onVisibility(function (shown) {
        if (shown && !started) { started = true; run(); }
        if (!opts.pollMs) return;
        if (shown && timer === null) timer = setInterval(run, opts.pollMs);
        if (!shown && timer !== null) { clearInterval(timer); timer = null; }
      }));
    }
    run();
    function stop() {
      stopped = true;
      if (timer !== null) clearInterval(timer);
      offs.forEach(function (off) { off(); });
    }
    window.addEventListener("pagehide", stop, { once: true });
    return stop;
  }

  // A Mac with every query slot taken answers busy (mac_busy hosted), and a relay with every slot
  // taken answers rate_limited: the read is asked again after 1, 2, 4, 8 and 8 s (plus up to
  // 250 ms), then the error is the page's. queryAll keeps its own rate_limited backoff.
  var BUSY_RETRY_MS = [1000, 2000, 4000, 8000, 8000];
  var BUSY = /^(mac_)?busy\b/;
  var LIMITED = /^rate_limited\b/;
  function retrying(codes, ask, tries, signal) {
    tries = tries || 0;
    return ask().then(function (answer) {
      if (tries) csCore.noteBusy(false, signal);
      return answer;
    }, function (err) {
      if (!codes.test(String(err && err.message)) || tries >= BUSY_RETRY_MS.length || (signal && signal.cancelled)) {
        if (tries) csCore.noteBusy(false, signal);
        throw err;
      }
      // The page is told once that the Mac is busy (pages say "Waiting for the Mac"), and once when the read is over.
      if (!tries) csCore.noteBusy(true, signal);
      return new Promise(function (ok) {
        setTimeout(ok, BUSY_RETRY_MS[tries] + Math.floor(Math.random() * 250));
      }).then(function () {
        // A cancelled or timed-out read stops here: no ask escapes the budget.
        if (signal && signal.cancelled) throw err;
        return retrying(codes, ask, tries + 1, signal);
      });
    });
  }

  // storyTotals for any number of ids, as one answer: the ids go in as many asks as fit
  // (`fits(chunk)`), three at a time, and the answers merge. Reason indexes are renumbered into
  // one reasons list; ids an answer could not hold (rest) are asked again; source is cache and
  // answered_at the oldest when any part came from the cache.
  function storyTotalsWith(ask, fits, ids) {
    if (!Array.isArray(ids)) return Promise.reject(new Error("invalid_args"));
    var queue = [], chunk = [];
    ids.forEach(function (id) {
      if (chunk.length && !fits(chunk.concat([id]))) { queue.push(chunk); chunk = []; }
      chunk.push(id);
    });
    if (chunk.length) queue.push(chunk);
    var out = { stories: {}, reasons: [], pending: [], indexing: { done: 0, total: 0 } };
    var index = Object.create(null);
    var source = "live", answeredAt = null;
    function add(m) {
      var a = m.answer || {};
      Object.keys(a.stories || {}).forEach(function (id) {
        var story = a.stories[id];
        if (story && story.unavailable) {
          var coded = {};
          Object.keys(story.unavailable).forEach(function (field) {
            var text = (a.reasons || [])[story.unavailable[field]];
            if (typeof text !== "string") return;
            if (!(text in index)) { index[text] = out.reasons.length; out.reasons.push(text); }
            coded[field] = index[text];
          });
          story = Object.assign({}, story, { unavailable: coded });
        }
        out.stories[id] = story;
      });
      out.pending = out.pending.concat(a.pending || []);
      if (a.indexing) out.indexing = a.indexing;
      if (m.source === "cache") source = "cache";
      if (typeof m.answered_at === "number" && (answeredAt === null || m.answered_at < answeredAt)) answeredAt = m.answered_at;
      if (Array.isArray(a.rest) && a.rest.length) queue.push(a.rest);
    }
    function next() {
      if (!queue.length) {
        out.source = source;
        if (answeredAt !== null) out.answered_at = answeredAt;
        return Promise.resolve(out);
      }
      var round = queue.splice(0, 3);
      return Promise.all(round.map(function (part) { return ask(part); })).then(function (answers) {
        answers.forEach(add);
        return next();
      });
    }
    return next();
  }

  // In the app the bridge takes 500 ids an ask.
  // `ask(part)` replaces the default read, so the page's read budget can carry each ask.
  function storyTotals(ids, ask) {
    return storyTotalsWith(ask || function (part) { return query("storyTotals", { storyIds: part }); }, function (chunk) { return chunk.length <= 500; }, ids);
  }

  // Adapt the in-app bridge to the same envelopes consumed by the hosted owner.
  function wireQuery(name, args, signal) {
    return retrying(BUSY, function () {
      return call(name, args).then(function (answer) { return { ok: true, answer: answer }; });
    }, 0, signal);
  }
  // Every page read goes through the page's read budget (CS.budget), then the bridge.
  function query(name, args, opts) { return csCore.ask(name, args, opts); }
  // One page of a paged read: through the budget, with a box so stopping the walk frees its slot.
  function pageQuery(name, args, box) { return csCore.ask(name, args, { box: box }); }

  function subscribe(name, args, everyS, cb) {
    var stopped = false;
    var busy = null;
    var queued = false;
    var revision = 0;
    function deliver(answer, error) {
      if (stopped) return;
      revision++;
      cb(error ? { ok: false, error: String(error.message || error) } : { ok: true, answer: answer });
    }
    function poll() {
      if (stopped) return;
      if (busy) { queued = true; return busy; }
      var stamp = revision;
      busy = query(name, args).then(function (m) {
        if (stamp === revision) deliver(m.answer);
      }, function (error) {
        if (stamp === revision) deliver(null, error);
      }).finally(function () {
        busy = null;
        if (queued && !stopped) { queued = false; poll(); }
      });
      return busy;
    }
    var off;
    if (name === "queryContext") {
      if (typeof cs.subscribeContext !== "function") throw new TypeError("codesense.subscribeContext is not available");
      var offContext = cs.subscribeContext(args, deliver);
      var timer = null;
      // The poll pauses while hidden and starts again on show: showing never reads.
      var offVisibility = typeof cs.onVisibility === "function" ? cs.onVisibility(function (shown) {
        if (timer !== null) { clearInterval(timer); timer = null; }
        if (shown && !stopped) timer = setInterval(poll, everyS * 1000);
      }) : null;
      off = function () {
        if (typeof offContext === "function") offContext();
        if (typeof offVisibility === "function") offVisibility();
        if (timer !== null) clearInterval(timer);
      };
    } else {
      off = live(poll, { events: name === "sessionUsage" ? ["sessions-changed"] : [], pollMs: everyS * 1000, onError: function (error) { deliver(null, error); } });
    }
    return function () { stopped = true; queued = false; off(); };
  }

  // Paging state belongs to one traversal owner; head updates never imply deletion.
  var QUERYALL_MAX_ROWS = 5000;
  var QUERYALL_LIMIT = 100;

  var PAGED_LISTS = {
    sessionUsage: { rows: "sessions", id: function (r) { return JSON.stringify([r.cli, r.sessionId]); }, cursor: { arg: "cursor", next: "nextCursor" }, total: "total" },
    queryContext: { rows: "rows", id: function (r) { return r.id != null ? r.id : r.path; }, cursor: { arg: "cursor", next: "nextCursor" }, total: "total" },
    storyChanges: { rows: "commits", id: "sha", cursor: { arg: "cursor", next: "nextCursor" }, total: "total" }
  };

  function pagingValue(answer, selector) {
    if (typeof selector === "function") return selector(answer);
    return selector.split(".").reduce(function (value, key) { return value == null ? undefined : value[key]; }, answer);
  }

  function pagingAnswer(m, shape) {
    if (!m || m.ok === false) throw new Error(m && m.error || "failed");
    var a = m.answer;
    if (!a || typeof a !== "object") throw new Error("failed");
    if (a.refused) throw new Error(String(a.refused));
    var rows = pagingValue(a, shape.rows);
    var out = Object.assign({}, a);
    // Remove the selected array from metadata, preserving other facts and nested siblings.
    if (typeof shape.rows === "string" && shape.rows !== "rows") {
      var path = shape.rows.split(".");
      var parent = out;
      for (var i = 0; i < path.length - 1 && parent[path[i]]; i++) {
        parent[path[i]] = Object.assign({}, parent[path[i]]);
        parent = parent[path[i]];
      }
      delete parent[path[path.length - 1]];
    } else if (typeof shape.rows === "function") {
      Object.keys(out).forEach(function (key) { if (out[key] === rows) delete out[key]; });
    }
    out.rows = Array.isArray(rows) ? rows : [];
    out.total = pagingValue(a, shape.total);
    out.nextCursor = pagingValue(a, shape.cursor.next);
    ["source", "answered_at", "server_time"].forEach(function (key) {
      if (m[key] !== undefined) out[key] = m[key];
    });
    return out;
  }

  // A walk whose cursor answers stale_cursor (its index snapshot is gone) starts over this often.
  var STALE_CURSOR_RESTARTS = 3;

  function pagingOwner(name, args, opts, notify) {
    opts = opts || {};
    var defaults = PAGED_LISTS[name];
    var shape = Object.assign({
      rows: function (a) { return Array.isArray(a.sessions) ? a.sessions : a.rows; },
      id: function (r) { return r.id != null ? r.id : r.path; },
      cursor: { arg: "cursor", next: "nextCursor" }, total: "total"
    }, defaults);
    ["rows", "id", "total"].forEach(function (key) {
      if (opts[key] !== undefined) shape[key] = opts[key];
    });
    shape.cursor = Object.assign({}, shape.cursor);
    ["arg", "next"].forEach(function (key) {
      if (opts.cursor && opts.cursor[key] !== undefined) shape.cursor[key] = opts.cursor[key];
    });
    // Snapshot nested filters too: later caller edits must not change a traversal.
    var base = JSON.parse(JSON.stringify(args || {}));
    var limit = opts.limit || base.limit || QUERYALL_LIMIT;
    var cap = Math.min(opts.maxRows || QUERYALL_MAX_ROWS, QUERYALL_MAX_ROWS);
    base.limit = limit;
    var visible = new Map();
    var fresh = null;
    var cursor = base[shape.cursor.arg] || null;
    var total = null;
    var metadata = {};
    var pending = null;
    var stopped = false;
    var loading = false;
    var error = null;
    var headError = null;
    var initialized = false;
    var revision = 0;
    var staleRestarted = false;
    var staleCursorRestarts = 0;
    var waitingForHead = false;
    var recoveries = 2;
    var lastWalk = Date.now();
    var queuedHead = null;
    var lastHeadBody = null;
    var cancels = new Set();
    var stopSub = null;
    var restMs = (opts.restEveryS || 60) * 1000;
    var paging = !!defaults || opts.rows !== undefined;

    function merge(target, rows, overwrite) {
      rows.forEach(function (row) {
        var id = pagingValue(row, shape.id);
        if (target.has(id)) { if (overwrite) target.set(id, row); }
        else if (target.size < cap) target.set(id, row);
      });
    }
    function complete(collection) { return total !== null && collection.size === total && !cursor; }
    function result() {
      return Object.assign({}, metadata, {
        rows: Array.from(visible.values()), total: total, returned: visible.size,
        complete: initialized && !waitingForHead && !fresh && complete(visible),
        truncated: !(initialized && !waitingForHead && !fresh && complete(visible)) &&
          (!!cursor || (fresh || visible).size >= cap || recoveries > 0),
        canLoadMore: !waitingForHead && !!cursor && (fresh || visible).size < cap,
        waitingForHead: waitingForHead,
        atCeiling: (fresh || visible).size >= cap,
        loading: loading, error: error || headError
      });
    }
    function emit() { if (!stopped && notify) notify(result()); }
    function summary(a) {
      if (typeof a.total === "number") total = a.total;
      Object.keys(a).forEach(function (k) {
        if (k !== shape.cursor.next && k !== shape.total && ["rows", "total", "nextCursor", "hasMore", "next_before", "returned", "complete", "truncated", "loading", "error"].indexOf(k) < 0) metadata[k] = a[k];
      });
    }
    function accept(a, stamp) {
      var newer = stamp === revision;
      if (newer) summary(a);
      merge(visible, a.rows, newer);
      if (fresh) merge(fresh, a.rows, newer);
      cursor = paging && a.nextCursor || null;
      initialized = true;
      error = null;
      if (fresh && newer && complete(fresh)) { visible = fresh; fresh = null; }
      // A fenced final manual page can finish coverage without replacing newer rows.
      if (opts.manual === true && !newer && complete(visible)) fresh = null;
    }
    function cancellable(start) {
      return new Promise(function (resolve, reject) {
        var cleanup = function () {};
        function cancel() { cleanup(); reject(new Error("stopped")); }
        cancels.add(cancel);
        start(function (value) { cancels.delete(cancel); resolve(value); }, function (err) { cancels.delete(cancel); reject(err); }, function (fn) { cleanup = fn; });
      });
    }
    function ask(pageArgs, retry) {
      if (stopped) return Promise.reject(new Error("stopped"));
      return cancellable(function (resolve, reject, cleanup) {
        // Stopping cancels the budgeted read, which frees its slot.
        var box = {};
        cleanup(function () { if (box.cancel) box.cancel(); });
        pageQuery(name, pageArgs, box).then(resolve, reject);
      }).then(function (m) { return pagingAnswer(m, shape); }).catch(function (err) {
        if (stopped) throw new Error("stopped");
        if (err.message !== "rate_limited" || retry >= 6) throw err;
        return cancellable(function (resolve, reject, cleanup) {
          var timer = setTimeout(resolve, 5000 * (retry + 1));
          cleanup(function () { clearTimeout(timer); });
        }).then(function () { return ask(pageArgs, retry + 1); });
      });
    }
    function begin(a) {
      queuedHead = null;
      fresh = new Map();
      cursor = null;
      recoveries = 2;
      staleRestarted = false;
      staleCursorRestarts = 0;
      lastWalk = Date.now();
      if (a) accept(a, revision);
    }
    function canContinue(auto) {
      var collection = fresh || visible;
      if (collection.size >= cap || !paging) return false;
      if (cursor) return true;
      if (auto && total !== null && collection.size !== total && recoveries > 0) {
        recoveries--;
        return true;
      }
      return false;
    }
    function drive(auto, forceFirst) {
      if (stopped) return Promise.reject(new Error("stopped"));
      if (pending) return pending;
      if (waitingForHead) return Promise.resolve(result());
      loading = true;
      error = null;
      emit();
      async function run() {
        var first = forceFirst;
        var stamp;
        while (!stopped) {
          // Reconcile before deciding whether to ask or capturing request state.
          if (queuedHead) { begin(queuedHead); first = false; }
          if (!first) {
            if (stamp !== undefined && (!auto || (!cursor && stamp !== revision))) break;
            if (!canContinue(auto)) break;
          }
          first = false;
          var pageArgs = Object.assign({}, base);
          delete pageArgs[shape.cursor.arg];
          if (cursor) pageArgs[shape.cursor.arg] = cursor;
          stamp = revision;
          try {
            var a = await ask(pageArgs, 0);
            if (stopped) throw new Error("stopped");
            accept(a, stamp);
            emit();
          } catch (err) {
            var staleCursor = /^stale_cursor\b/.test(err.message);
            if (!stopped && pageArgs[shape.cursor.arg] && staleCursor && staleCursorRestarts < STALE_CURSOR_RESTARTS) {
              // The snapshot this walk paged is gone: walk again from the head, silently,
              // and only that new walk replaces the rows (none of the old walk's are kept).
              staleCursorRestarts++;
              cursor = null;
              fresh = new Map();
              first = true;
              continue;
            }
            if (!stopped && pageArgs[shape.cursor.arg] && !staleCursor && !staleRestarted && /stale|cursor|query_invalid/i.test(err.message)) {
              staleRestarted = true;
              cursor = null;
              if (opts.manual === true) {
                waitingForHead = true;
                throw err;
              }
              // Keep collected rows for a moving-window recovery, with the same limit.
              first = true;
              continue;
            }
            throw err;
          }
        }
      }
      pending = run().then(function () {
        loading = false;
        pending = null;
        emit();
        return result();
      }, function (err) {
        loading = false;
        pending = null;
        if (!stopped) { error = err; emit(); }
        throw err;
      });
      return pending;
    }
    function background(promise) {
      // Errors were delivered by the owner; automatic work has no Promise caller.
      promise.catch(function () {});
    }
    function head(m) {
      if (stopped) return;
      var a;
      try { a = pagingAnswer(m, shape); }
      catch (err) { headError = err; loading = !!pending; emit(); return; }
      headError = null;
      var wasWaiting = waitingForHead;
      waitingForHead = false;
      if (wasWaiting) staleRestarted = false;
      var body = JSON.stringify(m.answer);
      if (initialized && body === lastHeadBody) {
        error = null;
        if (queuedHead) queuedHead = a;
        // Transport freshness can change while an old cached head body repeats.
        ["source", "answered_at", "server_time"].forEach(function (key) {
          if (a[key] !== undefined) metadata[key] = a[key];
        });
        if (opts.manual !== true && !pending && Date.now() - lastWalk >= restMs) {
          begin();
          background(drive(true, true));
          return;
        }
        if (!cursor && ((total !== null && visible.size < total) || (wasWaiting && total === null))) cursor = paging && a.nextCursor || null;
        if (opts.manual === true && !pending && complete(visible)) fresh = null;
        emit();
        if (opts.manual !== true && !pending && cursor && (fresh || visible).size < cap) background(drive(true, false));
        return;
      }
      lastHeadBody = body;
      revision++;
      if (opts.manual === true && initialized) {
        summary(a);
        merge(visible, a.rows, true);
        if (fresh) merge(fresh, a.rows, true);
        // Manual heads update coverage without reopening a completed traversal.
        if (!cursor && !pending && ((total !== null && visible.size < total) || (wasWaiting && total === null))) cursor = paging && a.nextCursor || null;
        if (!pending && complete(visible)) fresh = null;
        error = null;
        if (!pending) loading = false;
        emit();
        return;
      }
      var previous = total;
      var start = !initialized || (typeof a.total === "number" && a.total !== previous) || Date.now() - lastWalk >= restMs;
      // Rearm an exhausted incomplete traversal without refetching healthy histories.
      if (!start && !pending && !cursor && (fresh || !complete(visible))) start = true;
      if (start && !pending) begin(a);
      else {
        summary(a);
        merge(visible, a.rows, true);
        if (fresh) merge(fresh, a.rows, true);
        error = null;
        if (start || queuedHead) queuedHead = a;
      }
      if (!pending) loading = false;
      emit();
      if (!pending && opts.manual !== true && canContinue(false)) background(drive(true, false));
      else if (!pending && opts.manual !== true && start && total !== null && !complete(fresh || visible)) background(drive(true, false));
    }
    function refresh() {
      if (stopped) return Promise.reject(new Error("stopped"));
      if (pending) return pending.then(refresh);
      if (waitingForHead) return Promise.resolve(result());
      begin();
      return drive(opts.manual !== true, true);
    }
    function stop() {
      if (stopped) return;
      stopped = true;
      cancels.forEach(function (cancel) { cancel(); });
      cancels.clear();
      if (stopSub) stopSub();
    }
    stop.loadMore = function () {
      if (!initialized) return drive(false, true);
      return drive(false, false);
    };
    stop.refresh = refresh;
    return {
      query: function () { return drive(true, true); },
      subscribe: function (everyS) { loading = true; emit(); stopSub = subscribe(name, base, everyS, head); return stop; }
    };
  }

  function queryAll(name, args, opts) {
    opts = opts || {};
    return pagingOwner(name, args, opts, opts.onPage).query();
  }

  function subscribeAll(name, args, everyS, cb, opts) {
    return pagingOwner(name, args, opts, cb).subscribe(everyS);
  }

  // A small list component that consumes a queryAll promise or a subscribeAll
  // callback and shows the approved status row.
  var PARTIAL_LIST_STYLE = [
    ":where(.cs-partial-list) { display: flex; flex-direction: column; gap: var(--cs-space-2, 8px); color-scheme: var(--cs-color-scheme, inherit); }",
    ":where(.cs-partial-list > .cs-partial-list-rows) { display: flex; flex-direction: column; }",
    ":where(.cs-partial-list > .cs-partial-list-status) { display: flex; align-items: center; justify-content: space-between; gap: var(--cs-space-2, 8px); flex-wrap: wrap; min-width: 0; font-family: var(--cs-font-sans, system-ui, -apple-system, sans-serif); font-size: var(--cs-text-base, 13px); line-height: var(--cs-leading, 1.5); }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-text) { font-size: var(--cs-text-sm, 12px); color: var(--cs-muted-fg, light-dark(#636c76, #8b949e)); }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more) { box-sizing: border-box; flex: none; display: inline-flex; align-items: center; justify-content: center; gap: 6px; height: 22px; padding: 0 var(--cs-space-2, 8px); font: inherit; font-size: var(--cs-text-xs, 11px); font-weight: 500; line-height: 1; color: var(--cs-fg, light-dark(#1f2328, #e6edf3)); background: var(--cs-surface, light-dark(#f7f8fa, #262b32)); border: 1px solid var(--cs-border, light-dark(#d0d7de, #30363d)); border-radius: var(--cs-radius, 6px); cursor: pointer; white-space: nowrap; }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:hover) { background: var(--cs-hover, color-mix(in srgb, var(--cs-fg, light-dark(#1f2328, #e6edf3)) 6%, transparent)); }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:focus-visible) { outline: 2px solid var(--cs-accent, light-dark(#0969da, #58a6ff)); outline-offset: 1px; }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:disabled) { opacity: 0.5; cursor: default; }",
    ":where(.cs-partial-list > .cs-partial-list-status) > .cs-partial-list-more[hidden] { display: none; }"
  ].join("\n");

  function installPartialListStyle() {
    if (document.getElementById("cs-partial-list-style")) return;
    var style = document.createElement("style");
    style.id = "cs-partial-list-style";
    style.textContent = PARTIAL_LIST_STYLE;
    document.head.insertBefore(style, document.head.firstChild);
  }

  function partialList(opts) {
    opts = opts || {};
    var bound = opts.status && opts.button;
    var container = opts.container;
    if (!container && !bound) throw new TypeError("partialList: pass container or status and button");
    if (!bound && typeof opts.render !== "function") throw new TypeError("partialList: pass render(row) -> element");
    var root = bound ? container || opts.status.parentElement : document.createElement("div");
    var rowsBox = null;
    var textEl = bound ? opts.status : document.createElement("span");
    var moreBtn = bound ? opts.button : document.createElement("button");
    if (!bound) {
      installPartialListStyle();
      root.className = "cs-partial-list";
      rowsBox = document.createElement("div");
      rowsBox.className = "cs-partial-list-rows";
      textEl.className = "cs-partial-list-text cs-small cs-muted";
      moreBtn.type = "button";
      moreBtn.className = "cs-partial-list-more cs-btn is-sm";
      var statusRow = document.createElement("div");
      statusRow.className = "cs-partial-list-status cs-row";
      statusRow.appendChild(textEl);
      statusRow.appendChild(moreBtn);
      root.appendChild(rowsBox);
      root.appendChild(statusRow);
      container.appendChild(root);
    }
    textEl.setAttribute("aria-live", "polite");
    var current = { rows: [], total: null, complete: false, truncated: false, returned: 0, loading: true };
    var handle = null;
    var stopped = false;
    var busy = null;
    var renderedRows = null;
    var settleAction = null;

    function update() {
      if (stopped) return;
      if (rowsBox) {
        var text = JSON.stringify(current.rows);
        if (text !== renderedRows) {
          renderedRows = text;
          rowsBox.innerHTML = "";
          current.rows.forEach(function (row, i) { rowsBox.appendChild(opts.render(row, i)); });
        }
      }
      var msg;
      if (current.loading && current.returned === 0) msg = "Loading…";
      else if (current.complete) msg = "Showing all " + current.returned;
      else if (typeof current.total !== "number") msg = "Loaded " + current.returned + " · total unknown";
      else if (current.truncated) msg = "Showing " + current.returned + " of " + current.total;
      else msg = "Latest " + current.returned + " of " + current.total;
      if (current.atCeiling && !current.complete) msg += " · row limit reached";
      if (current.loading && current.returned > 0) msg += " · loading more…";
      if (current.waitingForHead) msg += " · Waiting for fresh data…";
      else if (current.error) msg += " · Could not load: " + String(current.error.message || current.error) + ". Try again.";
      textEl.textContent = msg;
      var available = current.canLoadMore === undefined ? current.truncated : current.canLoadMore;
      moreBtn.textContent = current.waitingForHead ? "Waiting…" : busy || current.loading ? "Loading…" : current.error ? "Retry" : "Load more";
      moreBtn.disabled = !!busy || current.loading || current.waitingForHead || (!available && !current.error);
      moreBtn.hidden = !current.error && (current.complete || !available);
    }
    function receive(r) {
      if (stopped) return;
      current = r;
      update();
      if (typeof opts.onResult === "function") opts.onResult(r);
    }
    function run(refresh) {
      if (stopped || current.waitingForHead) return Promise.resolve(current);
      if (busy) return busy;
      var method = handle && (refresh ? handle.refresh : handle.loadMore);
      if (typeof method !== "function") method = refresh ? opts.query : opts.loadMore || opts.query;
      if (typeof method !== "function") return Promise.resolve(current);
      // Queue invocation so the click is locked before a synchronous subscriber update.
      var stoppedAction = new Promise(function (resolve) { settleAction = resolve; });
      busy = Promise.race([stoppedAction, Promise.resolve().then(function () { if (!stopped) return method(); }).then(function (r) {
        if (r && r.rows) receive(r);
        return current;
      }).catch(function (err) {
        if (!stopped) receive(Object.assign({}, current, { loading: false, error: err }));
        return current;
      })]).finally(function () { busy = null; settleAction = null; update(); });
      update();
      return busy;
    }
    function click() { run(!!current.error && current.canLoadMore === false); }
    moreBtn.addEventListener("click", click);
    if (typeof opts.subscribe === "function") handle = opts.subscribe(receive);
    else if (typeof opts.query === "function") run(true);
    update();
    return {
      el: root,
      loadMore: function () { return run(false); },
      refresh: function () { return run(true); },
      stop: function () {
        if (stopped) return;
        stopped = true;
        moreBtn.removeEventListener("click", click);
        moreBtn.disabled = true;
        if (settleAction) settleAction(current);
        if (typeof handle === "function") handle();
      }
    };
  }

  // ── DAG layout ─────────────────────────────────────────────────────────
  /**
   * Lay out a directed graph in layers (a small Sugiyama): break cycles by
   * reversing DFS back edges, layer by longest path from the sources, route
   * edges that skip layers through virtual points, order each layer by
   * barycenter sweeps (keeping the order with the fewest crossings), then
   * place nodes near the average of their neighbours.
   *
   * nodes: [{id, width?, height?, ...}] or ids. edges: [{from, to, ...}], `from`
   * drawn above (TB) or left of (LR) `to`. Unknown endpoints and self-loops are
   * skipped; duplicate edges collapse.
   * opts: {direction: 'TB'|'LR', nodeWidth: 160, nodeHeight: 44, nodeSep: 24,
   *        rankSep: 56, margin: 16, iterations: 8}
   * Returns {width, height, direction, crossings,
   *   nodes: [{id, x, y, cx, cy, width, height, layer, order, data}],  // x,y top-left
   *   edges: [{from, to, points: [{x, y}], reversed, data}],          // points from → to
   *   reversed: [{from, to}], skipped: [edge]}
   */
  function layoutDag(nodesIn, edgesIn, opts) {
    return opts && typeof opts.group === "function" ? layoutGroups(nodesIn, edgesIn, opts) : layoutPlain(nodesIn, edgesIn, opts);
  }
  function layoutPlain(nodesIn, edgesIn, opts) {
    opts = opts || {};
    var lr = opts.direction === "LR";
    var nodeW = opts.nodeWidth || 160;
    var nodeH = opts.nodeHeight || 44;
    var nodeSep = opts.nodeSep != null ? opts.nodeSep : 24;
    var rankSep = opts.rankSep != null ? opts.rankSep : 56;
    var margin = opts.margin != null ? opts.margin : 16;
    var iterations = opts.iterations != null ? opts.iterations : 8;

    // Nodes. `cross` is the size across a layer, `along` the size along the rank axis.
    var nodes = [];
    var byId = Object.create(null);
    (nodesIn || []).forEach(function (n, i) {
      var isObj = n !== null && typeof n === "object";
      var id = String(isObj ? n.id : n);
      if (byId[id]) return;
      var w = (isObj && n.width) || nodeW;
      var ht = (isObj && n.height) || nodeH;
      var node = { id: id, data: n, index: i, w: w, h: ht, cross: lr ? ht : w, along: lr ? w : ht, dummy: false, preds: [], succs: [] };
      byId[id] = node;
      nodes.push(node);
    });

    // Edges.
    var edges = [];
    var skipped = [];
    var seen = Object.create(null);
    (edgesIn || []).forEach(function (e) {
      var f = e && e.from != null ? String(e.from) : null;
      var t = e && e.to != null ? String(e.to) : null;
      if (f === null || t === null || !byId[f] || !byId[t] || f === t) { skipped.push(e); return; }
      var key = f + "\u0000" + t;
      if (seen[key]) return;
      seen[key] = true;
      edges.push({ from: f, to: t, data: e, reversed: false, src: f, dst: t });
    });

    if (!nodes.length) {
      return { direction: lr ? "LR" : "TB", width: 2 * margin, height: 2 * margin, crossings: 0, nodes: [], edges: [], reversed: [], skipped: skipped };
    }

    // 1. Break cycles: reverse each DFS back edge (iterative, input order).
    var outEdges = Object.create(null);
    nodes.forEach(function (n) { outEdges[n.id] = []; });
    edges.forEach(function (e) { outEdges[e.from].push(e); });
    var mark = Object.create(null); // 1 on stack, 2 done
    nodes.forEach(function (root) {
      if (mark[root.id]) return;
      var stack = [{ id: root.id, i: 0 }];
      mark[root.id] = 1;
      while (stack.length) {
        var top = stack[stack.length - 1];
        var outs = outEdges[top.id];
        if (top.i >= outs.length) { mark[top.id] = 2; stack.pop(); continue; }
        var e = outs[top.i++];
        if (mark[e.to] === 1) { e.reversed = true; e.src = e.to; e.dst = e.from; }
        else if (!mark[e.to]) { mark[e.to] = 1; stack.push({ id: e.to, i: 0 }); }
      }
    });
    edges.forEach(function (e) { byId[e.src].succs.push(e.dst); byId[e.dst].preds.push(e.src); });

    // 2. Layers: longest path from the sources (Kahn order).
    var indeg = Object.create(null);
    nodes.forEach(function (n) { indeg[n.id] = n.preds.length; n.layer = 0; });
    var queue = nodes.filter(function (n) { return indeg[n.id] === 0; });
    for (var qi = 0; qi < queue.length; qi++) {
      var u = queue[qi];
      u.succs.forEach(function (vid) {
        var v = byId[vid];
        v.layer = Math.max(v.layer, u.layer + 1);
        if (--indeg[vid] === 0) queue.push(v);
      });
    }
    var layerCount = 0;
    nodes.forEach(function (n) { layerCount = Math.max(layerCount, n.layer + 1); });

    // 3. Virtual nodes for edges that skip layers; `segs` are the adjacent-layer links.
    var all = nodes.slice();
    var segs = [];
    edges.forEach(function (e, ei) {
      var a = byId[e.src], b = byId[e.dst];
      var chain = [a];
      for (var l = a.layer + 1; l < b.layer; l++) {
        var d = { id: "\u0000d" + ei + ":" + l, dummy: true, layer: l, cross: 0, along: 0, w: 0, h: 0, index: 1e9 + ei };
        all.push(d);
        chain.push(d);
      }
      chain.push(b);
      e.chain = chain;
      for (var k = 0; k + 1 < chain.length; k++) segs.push([chain[k], chain[k + 1]]);
    });
    all.forEach(function (n) { n.up = []; n.down = []; });
    segs.forEach(function (sg) { sg[0].down.push(sg[1]); sg[1].up.push(sg[0]); });

    var layers = [];
    for (var li = 0; li < layerCount; li++) layers.push([]);
    // Initial order: layer 0 in input order, then each node after its first parent.
    nodes.filter(function (n) { return n.layer === 0; }).forEach(function (n) { layers[0].push(n); });
    layers[0].forEach(function (n, i) { n.pos = i; });
    for (var lj = 1; lj < layerCount; lj++) {
      var row = all.filter(function (n) { return n.layer === lj; });
      row.forEach(function (n) {
        n.key0 = n.up.length ? Math.min.apply(null, n.up.map(function (p) { return p.pos; })) : Infinity;
      });
      row.sort(function (a, b) { return a.key0 - b.key0 || a.index - b.index; });
      row.forEach(function (n, i) { n.pos = i; });
      layers[lj] = row;
    }

    function crossingsBetween(upper) {
      var c = 0;
      var links = [];
      upper.forEach(function (u) { u.down.forEach(function (v) { links.push([u.pos, v.pos]); }); });
      for (var i = 0; i < links.length; i++)
        for (var j = i + 1; j < links.length; j++)
          if ((links[i][0] - links[j][0]) * (links[i][1] - links[j][1]) < 0) c++;
      return c;
    }
    function totalCrossings() {
      var c = 0;
      for (var i = 0; i + 1 < layers.length; i++) c += crossingsBetween(layers[i]);
      return c;
    }
    function sortByBarycenter(row, side) {
      row.forEach(function (n) {
        var nb = n[side];
        n.bc = nb.length ? nb.reduce(function (sum, m) { return sum + m.pos; }, 0) / nb.length : n.pos;
      });
      row.sort(function (a, b) { return a.bc - b.bc || a.pos - b.pos; });
      row.forEach(function (n, i) { n.pos = i; });
    }
    function snapshot() { return layers.map(function (r) { return r.slice(); }); }
    var best = snapshot();
    var bestC = totalCrossings();
    for (var it = 0; it < iterations && bestC > 0; it++) {
      for (var d1 = 1; d1 < layers.length; d1++) sortByBarycenter(layers[d1], "up");
      for (var d2 = layers.length - 2; d2 >= 0; d2--) sortByBarycenter(layers[d2], "down");
      var c = totalCrossings();
      if (c < bestC) { bestC = c; best = snapshot(); }
    }
    layers = best;
    layers.forEach(function (r) { r.forEach(function (n, i) { n.pos = i; }); });

    // 4. Coordinates across each layer: pack, then pull toward the neighbours.
    function gap(a, b) {
      var sep = a.dummy || b.dummy ? nodeSep / 2 : nodeSep;
      return a.cross / 2 + sep + b.cross / 2;
    }
    layers.forEach(function (r) {
      var x = 0;
      r.forEach(function (n, i) { if (i) x += gap(r[i - 1], n); n.c = x; });
    });
    function place(r, side) {
      if (!r.length) return;
      var want = r.map(function (n) {
        var nb = n[side];
        return nb.length ? nb.reduce(function (sum, m) { return sum + m.c; }, 0) / nb.length : n.c;
      });
      var got = want.slice();
      for (var i = 1; i < r.length; i++) got[i] = Math.max(got[i], got[i - 1] + gap(r[i - 1], r[i]));
      var shift = 0;
      for (var j = 0; j < r.length; j++) shift += want[j] - got[j];
      shift /= r.length;
      r.forEach(function (n, k) { n.c = got[k] + shift; });
    }
    for (var pass = 0; pass < 4; pass++) {
      for (var p1 = 1; p1 < layers.length; p1++) place(layers[p1], "up");
      for (var p2 = layers.length - 2; p2 >= 0; p2--) place(layers[p2], "down");
    }
    var minC = Infinity, maxC = -Infinity;
    all.forEach(function (n) { minC = Math.min(minC, n.c - n.cross / 2); maxC = Math.max(maxC, n.c + n.cross / 2); });
    if (!all.length) { minC = 0; maxC = 0; }

    // Along the rank axis: each layer as deep as its largest node.
    var rankAt = [];
    var at = margin;
    layers.forEach(function (r, i) {
      var depth = r.reduce(function (m, n) { return Math.max(m, n.along); }, 0);
      rankAt[i] = { start: at, mid: at + depth / 2, depth: depth };
      at += depth + rankSep;
    });
    var alongTotal = layers.length ? at - rankSep + margin : 2 * margin;
    function pt(crossC, alongC) {
      return lr ? { x: alongC, y: crossC } : { x: crossC, y: alongC };
    }
    all.forEach(function (n) { n.cc = n.c - minC + margin; n.ac = rankAt[n.layer].mid; });

    var outNodes = nodes.map(function (n) {
      var center = pt(n.cc, n.ac);
      return {
        id: n.id, data: n.data, layer: n.layer, order: n.pos, width: n.w, height: n.h,
        cx: center.x, cy: center.y, x: center.x - n.w / 2, y: center.y - n.h / 2,
      };
    });
    var outEdges2 = edges.map(function (e) {
      var pts = e.chain.map(function (n, i) {
        var a = n.ac;
        if (i === 0) a = n.ac + n.along / 2;
        else if (i === e.chain.length - 1) a = n.ac - n.along / 2;
        return pt(n.cc, a);
      });
      if (e.reversed) pts.reverse();
      return { from: e.from, to: e.to, points: pts, reversed: e.reversed, data: e.data };
    });
    var crossTotal = maxC - minC + 2 * margin;
    return {
      direction: lr ? "LR" : "TB",
      width: lr ? alongTotal : crossTotal,
      height: lr ? crossTotal : alongTotal,
      crossings: bestC,
      nodes: outNodes,
      edges: outEdges2,
      reversed: edges.filter(function (e) { return e.reversed; }).map(function (e) { return { from: e.from, to: e.to }; }),
      skipped: skipped,
    };
  }

  /**
   * layoutDag with opts.group = (node) => key: each group (an epic, a machine) is
   * laid out on its own inside a framed cluster, and the clusters are packed in
   * rows no wider than opts.maxWidth. Edges inside a cluster route as usual; an
   * edge between clusters is a direct curve marked `cross: true`. Adds
   * groups: [{key, x, y, width, height, count}] (frame boxes; the label goes in the
   * top opts.groupHeader px). Groups order so a group others depend on comes first.
   */
  function layoutGroups(nodesIn, edgesIn, opts) {
    var lr = opts.direction === "LR";
    var margin = opts.margin != null ? opts.margin : 16;
    var pad = opts.groupPadding != null ? opts.groupPadding : 14;
    var header = opts.groupHeader != null ? opts.groupHeader : 30;
    var gap = opts.groupGap != null ? opts.groupGap : 28;
    var maxWidth = opts.maxWidth || Infinity;
    var groups = [];
    var byKey = Object.create(null);
    var groupOf = Object.create(null);
    var seenNode = Object.create(null);
    (nodesIn || []).forEach(function (n) {
      var id = String(n !== null && typeof n === "object" ? n.id : n);
      if (seenNode[id]) return;
      seenNode[id] = true;
      var key = String(opts.group(n) == null ? "" : opts.group(n));
      if (!byKey[key]) { byKey[key] = { key: key, nodes: [], edges: [], deps: 0, index: groups.length }; groups.push(byKey[key]); }
      byKey[key].nodes.push(n);
      groupOf[id] = key;
    });
    var cross = [];
    var skipped = [];
    (edgesIn || []).forEach(function (e) {
      var f = e && e.from != null ? String(e.from) : null;
      var t = e && e.to != null ? String(e.to) : null;
      if (f === null || t === null || !(f in groupOf) || !(t in groupOf) || f === t) { skipped.push(e); return; }
      if (groupOf[f] === groupOf[t]) byKey[groupOf[f]].edges.push(e);
      else { cross.push(e); byKey[groupOf[t]].deps++; }
    });
    groups.sort(function (a, b) { return a.deps - b.deps || a.index - b.index; });
    var sub = Object.assign({}, opts, { group: undefined, margin: 0 });
    var x = margin, y = margin, rowH = 0, width = 0, crossings = 0, reversed = [];
    var nodes = [], edges = [], frames = [];
    groups.forEach(function (g) {
      var l = layoutPlain(g.nodes, g.edges, sub);
      var w = l.width + 2 * pad, ht = l.height + header + pad;
      if (x > margin && x + w > maxWidth - margin) { x = margin; y += rowH + gap; rowH = 0; }
      var ox = x + pad, oy = y + header;
      l.nodes.forEach(function (n) {
        n.x += ox; n.y += oy; n.cx += ox; n.cy += oy; n.group = g.key;
        nodes.push(n);
      });
      l.edges.forEach(function (e) {
        e.points.forEach(function (p) { p.x += ox; p.y += oy; });
        e.cross = false;
        edges.push(e);
      });
      crossings += l.crossings;
      reversed = reversed.concat(l.reversed);
      frames.push({ key: g.key, x: x, y: y, width: w, height: ht, count: g.nodes.length });
      x += w + gap;
      rowH = Math.max(rowH, ht);
      width = Math.max(width, x - gap + margin);
    });
    var at = Object.create(null);
    nodes.forEach(function (n) { at[n.id] = n; });
    cross.forEach(function (e) {
      var a = at[String(e.from)], b = at[String(e.to)];
      var p, q;
      if (lr) {
        var right = b.cx >= a.cx;
        p = { x: right ? a.x + a.width : a.x, y: a.cy };
        q = { x: right ? b.x : b.x + b.width, y: b.cy };
      } else {
        var down = b.cy >= a.cy;
        p = { x: a.cx, y: down ? a.y + a.height : a.y };
        q = { x: b.cx, y: down ? b.y : b.y + b.height };
      }
      edges.push({ from: String(e.from), to: String(e.to), points: [p, q], reversed: false, cross: true, data: e });
    });
    return {
      direction: lr ? "LR" : "TB",
      width: groups.length ? width : 2 * margin,
      height: groups.length ? y + rowH + margin : 2 * margin,
      crossings: crossings,
      nodes: nodes,
      edges: edges,
      groups: frames,
      reversed: reversed,
      skipped: skipped,
    };
  }

  /** An SVG path `d` through an edge's points, curving along the rank axis. */
  function edgePath(points, direction) {
    if (!points || !points.length) return "";
    var lr = direction === "LR";
    var d = "M" + points[0].x + "," + points[0].y;
    for (var i = 1; i < points.length; i++) {
      var p = points[i - 1], q = points[i];
      if (lr) {
        var mx = (p.x + q.x) / 2;
        d += " C" + mx + "," + p.y + " " + mx + "," + q.y + " " + q.x + "," + q.y;
      } else {
        var my = (p.y + q.y) / 2;
        d += " C" + p.x + "," + my + " " + q.x + "," + my + " " + q.x + "," + q.y;
      }
    }
    return d;
  }

  /** An arrowhead <marker> for edges; `active` draws in the accent colour. */
  function arrowMarker(id, active) {
    return s("marker", { id: id, viewBox: "0 0 10 10", refX: 9, refY: 5, markerWidth: 7, markerHeight: 7, orient: "auto-start-reverse" },
      s("path", { d: "M0,0 L10,5 L0,10 z", class: "cs-dag-arrow" + (active ? " is-active" : "") }));
  }
  // Nodes are HTML boxes positioned over an SVG of edges (not SVG foreignObject,
  // which WebKit, the app's engine, draws wrongly under a transform).
  function stage(layout) {
    var el = h("div", { class: "cs-dag-stage", style: { width: layout.width + "px", height: layout.height + "px" } });
    var svg = s("svg", { class: "cs-dag-lines", width: layout.width, height: layout.height, viewBox: "0 0 " + layout.width + " " + layout.height });
    var layer = h("div", { class: "cs-dag-nodes" });
    el.append(svg, layer);
    return { el: el, svg: svg, layer: layer };
  }
  function place(el, box) {
    el.style.left = box.x + "px";
    el.style.top = box.y + "px";
    el.style.width = box.width + "px";
    el.style.height = box.height + "px";
  }

  /**
   * Draw a laid-out DAG as a static element: curved arrows for the edges and each
   * node's element from renderNode(node) (e.g. a div.cs-dag-card) at its box.
   * dagSvg(layout, (node) => element, {edgeClass?: (edge) => string}) → <div class="cs-dag">.
   * For pan, zoom and live updates use kit.dagView.
   */
  function dagSvg(layout, renderNode, opts) {
    opts = opts || {};
    var st = stage(layout);
    var markerId = "cs-dag-arrow-" + Math.random().toString(36).slice(2, 8);
    st.svg.append(s("defs", null, arrowMarker(markerId, false)));
    layout.edges.forEach(function (e) {
      var extra = opts.edgeClass ? opts.edgeClass(e) : "";
      st.svg.append(s("path", {
        class: "cs-dag-edge" + (e.reversed ? " is-reversed" : "") + (e.cross ? " is-cross" : "") + (extra ? " " + extra : ""),
        d: edgePath(e.points, layout.direction),
        "marker-end": "url(#" + markerId + ")",
      }));
    });
    layout.nodes.forEach(function (n) {
      var box = h("div", { class: "cs-dag-node", "data-node": n.id });
      place(box, n);
      box.append(renderNode ? renderNode(n) : h("div", { class: "cs-dag-card", text: n.id }));
      st.layer.append(box);
    });
    var out = h("div", { class: "cs-dag" });
    out.append(st.el);
    return out;
  }

  /**
   * An interactive graph view: dagView(container, opts) mounts a pannable,
   * zoomable graph (drag or wheel to pan, pinch / ⌘-wheel to zoom, buttons to
   * zoom and fit) and returns a controller. view.update(nodes, edges) lays out and
   * draws; a re-read with the same shape only re-renders nodes whose data changed
   * (by opts.nodeVersion, default JSON), and keeps the pan and zoom. Hovering a
   * node highlights everything upstream and downstream of it.
   * opts: {renderNode(node) → element, renderGroup?(group) → element,
   *        onSelect?(id|null), onActivate?(id) (double-click), nodeVersion?(data),
   *        ...layoutDag options (direction, nodeWidth, group, ...)}
   * view: {el, update(nodes, edges), select(id), selected(), fit(), zoomBy(f),
   *        center(id), layout(), setOptions(opts)}
   */
  function dagView(container, opts) {
    opts = Object.assign({}, opts || {});
    var uid = Math.random().toString(36).slice(2, 8);
    var root = h("div", { class: "cs-dagview", tabindex: "0" });
    var st = stage({ width: 1, height: 1 });
    var defs = s("defs", null, arrowMarker("cs-a-" + uid, false), arrowMarker("cs-h-" + uid, true));
    var gGroups = s("g", { class: "cs-dag-groups" });
    var gEdges = s("g", { class: "cs-dag-edges" });
    st.svg.append(defs, gGroups, gEdges);
    var labels = h("div", { class: "cs-dag-labels" });
    st.el.insertBefore(labels, st.layer);
    function ctl(name, label, fn) {
      return h("button", { class: "cs-btn is-ghost is-icon is-sm", type: "button", title: label, "aria-label": label, onClick: fn }, icon(name, { size: 14 }));
    }
    var controls = h("div", { class: "cs-dagview-controls" },
      ctl("plus", "Zoom in", function () { zoomBy(1.25); }),
      ctl("minus", "Zoom out", function () { zoomBy(0.8); }),
      ctl("maximize", "Fit", function () { moved = false; fit(); }));
    root.append(st.el, controls);
    container.replaceChildren(root);

    var k = 1, tx = 0, ty = 0, moved = false;
    var lay = null, sig = "", sel = opts.selected || null, hover = null;
    var nodeEls = new Map(); // id → {el, v}
    var edgeEls = [];
    function apply() { st.el.style.transform = "translate(" + tx + "px," + ty + "px) scale(" + k + ")"; }
    function box() { return root.getBoundingClientRect(); }
    function zoomAt(px, py, f) {
      var nk = Math.min(2.5, Math.max(0.2, k * f));
      tx = px - ((px - tx) * nk) / k;
      ty = py - ((py - ty) * nk) / k;
      k = nk;
      moved = true;
      apply();
    }
    function zoomBy(f) { var b = box(); zoomAt(b.width / 2, b.height / 2, f); }
    function fit() {
      if (!lay) return;
      var b = box();
      if (!b.width || !b.height) return;
      k = Math.min(1, (b.width - 24) / lay.width, (b.height - 24) / lay.height);
      k = Math.max(opts.minFitScale || 0.45, k);
      var w = lay.width * k, ht = lay.height * k;
      tx = w < b.width ? (b.width - w) / 2 : 12;
      ty = ht < b.height ? Math.min(24, (b.height - ht) / 2) : 12;
      apply();
    }
    function center(id) {
      var n = lay && lay.nodes.find(function (m) { return m.id === id; });
      if (!n) return;
      var b = box();
      tx = b.width / 2 - n.cx * k;
      ty = b.height / 2 - n.cy * k;
      moved = true;
      apply();
    }

    // Pan by drag; a drag never counts as a click on the node under it.
    var drag = null, swallowClick = false;
    root.addEventListener("pointerdown", function (e) {
      if (e.button !== 0 || e.target.closest(".cs-dagview-controls") || e.target.closest("button, a, input, select, textarea")) return;
      drag = { x: e.clientX, y: e.clientY, tx: tx, ty: ty, on: false, id: e.pointerId };
    });
    root.addEventListener("pointermove", function (e) {
      if (!drag) return;
      var dx = e.clientX - drag.x, dy = e.clientY - drag.y;
      if (!drag.on && Math.abs(dx) + Math.abs(dy) > 4) {
        drag.on = true;
        root.classList.add("is-panning");
        try { root.setPointerCapture(drag.id); } catch (err) { /* already released */ }
      }
      if (drag.on) { tx = drag.tx + dx; ty = drag.ty + dy; moved = true; apply(); }
    });
    function endDrag() {
      if (drag && drag.on) { swallowClick = true; setTimeout(function () { swallowClick = false; }, 0); }
      drag = null;
      root.classList.remove("is-panning");
    }
    root.addEventListener("pointerup", endDrag);
    root.addEventListener("pointercancel", endDrag);
    root.addEventListener("click", function (e) {
      if (swallowClick) { e.stopPropagation(); e.preventDefault(); return; }
      if (e.target.closest(".cs-dagview-controls")) return;
      var node = e.target.closest("[data-node]");
      if (node) select(node.getAttribute("data-node"), true);
      else select(null, true);
    }, true);
    root.addEventListener("dblclick", function (e) {
      var node = e.target.closest("[data-node]");
      if (node && opts.onActivate) opts.onActivate(node.getAttribute("data-node"));
    });
    root.addEventListener("wheel", function (e) {
      e.preventDefault();
      var b = box();
      if (e.ctrlKey || e.metaKey) zoomAt(e.clientX - b.left, e.clientY - b.top, Math.exp(-e.deltaY * 0.01));
      else { tx -= e.deltaX; ty -= e.deltaY; moved = true; apply(); }
    }, { passive: false });
    root.addEventListener("keydown", function (e) {
      if (e.key === "Escape") select(null, true);
      else if (e.key === "+" || e.key === "=") zoomBy(1.25);
      else if (e.key === "-") zoomBy(0.8);
      else if (e.key === "0") { moved = false; fit(); }
    });
    st.layer.addEventListener("pointerover", function (e) {
      var node = e.target.closest("[data-node]");
      var id = node ? node.getAttribute("data-node") : null;
      if (id !== hover) { hover = id; paint(); }
    });
    st.layer.addEventListener("pointerleave", function () { hover = null; paint(); });
    if (typeof ResizeObserver === "function") {
      new ResizeObserver(function () { if (!moved) fit(); }).observe(root);
    }

    // Upstream / downstream of the focused node.
    function reach(id) {
      var up = new Set(), down = new Set();
      if (!lay || id == null) return null;
      var outs = Object.create(null), ins = Object.create(null);
      lay.edges.forEach(function (e) {
        (outs[e.from] = outs[e.from] || []).push(e.to);
        (ins[e.to] = ins[e.to] || []).push(e.from);
      });
      function walk(start, adj, acc) {
        var stack = [start];
        while (stack.length) {
          (adj[stack.pop()] || []).forEach(function (n) { if (!acc.has(n) && n !== id) { acc.add(n); stack.push(n); } });
        }
      }
      walk(id, ins, up);
      walk(id, outs, down);
      return { up: up, down: down };
    }
    function paint() {
      var focus = hover != null ? hover : sel;
      var r = reach(focus);
      nodeEls.forEach(function (ent, id) {
        ent.el.classList.toggle("is-selected", id === sel);
        ent.el.classList.toggle("is-focus", id === focus);
        ent.el.classList.toggle("is-upstream", !!r && r.up.has(id));
        ent.el.classList.toggle("is-downstream", !!r && r.down.has(id));
        ent.el.classList.toggle("is-dim", !!r && id !== focus && !r.up.has(id) && !r.down.has(id));
      });
      edgeEls.forEach(function (pair) {
        var e = pair.edge, el = pair.el;
        var inUp = function (n) { return n === focus || r.up.has(n); };
        var inDown = function (n) { return n === focus || r.down.has(n); };
        var onPath = !!r && ((inUp(e.from) && inUp(e.to)) || (inDown(e.from) && inDown(e.to)));
        el.classList.toggle("is-active", onPath);
        el.classList.toggle("is-dim", !!r && !onPath);
        el.setAttribute("marker-end", "url(#cs-" + (onPath ? "h-" : "a-") + uid + ")");
      });
    }
    function select(id, fromUser) {
      sel = id;
      paint();
      if (fromUser && opts.onSelect) opts.onSelect(id);
    }

    function update(nodes, edges) {
      var b0 = box();
      if (opts.group && !opts.maxWidth && b0.width && b0.height) {
        // Pack the clusters to the shape of the view: try a few row widths and
        // keep the one that fits at the largest scale.
        var best = null, bestK = -1;
        [1, 1.4, 2, 3, Infinity].forEach(function (f) {
          var l = layoutDag(nodes, edges, Object.assign({}, opts, { maxWidth: b0.width * f }));
          var fk = Math.min(1, b0.width / l.width, b0.height / l.height);
          if (fk > bestK + 0.01) { best = l; bestK = fk; }
        });
        lay = best;
      } else {
        lay = layoutDag(nodes, edges, opts);
      }
      var nextSig = JSON.stringify([
        lay.nodes.map(function (n) { return [n.id, n.x, n.y, n.width, n.height]; }),
        lay.edges.map(function (e) { return [e.from, e.to]; }),
        (lay.groups || []).map(function (g) { return [g.key, g.x, g.y, g.width, g.height, g.count]; }),
      ]);
      var shapeChanged = nextSig !== sig;
      sig = nextSig;
      if (shapeChanged) {
        st.el.style.width = lay.width + "px";
        st.el.style.height = lay.height + "px";
        st.svg.setAttribute("width", lay.width);
        st.svg.setAttribute("height", lay.height);
        st.svg.setAttribute("viewBox", "0 0 " + lay.width + " " + lay.height);
        gGroups.replaceChildren();
        labels.replaceChildren();
        (lay.groups || []).forEach(function (g) {
          gGroups.append(s("rect", { class: "cs-dag-group", x: g.x, y: g.y, width: g.width, height: g.height, rx: 12 }));
          var label = h("div", { class: "cs-dag-group-slot" });
          place(label, { x: g.x, y: g.y, width: g.width, height: opts.groupHeader || 30 });
          label.append(opts.renderGroup ? opts.renderGroup(g) : h("div", { class: "cs-dag-group-label", text: g.key }));
          labels.append(label);
        });
        gEdges.replaceChildren();
        edgeEls = lay.edges.map(function (e) {
          var el = s("path", {
            class: "cs-dag-edge" + (e.reversed ? " is-reversed" : "") + (e.cross ? " is-cross" : ""),
            d: edgePath(e.points, lay.direction),
          });
          gEdges.append(el);
          return { edge: e, el: el };
        });
      }
      var live = new Set();
      lay.nodes.forEach(function (n) {
        live.add(n.id);
        var v = opts.nodeVersion ? String(opts.nodeVersion(n.data)) : JSON.stringify(n.data);
        var ent = nodeEls.get(n.id);
        if (!ent) {
          ent = { el: h("div", { class: "cs-dag-node", "data-node": n.id }), v: null };
          nodeEls.set(n.id, ent);
          st.layer.append(ent.el);
        }
        place(ent.el, n);
        if (ent.v !== v) {
          ent.el.replaceChildren(opts.renderNode ? opts.renderNode(n) : h("div", { class: "cs-dag-card", text: n.id }));
          ent.v = v;
        }
      });
      nodeEls.forEach(function (ent, id) { if (!live.has(id)) { ent.el.remove(); nodeEls.delete(id); } });
      if (sel != null && !live.has(sel)) select(null, true);
      if (hover != null && !live.has(hover)) hover = null;
      paint();
      if (shapeChanged && !moved) fit();
      else apply();
    }

    return {
      el: root,
      update: update,
      select: function (id) { select(id, false); },
      selected: function () { return sel; },
      fit: function () { moved = false; fit(); },
      zoomBy: zoomBy,
      center: center,
      layout: function () { return lay; },
      setOptions: function (o) { Object.assign(opts, o); sig = ""; nodeEls.forEach(function (e) { e.v = null; }); },
    };
  }

/*@native-nav-start*/
  // Generated from the native components by scripts/generate-nav-kit.tsx.
  var NATIVE_RAILS = {"off:false":"<span aria-hidden=\"true\" data-rail-role=\"off\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0.5\" y=\"5.5\" width=\"17\" height=\"3\" rx=\"1.5\" fill=\"none\" stroke=\"var(--cs-rail-fill)\" stroke-width=\"1\" stroke-dasharray=\"1.5 1.5\"></rect></svg></span>","off:true":"<span aria-hidden=\"true\" data-rail-role=\"off\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0.5\" y=\"5.5\" width=\"17\" height=\"3\" rx=\"1.5\" fill=\"none\" stroke=\"var(--cs-rail-fill)\" stroke-width=\"1\" stroke-dasharray=\"1.5 1.5\"></rect></svg></span>","not_started:false":"<span aria-hidden=\"true\" data-rail-role=\"not_started\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect></svg></span>","not_started:true":"<span aria-hidden=\"true\" data-rail-role=\"not_started\" data-rail-ready=\"true\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"13.019999999999998\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><path d=\"M 13.619999999999997 3.598 L 17.4 7 L 13.619999999999997 10.402000000000001\" fill=\"none\" stroke=\"var(--cs-rail-fill)\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\" data-rail=\"ready\"></path></svg></span>","active:false":"<span aria-hidden=\"true\" data-rail-role=\"active\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"0\" y=\"5\" width=\"5.4\" height=\"4\" rx=\"1\" fill=\"var(--cs-rail-fill)\" data-rail=\"fill\"></rect></svg></span>","active:true":"<span aria-hidden=\"true\" data-rail-role=\"active\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"0\" y=\"5\" width=\"5.4\" height=\"4\" rx=\"1\" fill=\"var(--cs-rail-fill)\" data-rail=\"fill\"></rect></svg></span>","review:false":"<span aria-hidden=\"true\" data-rail-role=\"review\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"0\" y=\"5\" width=\"11.4\" height=\"4\" rx=\"1\" fill=\"var(--cs-rail-fill)\" data-rail=\"fill\"></rect></svg></span>","review:true":"<span aria-hidden=\"true\" data-rail-role=\"review\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"0\" y=\"5\" width=\"11.4\" height=\"4\" rx=\"1\" fill=\"var(--cs-rail-fill)\" data-rail=\"fill\"></rect></svg></span>","blocked:false":"<span aria-hidden=\"true\" data-rail-role=\"blocked\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"5.4\" height=\"4\" rx=\"1\" fill=\"var(--cs-rail-fill)\"></rect><rect x=\"9.85\" y=\"5\" width=\"8.15\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"7.1\" y=\"2.5\" width=\"1.75\" height=\"9\" rx=\"0.875\" fill=\"var(--cs-rail-wall)\" data-rail=\"wall\"></rect></svg></span>","blocked:true":"<span aria-hidden=\"true\" data-rail-role=\"blocked\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"5.4\" height=\"4\" rx=\"1\" fill=\"var(--cs-rail-fill)\"></rect><rect x=\"9.85\" y=\"5\" width=\"8.15\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"7.1\" y=\"2.5\" width=\"1.75\" height=\"9\" rx=\"0.875\" fill=\"var(--cs-rail-wall)\" data-rail=\"wall\"></rect></svg></span>","done:false":"<span aria-hidden=\"true\" data-rail-role=\"done\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-done)\" data-rail=\"fill\"></rect></svg></span>","done:true":"<span aria-hidden=\"true\" data-rail-role=\"done\" class=\"cs-rail\" style=\"width:18px;height:14px\"><svg width=\"18\" height=\"14\" viewBox=\"0 0 18 14\"><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-empty)\"></rect><rect x=\"0\" y=\"5\" width=\"18\" height=\"4\" rx=\"2\" fill=\"var(--cs-rail-done)\" data-rail=\"fill\"></rect></svg></span>"};
  var NATIVE_MARKS = {"question:6":"<span aria-hidden=\"true\" data-mark=\"question\" class=\"cs-mark cs-mark-question\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle class=\"cs-mark-halo\" cx=\"3\" cy=\"3\" r=\"4.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><mask id=\"CS_MASK\"><rect width=\"6\" height=\"6\" fill=\"white\"/><text x=\"3\" y=\"3.84\" font-size=\"4.5\" font-family=\"ui-sans-serif, system-ui\" font-weight=\"700\" text-anchor=\"middle\" fill=\"black\">?</text></mask><circle cx=\"3\" cy=\"3\" r=\"3\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","question:7":"<span aria-hidden=\"true\" data-mark=\"question\" class=\"cs-mark cs-mark-question\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle class=\"cs-mark-halo\" cx=\"3.5\" cy=\"3.5\" r=\"4.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><mask id=\"CS_MASK\"><rect width=\"7\" height=\"7\" fill=\"white\"/><text x=\"3.5\" y=\"4.48\" font-size=\"5.25\" font-family=\"ui-sans-serif, system-ui\" font-weight=\"700\" text-anchor=\"middle\" fill=\"black\">?</text></mask><circle cx=\"3.5\" cy=\"3.5\" r=\"3.5\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","question:8":"<span aria-hidden=\"true\" data-mark=\"question\" class=\"cs-mark cs-mark-question\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle class=\"cs-mark-halo\" cx=\"4\" cy=\"4\" r=\"5.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><mask id=\"CS_MASK\"><rect width=\"8\" height=\"8\" fill=\"white\"/><text x=\"4\" y=\"5.12\" font-size=\"6\" font-family=\"ui-sans-serif, system-ui\" font-weight=\"700\" text-anchor=\"middle\" fill=\"black\">?</text></mask><circle cx=\"4\" cy=\"4\" r=\"4\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","permission:6":"<span aria-hidden=\"true\" data-mark=\"permission\" class=\"cs-mark cs-mark-permission\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle class=\"cs-mark-halo\" cx=\"3\" cy=\"3\" r=\"4.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><mask id=\"CS_MASK\"><rect width=\"6\" height=\"6\" fill=\"white\"/><rect x=\"1.5\" y=\"2.5\" width=\"3\" height=\"1\" rx=\"0.3\" fill=\"black\"/></mask><circle cx=\"3\" cy=\"3\" r=\"3\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","permission:7":"<span aria-hidden=\"true\" data-mark=\"permission\" class=\"cs-mark cs-mark-permission\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle class=\"cs-mark-halo\" cx=\"3.5\" cy=\"3.5\" r=\"4.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><mask id=\"CS_MASK\"><rect width=\"7\" height=\"7\" fill=\"white\"/><rect x=\"1.5\" y=\"3\" width=\"4\" height=\"1\" rx=\"0.3\" fill=\"black\"/></mask><circle cx=\"3.5\" cy=\"3.5\" r=\"3.5\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","permission:8":"<span aria-hidden=\"true\" data-mark=\"permission\" class=\"cs-mark cs-mark-permission\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle class=\"cs-mark-halo\" cx=\"4\" cy=\"4\" r=\"5.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><mask id=\"CS_MASK\"><rect width=\"8\" height=\"8\" fill=\"white\"/><rect x=\"2\" y=\"3.5\" width=\"4\" height=\"1\" rx=\"0.3\" fill=\"black\"/></mask><circle cx=\"4\" cy=\"4\" r=\"4\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","blocked:6":"<span aria-hidden=\"true\" data-mark=\"blocked\" class=\"cs-mark cs-mark-blocked\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle class=\"cs-mark-halo-still\" cx=\"3\" cy=\"3\" r=\"4.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" opacity=\"0.35\"/><mask id=\"CS_MASK\"><rect width=\"6\" height=\"6\" fill=\"white\"/><rect x=\"1.5\" y=\"1.5\" width=\"1\" height=\"3\" fill=\"black\"/><rect x=\"3.5\" y=\"1.5\" width=\"1\" height=\"3\" fill=\"black\"/></mask><circle cx=\"3\" cy=\"3\" r=\"3\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","blocked:7":"<span aria-hidden=\"true\" data-mark=\"blocked\" class=\"cs-mark cs-mark-blocked\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle class=\"cs-mark-halo-still\" cx=\"3.5\" cy=\"3.5\" r=\"4.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" opacity=\"0.35\"/><mask id=\"CS_MASK\"><rect width=\"7\" height=\"7\" fill=\"white\"/><rect x=\"2\" y=\"1.75\" width=\"1\" height=\"3.5\" fill=\"black\"/><rect x=\"4\" y=\"1.75\" width=\"1\" height=\"3.5\" fill=\"black\"/></mask><circle cx=\"3.5\" cy=\"3.5\" r=\"3.5\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","blocked:8":"<span aria-hidden=\"true\" data-mark=\"blocked\" class=\"cs-mark cs-mark-blocked\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle class=\"cs-mark-halo-still\" cx=\"4\" cy=\"4\" r=\"5.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" opacity=\"0.35\"/><mask id=\"CS_MASK\"><rect width=\"8\" height=\"8\" fill=\"white\"/><rect x=\"2.5\" y=\"2\" width=\"1\" height=\"4\" fill=\"black\"/><rect x=\"4.5\" y=\"2\" width=\"1\" height=\"4\" fill=\"black\"/></mask><circle cx=\"4\" cy=\"4\" r=\"4\" fill=\"currentColor\" mask=\"url(#CS_MASK)\"/></svg></span>","crashed:6":"<span aria-hidden=\"true\" data-mark=\"crashed\" class=\"cs-mark cs-mark-crashed\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><rect x=\"0.6000000000000001\" y=\"0.6000000000000001\" width=\"4.8\" height=\"4.8\" rx=\"0.75\" fill=\"currentColor\"/></svg></span>","crashed:7":"<span aria-hidden=\"true\" data-mark=\"crashed\" class=\"cs-mark cs-mark-crashed\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><rect x=\"0.7000000000000001\" y=\"0.7000000000000001\" width=\"5.6\" height=\"5.6\" rx=\"0.75\" fill=\"currentColor\"/></svg></span>","crashed:8":"<span aria-hidden=\"true\" data-mark=\"crashed\" class=\"cs-mark cs-mark-crashed\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><rect x=\"0.8\" y=\"0.8\" width=\"6.4\" height=\"6.4\" rx=\"0.8\" fill=\"currentColor\"/></svg></span>","error:6":"<span aria-hidden=\"true\" data-mark=\"error\" class=\"cs-mark cs-mark-error\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><mask id=\"CS_MASK\"><rect width=\"6\" height=\"6\" fill=\"white\"/><rect x=\"2.5\" y=\"2.16\" width=\"1\" height=\"1.6800000000000002\" fill=\"black\"/><rect x=\"2.5\" y=\"4.199999999999999\" width=\"1\" height=\"1\" fill=\"black\"/></mask><path d=\"M 3 0.24 L 6 5.5200000000000005 L 0 5.5200000000000005 Z\" fill=\"currentColor\" stroke=\"currentColor\" stroke-width=\"0.6\" stroke-linejoin=\"round\" mask=\"url(#CS_MASK)\"/></svg></span>","error:7":"<span aria-hidden=\"true\" data-mark=\"error\" class=\"cs-mark cs-mark-error\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><mask id=\"CS_MASK\"><rect width=\"7\" height=\"7\" fill=\"white\"/><rect x=\"3\" y=\"2.52\" width=\"1\" height=\"1.9600000000000002\" fill=\"black\"/><rect x=\"3\" y=\"4.8999999999999995\" width=\"1\" height=\"1\" fill=\"black\"/></mask><path d=\"M 3.5 0.28 L 7 6.44 L 0 6.44 Z\" fill=\"currentColor\" stroke=\"currentColor\" stroke-width=\"0.6\" stroke-linejoin=\"round\" mask=\"url(#CS_MASK)\"/></svg></span>","error:8":"<span aria-hidden=\"true\" data-mark=\"error\" class=\"cs-mark cs-mark-error\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><mask id=\"CS_MASK\"><rect width=\"8\" height=\"8\" fill=\"white\"/><rect x=\"3.5\" y=\"2.88\" width=\"1\" height=\"2.24\" fill=\"black\"/><rect x=\"3.5\" y=\"5.6\" width=\"1\" height=\"1\" fill=\"black\"/></mask><path d=\"M 4 0.32 L 8 7.36 L 0 7.36 Z\" fill=\"currentColor\" stroke=\"currentColor\" stroke-width=\"0.6\" stroke-linejoin=\"round\" mask=\"url(#CS_MASK)\"/></svg></span>","tool:6":"<span aria-hidden=\"true\" data-mark=\"tool\" class=\"cs-mark cs-mark-tool\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\" class=\"cs-mark-spin\"><path d=\"M 3 0.75 A 2.25 2.25 0 1 1 0.75 3\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/></svg></span>","tool:7":"<span aria-hidden=\"true\" data-mark=\"tool\" class=\"cs-mark cs-mark-tool\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\" class=\"cs-mark-spin\"><path d=\"M 3.5 0.75 A 2.75 2.75 0 1 1 0.75 3.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/></svg></span>","tool:8":"<span aria-hidden=\"true\" data-mark=\"tool\" class=\"cs-mark cs-mark-tool\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\" class=\"cs-mark-spin\"><path d=\"M 4 0.75 A 3.25 3.25 0 1 1 0.75 4\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/></svg></span>","thinking:6":"<span aria-hidden=\"true\" data-mark=\"thinking\" class=\"cs-mark cs-mark-thinking\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle class=\"cs-mark-think\" data-i=\"0\" cx=\"1.2000000000000002\" cy=\"3\" r=\"0.66\" fill=\"currentColor\"/><circle class=\"cs-mark-think\" data-i=\"1\" cx=\"3\" cy=\"3\" r=\"0.66\" fill=\"currentColor\"/><circle class=\"cs-mark-think\" data-i=\"2\" cx=\"4.8\" cy=\"3\" r=\"0.66\" fill=\"currentColor\"/></svg></span>","thinking:7":"<span aria-hidden=\"true\" data-mark=\"thinking\" class=\"cs-mark cs-mark-thinking\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle class=\"cs-mark-think\" data-i=\"0\" cx=\"1.4\" cy=\"3.5\" r=\"0.77\" fill=\"currentColor\"/><circle class=\"cs-mark-think\" data-i=\"1\" cx=\"3.5\" cy=\"3.5\" r=\"0.77\" fill=\"currentColor\"/><circle class=\"cs-mark-think\" data-i=\"2\" cx=\"5.6\" cy=\"3.5\" r=\"0.77\" fill=\"currentColor\"/></svg></span>","thinking:8":"<span aria-hidden=\"true\" data-mark=\"thinking\" class=\"cs-mark cs-mark-thinking\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle class=\"cs-mark-think\" data-i=\"0\" cx=\"1.6\" cy=\"4\" r=\"0.88\" fill=\"currentColor\"/><circle class=\"cs-mark-think\" data-i=\"1\" cx=\"4\" cy=\"4\" r=\"0.88\" fill=\"currentColor\"/><circle class=\"cs-mark-think\" data-i=\"2\" cx=\"6.4\" cy=\"4\" r=\"0.88\" fill=\"currentColor\"/></svg></span>","stalled:6":"<span aria-hidden=\"true\" data-mark=\"stalled\" class=\"cs-mark cs-mark-stalled\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle cx=\"3\" cy=\"3\" r=\"2.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><circle cx=\"3\" cy=\"3\" r=\"0.9\" fill=\"currentColor\"/></svg></span>","stalled:7":"<span aria-hidden=\"true\" data-mark=\"stalled\" class=\"cs-mark cs-mark-stalled\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle cx=\"3.5\" cy=\"3.5\" r=\"2.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><circle cx=\"3.5\" cy=\"3.5\" r=\"1.05\" fill=\"currentColor\"/></svg></span>","stalled:8":"<span aria-hidden=\"true\" data-mark=\"stalled\" class=\"cs-mark cs-mark-stalled\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle cx=\"4\" cy=\"4\" r=\"3.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><circle cx=\"4\" cy=\"4\" r=\"1.2\" fill=\"currentColor\"/></svg></span>","ready:6":"<span aria-hidden=\"true\" data-mark=\"ready\" class=\"cs-mark cs-mark-ready\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle cx=\"3\" cy=\"3\" r=\"2.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg></span>","ready:7":"<span aria-hidden=\"true\" data-mark=\"ready\" class=\"cs-mark cs-mark-ready\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle cx=\"3.5\" cy=\"3.5\" r=\"2.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg></span>","ready:8":"<span aria-hidden=\"true\" data-mark=\"ready\" class=\"cs-mark cs-mark-ready\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle cx=\"4\" cy=\"4\" r=\"3.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg></span>","unknown:6":"<span aria-hidden=\"true\" data-mark=\"unknown\" class=\"cs-mark cs-mark-unknown\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle cx=\"3\" cy=\"3\" r=\"2.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-dasharray=\"1.2959069696057897 1.0602875205865552\"/></svg></span>","unknown:7":"<span aria-hidden=\"true\" data-mark=\"unknown\" class=\"cs-mark cs-mark-unknown\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle cx=\"3.5\" cy=\"3.5\" r=\"2.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-dasharray=\"1.583886296184854 1.2959069696057897\"/></svg></span>","unknown:8":"<span aria-hidden=\"true\" data-mark=\"unknown\" class=\"cs-mark cs-mark-unknown\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle cx=\"4\" cy=\"4\" r=\"3.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-dasharray=\"1.8718656227639185 1.5315264186250241\"/></svg></span>","ended:6":"<span aria-hidden=\"true\" data-mark=\"ended\" class=\"cs-mark cs-mark-ended\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><rect x=\"1.225\" y=\"1.225\" width=\"3.55\" height=\"3.55\" rx=\"0.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.25\"/></svg></span>","ended:7":"<span aria-hidden=\"true\" data-mark=\"ended\" class=\"cs-mark cs-mark-ended\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><rect x=\"1.3250000000000002\" y=\"1.3250000000000002\" width=\"4.35\" height=\"4.35\" rx=\"0.56\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.25\"/></svg></span>","ended:8":"<span aria-hidden=\"true\" data-mark=\"ended\" class=\"cs-mark cs-mark-ended\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><rect x=\"1.425\" y=\"1.425\" width=\"5.15\" height=\"5.15\" rx=\"0.64\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.25\"/></svg></span>","offline:6":"<span aria-hidden=\"true\" data-mark=\"offline\" class=\"cs-mark cs-mark-offline\" style=\"width:6px;height:6px\"><svg width=\"6\" height=\"6\" viewBox=\"0 0 6 6\" overflow=\"visible\"><circle cx=\"3\" cy=\"3\" r=\"2.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><line x1=\"0.8999999999999999\" y1=\"5.1\" x2=\"5.1\" y2=\"0.8999999999999999\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/></svg></span>","offline:7":"<span aria-hidden=\"true\" data-mark=\"offline\" class=\"cs-mark cs-mark-offline\" style=\"width:7px;height:7px\"><svg width=\"7\" height=\"7\" viewBox=\"0 0 7 7\" overflow=\"visible\"><circle cx=\"3.5\" cy=\"3.5\" r=\"2.75\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><line x1=\"1.05\" y1=\"5.95\" x2=\"5.95\" y2=\"1.05\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/></svg></span>","offline:8":"<span aria-hidden=\"true\" data-mark=\"offline\" class=\"cs-mark cs-mark-offline\" style=\"width:8px;height:8px\"><svg width=\"8\" height=\"8\" viewBox=\"0 0 8 8\" overflow=\"visible\"><circle cx=\"4\" cy=\"4\" r=\"3.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><line x1=\"1.2\" y1=\"6.8\" x2=\"6.8\" y2=\"1.2\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/></svg></span>"};
  var NATIVE_FILES = {"markdown":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"markdown\" class=\"cs-file-kind-icon flex-none\"><path d=\"M4 1.75h5.4L12.75 5.1v9.15H4z\"></path><path d=\"M9.25 1.85V5.25h3.4\"></path><path d=\"M6.25 8h4.25M6.25 10.25h4.25M6.25 12.25h2.5\"></path></svg>","text":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"text\" class=\"cs-file-kind-icon flex-none\"><path d=\"M4 1.75h5.4L12.75 5.1v9.15H4z\"></path><path d=\"M9.25 1.85V5.25h3.4\"></path></svg>","pdf":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"pdf\" class=\"cs-file-kind-icon flex-none\"><path d=\"M4 1.75h5.4L12.75 5.1v9.15H4z\"></path><path d=\"M9.25 1.85V5.25h3.4\"></path><path d=\"M4 10.5h8.75v3.75H4z\" fill=\"currentColor\" stroke=\"none\"></path></svg>","html":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"html\" class=\"cs-file-kind-icon flex-none\"><rect x=\"1.75\" y=\"2.75\" width=\"12.5\" height=\"10.5\" rx=\"1.75\"></rect><path d=\"M1.75 5.75h12.5\"></path><circle cx=\"3.75\" cy=\"4.25\" r=\".5\" fill=\"currentColor\" stroke=\"none\"></circle><circle cx=\"5.4\" cy=\"4.25\" r=\".5\" fill=\"currentColor\" stroke=\"none\"></circle><path d=\"M4.5 8.25h7M4.5 10.5h4.5\"></path></svg>","dashboard":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"dashboard\" class=\"cs-file-kind-icon flex-none\"><rect x=\"1.75\" y=\"2.75\" width=\"12.5\" height=\"10.5\" rx=\"1.75\"></rect><path d=\"M1.75 5.75h12.5\"></path><rect x=\"3.75\" y=\"7.5\" width=\"3.5\" height=\"4\" rx=\".6\" fill=\"currentColor\" stroke=\"none\"></rect><rect x=\"8.9\" y=\"7.6\" width=\"3.35\" height=\"1.5\" rx=\".5\" stroke-width=\"1\"></rect><rect x=\"8.9\" y=\"10.1\" width=\"3.35\" height=\"1.35\" rx=\".5\" fill=\"currentColor\" stroke=\"none\" opacity=\"0.75\"></rect></svg>","slides":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"slides\" class=\"cs-file-kind-icon flex-none\"><rect x=\"1.75\" y=\"2.5\" width=\"12.5\" height=\"8.5\" rx=\"1.25\"></rect><path d=\"M8 11v2.75M5.5 13.75h5\"></path><path d=\"M4.5 5.25h4v1.25h-4z\" fill=\"currentColor\" stroke=\"none\"></path></svg>","canvas":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"canvas\" class=\"cs-file-kind-icon flex-none\"><path d=\"M5 2.6c1.6-.2 2.7.9 2.6 2.4-.1 1.6-1.3 2.5-2.8 2.4C3.4 7.3 2.4 6.2 2.5 4.8 2.6 3.6 3.6 2.7 5 2.6z\"></path><rect x=\"9\" y=\"9.25\" width=\"5.25\" height=\"4.5\" rx=\".9\" transform=\"rotate(-3 11.6 11.5)\"></rect><path d=\"M7.4 6.6c1.4.6 2.3 1.4 2.9 2.4\"></path><path d=\"M9.2 8.9l1.2.2.1-1.2\"></path></svg>","image":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"image\" class=\"cs-file-kind-icon flex-none\"><rect x=\"1.75\" y=\"2.75\" width=\"12.5\" height=\"10.5\" rx=\"1.75\"></rect><circle cx=\"5.5\" cy=\"6.25\" r=\"1.15\"></circle><path d=\"M1.9 11.75l3.6-3.25 2.75 2.5 2.25-1.75 3.6 2.9\"></path></svg>","video":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"video\" class=\"cs-file-kind-icon flex-none\"><rect x=\"1.75\" y=\"2.75\" width=\"12.5\" height=\"10.5\" rx=\"1.75\"></rect><path d=\"M6.5 5.75v4.5L10.25 8z\" fill=\"currentColor\" stroke=\"none\"></path></svg>","code":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"code\" class=\"cs-file-kind-icon flex-none\"><path d=\"M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5\"></path><path d=\"M9 3.25L7 12.75\" opacity=\"0.55\"></path></svg>","json":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"json\" class=\"cs-file-kind-icon flex-none\"><path d=\"M5.5 2.5c-1.6 0-2 .7-2 2v1.6c0 1-.5 1.6-1.5 1.9 1 .3 1.5.9 1.5 1.9v1.6c0 1.3.4 2 2 2M10.5 2.5c1.6 0 2 .7 2 2v1.6c0 1 .5 1.6 1.5 1.9-1 .3-1.5.9-1.5 1.9v1.6c0 1.3-.4 2-2 2\"></path><circle cx=\"8\" cy=\"8\" r=\".75\" fill=\"currentColor\" stroke=\"none\"></circle></svg>","config":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"config\" class=\"cs-file-kind-icon flex-none\"><path d=\"M2 4.75h6.65M11.85 4.75H14M2 11.25h2.15M7.35 11.25H14\"></path><circle cx=\"10.25\" cy=\"4.75\" r=\"1.6\"></circle><circle cx=\"5.75\" cy=\"11.25\" r=\"1.6\"></circle></svg>","table":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"table\" class=\"cs-file-kind-icon flex-none\"><path d=\"M2.25 5.25h11.5M2.25 8.25h11.5M2.25 11.25h11.5M6.5 2.75v11\"></path><path d=\"M2.25 2.75h11.5\" opacity=\"0.5\"></path></svg>","shell":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"shell\" class=\"cs-file-kind-icon flex-none\"><path d=\"M2.5 4.5L6 8l-3.5 3.5\"></path><path d=\"M8 11.75h5.5\"></path></svg>","archive":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"archive\" class=\"cs-file-kind-icon flex-none\"><path d=\"M2.25 5l5.75-3 5.75 3v6.25L8 14.25 2.25 11.25z\"></path><path d=\"M2.25 5L8 8l5.75-3M8 8v6.25\"></path></svg>","folder":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"folder\" class=\"cs-file-kind-icon flex-none\"><path d=\"M1.75 4.25c0-.8.6-1.5 1.5-1.5h3l1.5 1.75h5c.8 0 1.5.6 1.5 1.5v6.75c0 .8-.7 1.5-1.5 1.5H3.25c-.9 0-1.5-.7-1.5-1.5z\"></path></svg>","folderOpen":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"folderOpen\" class=\"cs-file-kind-icon flex-none\"><path d=\"M1.75 12.25V4.25c0-.8.6-1.5 1.5-1.5h3l1.5 1.75h4.5c.8 0 1.5.6 1.5 1.5v1.25\"></path><path d=\"M1.75 12.25l1.6-4.5c.2-.5.6-.75 1.1-.75h9.8l-1.75 5.25c-.2.5-.6.75-1.1.75H1.75z\"></path></svg>","epic":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"epic\" class=\"cs-file-kind-icon flex-none\"><path d=\"M1.75 4.25c0-.8.6-1.5 1.5-1.5h3l1.5 1.75h5c.8 0 1.5.6 1.5 1.5v6.75c0 .8-.7 1.5-1.5 1.5H3.25c-.9 0-1.5-.7-1.5-1.5z\"></path><rect x=\"4.25\" y=\"8.75\" width=\"7.5\" height=\"2.25\" rx=\"1.1\" fill=\"currentColor\" stroke=\"none\" opacity=\"0.35\"></rect><rect x=\"4.25\" y=\"8.75\" width=\"4\" height=\"2.25\" rx=\"1.1\" fill=\"currentColor\" stroke=\"none\"></rect></svg>","repo":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"repo\" class=\"cs-file-kind-icon flex-none\"><circle cx=\"4.5\" cy=\"3.75\" r=\"1.6\"></circle><circle cx=\"4.5\" cy=\"12.25\" r=\"1.6\"></circle><circle cx=\"11.5\" cy=\"5.5\" r=\"1.6\"></circle><path d=\"M4.5 5.35v5.3M11.5 7.1c0 2.4-2.5 2.6-5.5 4.2\"></path></svg>","agentDef":"<svg width=\"14\" height=\"14\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4285714285714286\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\" data-file-kind=\"agentDef\" class=\"cs-file-kind-icon flex-none\"><rect x=\"2.5\" y=\"4.75\" width=\"11\" height=\"8.5\" rx=\"2.5\"></rect><path d=\"M8 4.75V2.5\"></path><circle cx=\"8\" cy=\"2.1\" r=\".8\" fill=\"currentColor\" stroke=\"none\"></circle><circle cx=\"5.9\" cy=\"8.75\" r=\"1\" fill=\"currentColor\" stroke=\"none\"></circle><circle cx=\"10.1\" cy=\"8.75\" r=\"1\" fill=\"currentColor\" stroke=\"none\"></circle></svg>"};
  var NATIVE_NAV_GLYPHS = {"changes":"<svg width=\"13\" height=\"13\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" aria-hidden=\"true\"><path d=\"M2.5 3.5h11M2.5 7h11M2.5 10.5h5.5\"></path><circle cx=\"11.75\" cy=\"11.25\" r=\"1.9\"></circle></svg>","history":"<svg width=\"13\" height=\"13\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"M2.6 8a5.4 5.4 0 1 0 1.6-3.8\"></path><path d=\"M2.4 2.6v2.6H5\"></path><path d=\"M8 5.2V8l2 1.4\"></path></svg>","grouped":"<svg width=\"13\" height=\"13\" viewBox=\"0 0 16 16\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" aria-hidden=\"true\"><path d=\"M2.5 3.5h11M4.5 6.5h9M4.5 9.5h9M2.5 12.5h11\"></path></svg>"};
  var NATIVE_RULE_DATA = {"fileKinds":{"txt":"text","text":"text","log":"text","rst":"text","yaml":"config","yml":"config","toml":"config","ini":"config","env":"config","conf":"config","properties":"config","csv":"table","tsv":"table","sh":"shell","bash":"shell","zsh":"shell","fish":"shell","pdf":"pdf","doc":"pdf","docx":"pdf","xls":"pdf","xlsx":"pdf","rtf":"code","odt":"code","ods":"code","ppt":"slides","pptx":"slides","key":"slides","odp":"slides","mp4":"video","webm":"video","mov":"video","avi":"video","mkv":"video","mp3":"video","wav":"video","flac":"video","ogg":"video","m4a":"video","ico":"image","bmp":"image","tiff":"image","tif":"image","heic":"image","json":"json","jsonc":"json","json5":"json","xml":"code","md":"markdown","markdown":"markdown","html":"html","htm":"html","excalidraw":"canvas","png":"image","jpg":"image","jpeg":"image","gif":"image","webp":"image","svg":"image","py":"code","rs":"code","ts":"code","js":"code","zip":"archive","tar":"archive","gz":"archive","tgz":"archive","bz2":"archive","xz":"archive","zst":"archive","br":"archive","7z":"archive","rar":"archive","exe":"archive","dll":"archive","so":"archive","dylib":"archive","o":"archive","a":"archive","bin":"archive","wasm":"archive","class":"archive","pyc":"archive","woff":"archive","woff2":"archive","ttf":"archive","otf":"archive","eot":"archive","sqlite":"archive","db":"archive"},"dashboardFileKinds":{"html":"dashboard","htm":"dashboard"},"agentDefinitionPatterns":[["(^|\\/)\\.claude\\/agents\\/[^/]+\\.md$","i"],["(^|\\/)skills\\/[^/]+\\/SKILL\\.md$",""]],"dashboardDirPattern":["(^|\\/)dashboards\\/",""],"defaultFileKind":"code","stripViewerExtensions":["md","markdown","html","htm","excalidraw"],"pipelineExtensions":["md","html","htm"],"lenses":["everything","needs","agents","ready","active","review","blocked","not_started","done"],"lensMasks":{"||false":"100000000","||true":"100000000","|question|false":"111000000","|question|true":"111000000","|permission|false":"111000000","|permission|true":"111000000","|blocked|false":"111000000","|blocked|true":"111000000","|crashed|false":"100000000","|crashed|true":"100000000","|error|false":"101000000","|error|true":"101000000","|tool|false":"101000000","|tool|true":"101000000","|thinking|false":"101000000","|thinking|true":"101000000","|stalled|false":"101000000","|stalled|true":"101000000","|ready|false":"101000000","|ready|true":"101000000","|unknown|false":"101000000","|unknown|true":"101000000","|ended|false":"100000000","|ended|true":"100000000","|offline|false":"101000000","|offline|true":"101000000","not_started||false":"100000010","not_started||true":"100100010","not_started|question|false":"111000010","not_started|question|true":"111100010","not_started|permission|false":"111000010","not_started|permission|true":"111100010","not_started|blocked|false":"111000010","not_started|blocked|true":"111100010","not_started|crashed|false":"100000010","not_started|crashed|true":"100100010","not_started|error|false":"101000010","not_started|error|true":"101100010","not_started|tool|false":"101000010","not_started|tool|true":"101100010","not_started|thinking|false":"101000010","not_started|thinking|true":"101100010","not_started|stalled|false":"101000010","not_started|stalled|true":"101100010","not_started|ready|false":"101000010","not_started|ready|true":"101100010","not_started|unknown|false":"101000010","not_started|unknown|true":"101100010","not_started|ended|false":"100000010","not_started|ended|true":"100100010","not_started|offline|false":"101000010","not_started|offline|true":"101100010","active||false":"100010000","active||true":"100010000","active|question|false":"111010000","active|question|true":"111010000","active|permission|false":"111010000","active|permission|true":"111010000","active|blocked|false":"111010000","active|blocked|true":"111010000","active|crashed|false":"100010000","active|crashed|true":"100010000","active|error|false":"101010000","active|error|true":"101010000","active|tool|false":"101010000","active|tool|true":"101010000","active|thinking|false":"101010000","active|thinking|true":"101010000","active|stalled|false":"101010000","active|stalled|true":"101010000","active|ready|false":"101010000","active|ready|true":"101010000","active|unknown|false":"101010000","active|unknown|true":"101010000","active|ended|false":"100010000","active|ended|true":"100010000","active|offline|false":"101010000","active|offline|true":"101010000","review||false":"100001000","review||true":"100001000","review|question|false":"111001000","review|question|true":"111001000","review|permission|false":"111001000","review|permission|true":"111001000","review|blocked|false":"111001000","review|blocked|true":"111001000","review|crashed|false":"100001000","review|crashed|true":"100001000","review|error|false":"101001000","review|error|true":"101001000","review|tool|false":"101001000","review|tool|true":"101001000","review|thinking|false":"101001000","review|thinking|true":"101001000","review|stalled|false":"101001000","review|stalled|true":"101001000","review|ready|false":"101001000","review|ready|true":"101001000","review|unknown|false":"101001000","review|unknown|true":"101001000","review|ended|false":"100001000","review|ended|true":"100001000","review|offline|false":"101001000","review|offline|true":"101001000","blocked||false":"100000100","blocked||true":"100000100","blocked|question|false":"111000100","blocked|question|true":"111000100","blocked|permission|false":"111000100","blocked|permission|true":"111000100","blocked|blocked|false":"111000100","blocked|blocked|true":"111000100","blocked|crashed|false":"100000100","blocked|crashed|true":"100000100","blocked|error|false":"101000100","blocked|error|true":"101000100","blocked|tool|false":"101000100","blocked|tool|true":"101000100","blocked|thinking|false":"101000100","blocked|thinking|true":"101000100","blocked|stalled|false":"101000100","blocked|stalled|true":"101000100","blocked|ready|false":"101000100","blocked|ready|true":"101000100","blocked|unknown|false":"101000100","blocked|unknown|true":"101000100","blocked|ended|false":"100000100","blocked|ended|true":"100000100","blocked|offline|false":"101000100","blocked|offline|true":"101000100","done||false":"100000001","done||true":"100000001","done|question|false":"111000001","done|question|true":"111000001","done|permission|false":"111000001","done|permission|true":"111000001","done|blocked|false":"111000001","done|blocked|true":"111000001","done|crashed|false":"100000001","done|crashed|true":"100000001","done|error|false":"101000001","done|error|true":"101000001","done|tool|false":"101000001","done|tool|true":"101000001","done|thinking|false":"101000001","done|thinking|true":"101000001","done|stalled|false":"101000001","done|stalled|true":"101000001","done|ready|false":"101000001","done|ready|true":"101000001","done|unknown|false":"101000001","done|unknown|true":"101000001","done|ended|false":"100000001","done|ended|true":"100000001","done|offline|false":"101000001","done|offline|true":"101000001"},"stageTable":{"|":"off","|question":"needs","|permission":"needs","|blocked":"needs","|crashed":"off","|error":"off","|tool":"off","|thinking":"off","|stalled":"off","|ready":"off","|unknown":"off","|ended":"off","|offline":"off","not_started|":"not_started","not_started|question":"needs","not_started|permission":"needs","not_started|blocked":"needs","not_started|crashed":"not_started","not_started|error":"not_started","not_started|tool":"not_started","not_started|thinking":"not_started","not_started|stalled":"not_started","not_started|ready":"not_started","not_started|unknown":"not_started","not_started|ended":"not_started","not_started|offline":"not_started","active|":"active","active|question":"needs","active|permission":"needs","active|blocked":"needs","active|crashed":"active","active|error":"active","active|tool":"active","active|thinking":"active","active|stalled":"active","active|ready":"active","active|unknown":"active","active|ended":"active","active|offline":"active","review|":"review","review|question":"needs","review|permission":"needs","review|blocked":"needs","review|crashed":"review","review|error":"review","review|tool":"review","review|thinking":"review","review|stalled":"review","review|ready":"review","review|unknown":"review","review|ended":"review","review|offline":"review","blocked|":"blocked","blocked|question":"needs","blocked|permission":"needs","blocked|blocked":"needs","blocked|crashed":"blocked","blocked|error":"blocked","blocked|tool":"blocked","blocked|thinking":"blocked","blocked|stalled":"blocked","blocked|ready":"blocked","blocked|unknown":"blocked","blocked|ended":"blocked","blocked|offline":"blocked","done|":"done","done|question":"needs","done|permission":"needs","done|blocked":"needs","done|crashed":"done","done|error":"done","done|tool":"done","done|thinking":"done","done|stalled":"done","done|ready":"done","done|unknown":"done","done|ended":"done","done|offline":"done"},"collatorOptions":{"numeric":true,"sensitivity":"base"}};
  var NATIVE_FILE_KINDS = NATIVE_RULE_DATA.fileKinds;
  var NATIVE_STRIP_VIEWER = new Set(NATIVE_RULE_DATA.stripViewerExtensions);
  var NATIVE_STRIP_PIPELINE = new Set(NATIVE_RULE_DATA.pipelineExtensions);
  var NATIVE_LENS_MASKS = NATIVE_RULE_DATA.lensMasks;
  var NATIVE_STAGE_TABLE = NATIVE_RULE_DATA.stageTable;
  var NATIVE_NAV_LENSES = NATIVE_RULE_DATA.lenses;
  var NATIVE_NAV_COLLATOR = new Intl.Collator(undefined, NATIVE_RULE_DATA.collatorOptions);
  var NATIVE_AGENT_PATTERNS = NATIVE_RULE_DATA.agentDefinitionPatterns.map(function(p) { return new RegExp(p[0], p[1]); });
  var NATIVE_DASHBOARD_DIR = new RegExp(NATIVE_RULE_DATA.dashboardDirPattern[0], NATIVE_RULE_DATA.dashboardDirPattern[1]);
  function fileKindOf(path) {
    if (NATIVE_AGENT_PATTERNS.some(function(pattern) { return pattern.test(path); })) return "agentDef";
    var base = path.slice(path.lastIndexOf("/") + 1), dot = base.lastIndexOf("."), ext = dot < 0 ? null : base.slice(dot + 1).toLowerCase();
    if (ext === null) return "text";
    if (NATIVE_DASHBOARD_DIR.test(path) && NATIVE_RULE_DATA.dashboardFileKinds[ext]) return NATIVE_RULE_DATA.dashboardFileKinds[ext];
    return NATIVE_FILE_KINDS[ext] || NATIVE_RULE_DATA.defaultFileKind;
  }
  function stripKnownExtension(name, extensions) { var dot = name.lastIndexOf("."), ext = dot < 0 ? "" : name.slice(dot + 1).toLowerCase(); return dot >= 0 && extensions.has(ext) ? name.slice(0, dot) : name; }
  function stripViewerExtension(name) { return stripKnownExtension(name, NATIVE_STRIP_VIEWER); }
  function stripPipelineExtension(name) { return stripKnownExtension(name, NATIVE_STRIP_PIPELINE); }
  function inLens(lens, story) { var index = NATIVE_NAV_LENSES.indexOf(lens), mask = NATIVE_LENS_MASKS[(story.role || "") + "|" + (story.mark || "") + "|" + String(story.ready === true)]; return index >= 0 && !!mask && mask.charAt(index) === "1"; }
  function countLenses(stories) { var counts = Object.fromEntries(NATIVE_NAV_LENSES.map(function(lens) { return [lens, 0]; })); for (var story of stories) for (var lens of NATIVE_NAV_LENSES) if (inLens(lens, story)) counts[lens]++; return counts; }
  function stageGroupOf(story) { return NATIVE_STAGE_TABLE[(story.role || "") + "|" + (story.mark || "")] || story.role || "off"; }
  function compareNavNames(a, b) { return NATIVE_NAV_COLLATOR.compare(a, b); }
  var navMask = 0;
  function nativeGlyph(markup) {
    var template = document.createElement("template");
    template.innerHTML = markup.replaceAll("CS_MASK", "cs-kit-mask-" + (++navMask));
    return template.content.firstElementChild;
  }
  function stageRail(role, ready) { return nativeGlyph(NATIVE_RAILS[(role || "off") + ":" + !!ready]); }
  function statusMark(mark, options) {
    options = options || {};
    var size = options.size === 6 ? 6 : options.size === 8 ? 8 : 7;
    var markup = NATIVE_MARKS[mark + ":" + size];
    return nativeGlyph(options.still ? '<span class="cs-mark-still inline-flex">' + markup + "</span>" : markup);
  }
  function fileKindIcon(kind) { return nativeGlyph(NATIVE_FILES[kind]); }
  function navGlyph(name) { return nativeGlyph(NATIVE_NAV_GLYPHS[name]); }
  /*@native-nav-end*/

  /*@kit-core-start*/
  // The generic dashboard patterns: one boot, a page-wide read budget, paced reads, honest values, batch and
  // visible-only totals, cached queries, per-section loading and keyed regions. ONE source: scripts/gen-dashboard-kit-core.mjs
  // splices this file, unchanged, into dashboard_kit/kit.js (in-app, window.CS and the in-app kit object) and toolkit/cs-live.js
  // (hosted, CS). Each host registers an adapter; the first core on a page wins and a second file adopts it, so a page that
  // loads both has one budget and one cache. Timers and the clock are read at call time, so fake clocks drive every piece.
  // paced, honest, totals, focus and patches are promoted from the context repo's dashboards/_shared and story-journey.
  var csCore = window.__csKitCore || (window.__csKitCore = (function () {
    var core = { version: 4, adapters: {} };
    function now() { return Date.now(); }
    function later(fn, ms) { return window.setTimeout(fn, ms); }
    function unlater(handle) { window.clearTimeout(handle); }
    var versionSerial = 0;
    // A version for content: its JSON, or, when that cannot be made (circular, BigInt), one that is new every time so the row repaints.
    function jsonOr(v) {
      try {
        var text = JSON.stringify(v);
        return text === undefined ? "" : text;
      } catch (e) {
        return "#" + (++versionSerial);
      }
    }
    function stable(v) {
      if (v && typeof v === "object") {
        if (Array.isArray(v)) return "[" + v.map(stable).join(",") + "]";
        return "{" + Object.keys(v).sort().map(function (k) { return JSON.stringify(k) + ":" + stable(v[k]); }).join(",") + "}";
      }
      return JSON.stringify(v === undefined ? null : v);
    }
    function el(tag, cls, text) {
      var node = document.createElement(tag);
      if (cls) node.className = cls;
      if (text != null) node.textContent = text;
      return node;
    }
    var styled = false;
    function style() {
      if (styled || !document.head) return;
      styled = true;
      var s = document.createElement("style");
      s.setAttribute("data-cs-kit-core", "");
      s.textContent = ".cs-section-body{margin-top:8px}.cs-section-skeleton{display:grid;gap:6px}.cs-section-skeleton>span{height:12px;border-radius:4px;background:var(--cs-surface-2,rgba(128,128,128,.2))}" +
        ".cs-section-meta button{margin-left:6px}.cs-section-meta,.cs-section-error{font-size:12px;color:var(--cs-muted,#888);margin-top:6px}.cs-section-error{color:var(--cs-danger,#c0392b)}.cs-value[data-state=waiting],.cs-value[data-state=pending],.cs-value[data-state=loading],.cs-value[data-state=unknown],.cs-value[data-state=error],.cs-value[data-state=stale]{color:var(--cs-muted,#888)}" +
        ".cs-kit-banner{font-size:12px;padding:6px 10px;border-radius:6px;background:var(--cs-surface-2,rgba(128,128,128,.15))}.cs-kit-meter{position:fixed;right:8px;bottom:8px;z-index:2147483000;font:11px/1.4 ui-monospace,monospace;padding:4px 8px;border-radius:6px;background:#000c;color:#fff}";
      document.head.appendChild(s);
    }

    // ── Hosts and K1 boot ──────────────────────────────────────────────────
    // The host is the adapter whose bridge object is present, never a guess from the DOM (script[src], currentScript, file name).
    core.register = function (name, adapter) { adapter.name = name; core.adapters[name] = adapter; };
    function pick() {
      var app = core.adapters.app;
      return app && app.present() ? app : core.adapters.hosted || app;
    }
    core.host = function () { return pick().name; };

    // ── K9 budget: every kit read goes through one page-wide queue ─────────
    // inFlight reads at once; reads in the same slot start at least minGapMs apart (a refresh burst is spaced out); a read that
    // does not answer in timeoutMs (default: the engine's own 900 s backstop, so a slow but legitimate read is never cut off; an ask
    // can pass its own timeoutMs) frees its slot; cancelling a read frees it at once and stops the host retrying it. The Mac answering
    // busy is "busy": pages say "Waiting for the Mac", never an error. Reads merely queued behind a full budget are "waiting".
    var cfg = { inFlight: 2, minGapMs: 2000, bytesPerMinute: 0, timeoutMs: 900000 };
    var running = 0, calls = 0, bytes = 0, queue = [], sizes = [], slotStarts = new Map(), gate = null, meter = null, watchers = [], busyCount = 0;
    var BUSY_CODE = /\b(?:mac_busy|busy|rate_limited)\b/;
    function superseded() { return Object.assign(new Error("superseded"), { superseded: true }); }
    function cancelled() { return Object.assign(new Error("cancelled"), { superseded: true, cancelled: true }); }
    core.isSuperseded = function (e) { return !!(e && e.superseded); };
    // THE code table: every code a read can throw, tagged refusal (the page may not ask this), transient (the Mac, the relay or the wire may
    // answer next time) or other, with its sentence. The hosted codes are the R23 list (LIVE_ERRORS in live_data_authoring.rs: a test fails
    // when a code is missing here); the in-app forms follow it. A "verb" refusal refuses the whole verb for this page, not some ids.
    // A code is its prefix: the wire answers a bare code, the app bridge "code: detail" or free text.
    var GENERIC_ERROR = "Could not read this", REFUSED = "This page can't show this", SILENT = "The Mac didn't answer in time", BUSY_TEXT = "The Mac is busy";
    var CODES = {
      arg_not_allowed: ["refusal", REFUSED], query_not_declared: ["refusal", REFUSED, "verb"], query_invalid: ["refusal", REFUSED],
      access_ended: ["refusal", "Access to this dashboard has ended", "verb"],
      invalid_args: ["refusal", REFUSED], not_approved: ["refusal", REFUSED], not_declared: ["refusal", REFUSED, "verb"],
      undeclared: ["refusal", REFUSED, "verb"], refused: ["refusal", REFUSED],
      mac_offline: ["transient", "The Mac is offline"], mac_busy: ["transient", BUSY_TEXT], busy: ["transient", BUSY_TEXT],
      rate_limited: ["transient", BUSY_TEXT], mac_timeout: ["transient", SILENT], no_answer: ["transient", SILENT], timeout: ["transient", SILENT],
      answer_failed: ["other", "The Mac couldn't answer"], answer_too_large: ["other", "Too much to show at once"],
      stale_cursor: ["other", "The list changed while loading"], bad_request: ["other", GENERIC_ERROR],
      post_to_own_window: ["other", GENERIC_ERROR], not_owner: ["other", GENERIC_ERROR], needs_gesture: ["other", GENERIC_ERROR],
      failed: ["other", "The Mac couldn't read this"], omitted: ["other", GENERIC_ERROR], no_totals: ["other", GENERIC_ERROR],
    };
    function lookup(e) {
      var code = typeof e === "string" ? e : e instanceof Error ? e.message : "";
      var m = /^([a-z_]+)(?::|$)/.exec(code), hit = m && Object.prototype.hasOwnProperty.call(CODES, m[1]) ? CODES[m[1]] : null;
      if (hit) return { kind: hit[0], text: hit[1], verb: hit[2] === "verb" };
      if (/is not allowlisted$/.test(code)) return { kind: "refusal", text: REFUSED, verb: true };
      if (/^storyTotals: /.test(code)) return { kind: "refusal", text: REFUSED, verb: false };
      return { kind: "other", text: GENERIC_ERROR, verb: false };
    }
    // A read's error as a sentence, never a raw code or [object Object]: a known code gets its own sentence; anything else (and any
    // value that is neither an Error nor a string) is "Could not read this".
    core.errorText = function (e) { return lookup(e).text; };
    core.errorKind = function (e) { return lookup(e).kind; };
    core.isRefusal = function (e) { return lookup(e).kind === "refusal"; };
    core.isVerbRefusal = function (e) { return lookup(e).verb; };
    core.errorCodes = function () {
      var out = {};
      Object.keys(CODES).forEach(function (code) { out[code] = CODES[code][0]; });
      return out;
    };
    core.isBusy = function (e) { return BUSY_CODE.test(String((e && e.message) || e)); };
    function waiting() { return busyCount > 0 || queue.some(function (q) { return !q.held; }); }
    core.waiting = waiting;
    // The hosts say when a read is being asked again because the Mac or the relay answered busy.
    // With a signal (one budgeted read's), the note is idempotent: cancelling or timing out the read clears it at once.
    core.noteBusy = function (on, signal) {
      if (signal) {
        if (!!signal.busy === on) return;
        signal.busy = on;
      }
      busyCount = Math.max(0, busyCount + (on ? 1 : -1));
      changed();
    };
    core.busy = function () { return busyCount > 0; };
    function stats() { return { calls: calls, bytes: bytes, inFlight: running, queued: queue.length, waiting: waiting(), busy: busyCount > 0 }; }
    function changed() {
      if (!meter && /[?&]csmeter=1(&|$)/.test(window.location.search) && document.body) {
        style();
        meter = el("div", "cs-kit-meter");
        document.body.appendChild(meter);
      }
      var s = stats();
      if (meter) meter.textContent = "reads " + s.calls + " · " + s.bytes + " B · in flight " + s.inFlight + " · queued " + s.queued;
      watchers.slice().forEach(function (cb) { cb(s); });
    }
    function windowBytes() {
      var cut = now() - 60000;
      sizes = sizes.filter(function (r) { return r.at > cut; });
      return sizes.reduce(function (sum, r) { return sum + r.n; }, 0);
    }
    // A watcher (or anything it calls) that pumps while a pump runs is deferred to the end of that pump, never recursed into.
    var pumping = false, pumpAgain = false;
    function pump() {
      if (pumping) { pumpAgain = true; return; }
      pumping = true;
      try {
        for (var rounds = 0; rounds < 5; rounds++) {
          pumpAgain = false;
          pumpOnce();
          if (!pumpAgain) break;
        }
      } finally {
        pumping = false;
      }
    }
    function pumpOnce() {
      var wake = Infinity;
      for (var i = 0; i < queue.length && running < cfg.inFlight;) {
        var item = queue[i];
        if (item.stale && item.stale()) { queue.splice(i, 1); item.fail(superseded()); continue; }
        var gap = item.slot && slotStarts.has(item.slot) ? slotStarts.get(item.slot) + cfg.minGapMs - now() : 0;
        var bytesWait = cfg.bytesPerMinute && windowBytes() >= cfg.bytesPerMinute ? 1000 : 0;
        item.held = gap > 0 && !bytesWait;
        if (gap > 0 || bytesWait) { wake = Math.min(wake, Math.max(gap, bytesWait)); i++; continue; }
        queue.splice(i, 1);
        start(item);
      }
      unlater(gate);
      gate = wake < Infinity ? later(function () { gate = null; pump(); }, wake) : null;
      changed();
    }
    function start(item) {
      running++;
      calls++;
      if (item.slot) {
        slotStarts.set(item.slot, now());
        if (slotStarts.size > 500) slotStarts.forEach(function (t, k) { if (now() - t > cfg.minGapMs) slotStarts.delete(k); });
      }
      var done = false, timer = later(function () { settle(function () { item.fail(Object.assign(new Error("timeout"), { timeout: true })); }, true); }, item.timeoutMs || cfg.timeoutMs);
      // `stop`: the read is abandoned (cancelled or timed out): the host must not ask it again, and its busy note is cleared at once.
      function settle(finish, stop) {
        if (done) return;
        done = true;
        unlater(timer);
        running--;
        item.cancel = function () {};
        if (stop) {
          item.signal.cancelled = true;
          core.noteBusy(false, item.signal);
        }
        finish();
        pump();
      }
      item.cancel = function () { settle(function () { item.fail(cancelled()); }, true); };
      item.run(item.signal).then(function (m) {
        settle(function () {
          if (cfg.bytesPerMinute || meter) {
            var n = 0;
            try { n = JSON.stringify(m && m.answer !== undefined ? m.answer : m).length; } catch (e) { /* not countable */ }
            bytes += n;
            sizes.push({ at: now(), n: n });
          }
          item.ok(m);
        });
      }, function (e) { settle(function () { item.fail(e); }); });
    }
    // opts.slot: a newer ask in the same slot drops an older one still queued. opts.stale(): true drops the ask at its turn.
    // opts.timeoutMs: this ask's own timeout. opts.box: {cancel} is filled in; box.cancel() drops the ask if queued, or frees its
    // slot if it is running. run(signal) gets {cancelled, busy}: a host retrying the read stops when signal.cancelled is set.
    function schedule(run, opts) {
      opts = opts || {};
      if (opts.slot) {
        queue = queue.filter(function (q) {
          if (q.slot !== opts.slot) return true;
          q.fail(superseded());
          return false;
        });
      }
      return new Promise(function (ok, fail) {
        var item = { run: run, ok: ok, fail: fail, slot: opts.slot, stale: opts.stale, timeoutMs: opts.timeoutMs, held: false, signal: { cancelled: false, busy: false } };
        item.cancel = function () {
          var i = queue.indexOf(item);
          if (i < 0) return;
          queue.splice(i, 1);
          fail(cancelled());
          changed();
        };
        if (opts.box) opts.box.cancel = function () { item.cancel(); };
        queue.push(item);
        pump();
      });
    }
    core.budget = function (opts) {
      // A bad value (zero, negative, NaN) keeps the current setting: inFlight is at least 1 (the default is 2).
      // Only a changed setting pumps: budget() with no or unchanged options has no side effect, so a watcher may call it freely.
      var before = JSON.stringify(cfg);
      if (opts) Object.keys(opts).forEach(function (k) {
        var n = Number(opts[k]);
        if (k === "inFlight" && n >= 1) cfg.inFlight = Math.floor(n);
        else if (k === "timeoutMs" && n > 0) cfg.timeoutMs = n;
        else if ((k === "minGapMs" || k === "bytesPerMinute") && n >= 0) cfg[k] = n;
      });
      if (JSON.stringify(cfg) !== before) pump();
      return {
        config: function () { return Object.assign({}, cfg); },
        stats: stats,
        // "idle" or "waiting" (the Mac answered busy, or reads are queued behind the budget).
        state: function () { return waiting() ? "waiting" : "idle"; },
        onChange: function (cb) {
          watchers.push(cb);
          return function () { watchers = watchers.filter(function (w) { return w !== cb; }); };
        },
      };
    };
    // Cancel handles of the asks a caller made, kept in the caller's list while they are in flight: an ask removes its box when it settles.
    core.tracked = function (boxes, ask) {
      var box = {};
      boxes.push(box);
      var done = function () { var i = boxes.indexOf(box); if (i >= 0) boxes.splice(i, 1); };
      var promise = ask(box);
      promise.then(done, done);
      return promise;
    };
    // The answer message ({answer, source, answered_at}) of one read. The adapter's ask gets opts too (opts.paging: no relay retry).
    core.ask = function (name, args, opts) {
      return schedule(function (signal) { return pick().ask(name, args || {}, Object.assign({}, opts, { signal: signal })); }, opts);
    };
    core.read = function (name, args, opts) { return core.ask(name, args, opts).then(function (m) { return m.answer; }); };

    // ── K2 paced reads (paced.js) ──────────────────────────────────────────
    var PACED_STALE = /stale_cursor/;
    // One "the Mac is busy" signal per paced owner, however many times its read is asked again.
    function busyNote() {
      var on = false;
      return function (v) { if (v !== on) { on = v; core.noteBusy(v); } };
    }
    function createPaced(o) {
      var run = o.run, minGapMs = o.minGapMs == null ? 2000 : o.minGapMs, ttlMs = o.ttlMs || 0, onBusy = o.onBusy || function () {};
      var retryDelays = o.retryDelays || [1000, 2000, 4000, 8000], staleTries = o.staleTries == null ? 3 : o.staleTries;
      var stopped = false, gapTimer = null, busyTimer = null, wakeBusy = null, going = false, wanted = false, lastStart = -Infinity, lastOk = -Infinity, waiters = [];
      function pause(ms) {
        return new Promise(function (resolve) { wakeBusy = resolve; busyTimer = later(function () { busyTimer = null; wakeBusy = null; resolve(); }, ms); });
      }
      async function attempt() {
        var busyTries = 0, staleCount = 0;
        while (!stopped) {
          try {
            var out = await run();
            onBusy(false);
            lastOk = now();
            return out;
          } catch (error) {
            var code = String((error && error.message) || error);
            // A dead cursor restarts the read from the top, without a word to the page.
            if (PACED_STALE.test(code) && staleCount++ < staleTries) continue;
            if (BUSY_CODE.test(code)) { onBusy(true); await pause(retryDelays[Math.min(busyTries++, retryDelays.length - 1)]); continue; }
            onBusy(false);
            throw error;
          }
        }
      }
      function begin() {
        gapTimer = null;
        if (stopped || going) return;
        wanted = false;
        lastStart = now();
        going = true;
        var group = waiters;
        waiters = [];
        attempt().then(function (v) { group.forEach(function (w) { w.ok(v); }); }, function (e) { group.forEach(function (w) { w.fail(e); }); })
          .finally(function () { going = false; if (wanted && !stopped) plan(); });
      }
      function plan() {
        if (gapTimer || going || stopped) return;
        var wait = Math.max(0, lastStart + minGapMs - now());
        if (wait === 0) begin(); else gapTimer = later(begin, wait);
      }
      // Requests made while a run is going, or inside the gap, share the next run. {force:false} with a ttl answers at once
      // when the last good run is younger than ttlMs and nothing else is waiting.
      function request(r) {
        var force = !!(r && r.force);
        if (stopped) return Promise.resolve();
        if (!force && ttlMs > 0 && now() - lastOk < ttlMs && !wanted && !going) return Promise.resolve();
        wanted = true;
        var promise = new Promise(function (ok, fail) { waiters.push({ ok: ok, fail: fail }); });
        plan();
        return promise;
      }
      function stop() {
        stopped = true;
        onBusy(false);
        unlater(gapTimer);
        unlater(busyTimer);
        gapTimer = busyTimer = null;
        if (wakeBusy) wakeBusy();
        wakeBusy = null;
        var list = waiters;
        waiters = [];
        list.forEach(function (w) { w.ok(); });
      }
      return { request: request, stop: stop, get running() { return going; } };
    }
    // paced(query, {key, limit, inFlight, ttlMs, minGapMs, margin}): query is a read name (budgeted) or (args) => Promise.
    // request(args) runs now; observe(el, args) runs when the element nears the viewport, at most `inFlight` (3) at once.
    core.paced = function (query, o) {
      o = o || {};
      var owners = new Map(), observed = new Map(), waiting = [], going = 0, cap = o.inFlight || 3, observer = null, stopped = false, asked = new Set(), boxes = [];
      function owner(args) {
        var k = stable(args);
        if (!owners.has(k)) {
          var runQuery = typeof query === "function" ? function () { return query(args); }
            : function () {
              return core.tracked(boxes, function (box) { return core.read(query, Object.assign({}, args, o.limit ? { limit: o.limit } : {}), { slot: (o.key || query) + k, box: box }); });
            };
          owners.set(k, createPaced({ run: runQuery, minGapMs: o.minGapMs, ttlMs: o.ttlMs, onBusy: busyNote() }));
        }
        return owners.get(k);
      }
      function pump() {
        while (going < cap && waiting.length) {
          var args = waiting.shift();
          going++;
          owner(args).request().catch(function () {}).finally(function () { going--; pump(); });
        }
      }
      // An element's args are read once, however often it scrolls back into view; refresh() reads them again.
      // Nodes that left the document are no longer watched. A node built before it is attached is kept: only a node once seen
      // connected and now detached is forgotten.
      var wasConnected = new WeakSet();
      function forgetDetached() {
        observed.forEach(function (args, node) {
          if (node.isConnected) { wasConnected.add(node); return; }
          if (!wasConnected.has(node)) return;
          if (observer) observer.unobserve(node);
          observed.delete(node);
        });
      }
      function enqueue(args) {
        var k = stable(args);
        if (asked.has(k)) return;
        asked.add(k);
        if (!waiting.some(function (a) { return stable(a) === k; })) waiting.push(args);
        pump();
      }
      function watch() {
        if (observer || typeof IntersectionObserver === "undefined") return observer;
        observer = new IntersectionObserver(function (entries) {
          forgetDetached();
          entries.forEach(function (e) { if (e.isIntersecting && observed.has(e.target)) enqueue(observed.get(e.target)); });
        }, { rootMargin: (o.margin == null ? 200 : o.margin) + "px" });
        return observer;
      }
      return {
        request: function (args, r) { return owner(args || {}).request(r); },
        observe: function (node, args) {
          if (stopped) return;
          forgetDetached();
          observed.set(node, args || {});
          if (node.isConnected) wasConnected.add(node);
          if (watch()) observer.observe(node); else enqueue(args || {});
        },
        // The next run of every owner that has run or is waiting, shared by requests already queued.
        refresh: function () { asked.clear(); owners.forEach(function (p) { p.request({ force: true }).catch(function () {}); }); },
        stop: function () {
          stopped = true;
          if (observer) observer.disconnect();
          owners.forEach(function (p) { p.stop(); });
          boxes.splice(0).forEach(function (b) { if (b.cancel) b.cancel(); });
          observed.clear();
          waiting = [];
        },
        get running() { return going; },
      };
    };

    // ── K3 honest states (honest.js, freshness.js) ─────────────────────────
    var LOADING = "Loading…", PENDING = "Counting…", WAITING = "Waiting for the Mac", UNKNOWN = "Unknown", NOT_LOADED = "Not loaded";
    function sentence(reason) { return reason ? String(reason)[0].toUpperCase() + String(reason).slice(1) : ""; }
    function unknownLabel(reason) { return reason ? UNKNOWN + " · " + sentence(reason) : UNKNOWN; }
    function reasonAt(reasons, index) { return Array.isArray(reasons) && Number.isInteger(index) && typeof reasons[index] === "string" ? reasons[index] : ""; }
    function distinctReasons(texts) {
      return Array.from(new Set(texts.filter(function (t) { return typeof t === "string" && t; })));
    }
    function dataAge(answeredAt) { return answeredAt == null ? null : Math.max(0, Math.floor((now() - answeredAt) / 60000)); }
    function freshnessNote(o) {
      var age = o.dataAge == null ? "the last saved data" : "data from " + (o.dataAge === 0 ? "less than a minute" : o.dataAge + " minute" + (o.dataAge === 1 ? "" : "s")) + " ago";
      if (o.offline) return o.hasData ? "The owner's Mac is offline. Showing " + age + ". The page updates when it's back." : "The owner's Mac is offline. The page updates when it's back.";
      if (o.stale) return o.dataAge == null ? "Showing cached data. The page updates when fresh data is available." : "Showing cached " + age + ". The page updates when fresh data is available.";
      return null;
    }
    // A figure is exactly one of: known (a 0 stays a 0), Waiting for the Mac (the Mac is busy: waiting, or an error that says busy),
    // Counting… (pending), Loading…, Unknown · reason, Not loaded (read failed).
    // A missing value is never turned into 0, a bare dash or "at least 0".
    core.valueState = function (v, o) {
      o = o || {};
      if (v !== null && v !== undefined && !(typeof v === "number" && !isFinite(v))) {
        var text = o.format ? o.format(v) : String(v);
        var late = o.stale || o.offline;
        return { state: late ? "stale" : "known", text: late && o.ageMs != null ? text + " · " + Math.round(o.ageMs / 60000) + " min ago" : text };
      }
      if (o.waiting || (o.error && core.isBusy(o.error))) return { state: "waiting", text: WAITING };
      if (o.pending) return { state: "pending", text: PENDING };
      if (o.loading) return { state: "loading", text: LOADING };
      if (o.error) {
        var why = o.error === true ? GENERIC_ERROR : core.errorText(o.error);
        return { state: "error", text: why === GENERIC_ERROR ? NOT_LOADED : NOT_LOADED + " · " + why };
      }
      return { state: "unknown", text: unknownLabel(o.unavailable) };
    };
    core.value = function (v, o) {
      var s = core.valueState(v, o);
      var node = el("span", "cs-value", s.text);
      node.setAttribute("data-state", s.state);
      if (s.state === "error" && o && o.onRetry) {
        var b = el("button", "cs-btn is-sm", "Retry");
        b.type = "button";
        b.addEventListener("click", o.onRetry);
        node.append(" ", b);
      }
      style();
      return node;
    };
    core.stateBanner = function (o) {
      o = o || {};
      var text = freshnessNote({ offline: o.offline, stale: o.stale, dataAge: dataAge(o.asOf), hasData: o.hasData !== false });
      if (!text) return null;
      var node = el("div", "cs-kit-banner", text);
      node.setAttribute("role", "status");
      style();
      return node;
    };
    // "Totals loading · 120 of 770 stories": a partial total is progress, never a figure.
    core.partial = function (known, total, noun) { return known >= total ? "" : "Totals loading · " + known + " of " + total + " " + (noun || "stories"); };

    // ── K4 batch totals (totals.js) ────────────────────────────────────────
    var TOTALS_FOLLOW_DELAYS = [2000, 3000, 5000, 8000], TOTALS_FOLLOW_LIMIT = 6;
    // What a storyTotals answer says, per story: ready (the engine's totals, nulls kept, each reason resolved to text), pending
    // (not indexed yet) or error. Pacing, busy backoff and debounce belong to paced and the budget; this only says what to ask next.
    function createTotalsReader(o) {
      var read = o.read, getIds = o.getIds, retryable = o.retryable, name = o.name || "storyTotals", warnedPin = false, everReady = false, dirty = new Map(), dirtyGen = 0, askedDirty = new Map(), verbRefused = false, byId = new Map(), indexing = null, answeredAt = null, source = null, loaded = false, follows = 0;
      function resolved(story, list) {
        var out = {};
        Object.keys(story.unavailable || {}).forEach(function (field) {
          var text = reasonAt(list, story.unavailable[field]);
          if (text) out[field] = text;
        });
        return out;
      }
      // A failure keeps a story's last totals; it is asked again only when it is one the Mac or the wire clears by itself.
      function failed(before, had, code) {
        var again = !retryable || retryable({ kind: "error", error: code });
        return had ? Object.assign({}, before, { refreshing: again, kept: true, error: code }) : { kind: "error", error: code };
      }
      // A story answered again no longer carries the failure that kept its old totals.
      function withoutFailure(entry) {
        var copy = Object.assign({}, entry);
        delete copy.kept;
        delete copy.error;
        return copy;
      }
      function facet(story, key) { return key in story ? (Number.isFinite(story[key]) ? story[key] : null) : undefined; }
      function apply(answer, asked, gens) {
        var list = Array.isArray(answer && answer.reasons) ? answer.reasons : [];
        asked.forEach(function (id) {
          // A successful answer settles only the event this ask was made for: one that arrived during the read stays dirty.
          if (dirty.has(id) && dirty.get(id) === gens.get(id)) dirty.delete(id);
          var story = answer && answer.stories && answer.stories[id], before = byId.get(id), had = !!before && before.kind === "ready";
          if (!story || typeof story !== "object" || Array.isArray(story)) byId.set(id, failed(before, had, "omitted"));
          else if (story.pending === true) byId.set(id, had ? Object.assign(withoutFailure(before), { refreshing: true }) : { kind: "pending" });
          else if (story.error) byId.set(id, failed(before, had, String(story.error)));
          else if (!story.totals || typeof story.totals !== "object") byId.set(id, failed(before, had, "no_totals"));
          // The totals are story_changes' totals, field for field: a null stays null, and nothing here turns it into a 0.
          else byId.set(id, {
            kind: "ready", totals: Object.assign({}, story.totals), unavailable: resolved(story, list),
            computedAtMs: story.computedAtMs == null ? null : story.computedAtMs, refreshing: story.refreshing === true,
            mergedAt: "mergedAt" in story ? (Number.isFinite(story.mergedAt) ? story.mergedAt : null) : undefined,
            mergedAtReason: typeof story.mergedAtReason === "string" ? story.mergedAtReason : "",
            // The activity facets pass through as mergedAt does: a finite number or null when the entry names the key, never a made-up zero.
            firstActivityAt: facet(story, "firstActivityAt"), lastActivityAt: facet(story, "lastActivityAt"), openSessions: facet(story, "openSessions"),
            activityReason: typeof story.activityReason === "string" ? story.activityReason : undefined,
            prs: Array.isArray(story.prs) ? story.prs.filter(function (p) { return p && Number.isFinite(p.number); }).map(function (p) { return { number: p.number, merged: Number.isFinite(p.merged) ? p.merged : null }; }) : undefined,
            prsTotal: Number.isFinite(story.prsTotal) ? story.prsTotal : undefined,
            runtimes: Array.isArray(story.runtimes) ? story.runtimes.filter(function (k) { return typeof k === "string"; }) : undefined,
          });
        });
        if (answer && answer.indexing && typeof answer.indexing === "object") indexing = answer.indexing;
        if (answer && Number.isFinite(Number(answer.answered_at))) answeredAt = Number(answer.answered_at);
        source = answer && answer.source != null ? answer.source : source;
        // An answer came back, so the verb and the pin accepted this ask (ready, pending or a per-story error alike). From now on no
        // refusal is ever read as the verb or the pin being wrong for the page.
        everReady = true;
      }
      // Not settled: unseen, pending, errored, or ready but recomputing. Only ids still in the list count.
      function needs(id) {
        var s = byId.get(id);
        if (!s) return true;
        if (isRefused(s)) return false;
        if (dirty.has(id)) return true;
        if (s.kind === "ready") return s.refreshing;
        return s.kind === "pending" || !retryable || retryable(s);
      }
      function unsettled() { return getIds().filter(needs); }
      // Never attempted (an ask that threw still counts as attempted: the follow-up timer retries it), or named by a data-change event not asked yet.
      var attempted = new Set();
      // A dirty id whose event was already asked (and failed) is the follow-up timers' to retry, so a show does not ask it again.
      function unseen(id) { return !attempted.has(id) || (dirty.has(id) && askedDirty.get(id) !== dirty.get(id)); }
      // Already showing values and only recomputing on the Mac: asked on a long interval, never on the fast follow-ups.
      function slow(id) {
        var s = byId.get(id);
        return !!s && s.kind === "ready" && s.refreshing === true && !s.error && !dirty.has(id);
      }
      function fast(id) { return needs(id) && !slow(id); }
      // pull({all, restart}): every id (open, refresh, live event), or only the unsettled ones (restart: start the backoff over). One read either
      // way. pull({slow}) asks only the ready-but-recomputing ones.
      // A refused story (the page may not ask it) is left out of every automatic pull; only pull({refused: true}) asks it again.
      function isRefused(s) { return !!s && typeof s.error === "string" && core.isRefusal(s.error); }
      function markRefused(id) {
        var before = byId.get(id);
        byId.set(id, failed(before, !!before && before.kind === "ready", "refused"));
      }
      async function pull(p) {
        var refusedToo = !!(p && p.refused);
        if (refusedToo) verbRefused = false;
        var all = !p || (p.all !== false && !p.slow);
        var asked = p && p.ids ? p.ids.filter(function (id) { return !isRefused(byId.get(id)); })
          : p && p.slow ? getIds().filter(slow)
          : all ? getIds().filter(function (id) { return refusedToo || !isRefused(byId.get(id)); }) : unsettled();
        if (!asked.length) return;
        // Written before the read, so a failed ask is still an attempt; a dirty id stays dirty until an answer arrives.
        // gens: the event each dirty id carries now, kept by THIS ask, so an answer settles only what it was asked for.
        var gens = new Map();
        asked.forEach(function (id) {
          attempted.add(id);
          if (dirty.has(id)) { gens.set(id, dirty.get(id)); askedDirty.set(id, dirty.get(id)); } else askedDirty.delete(id);
        });
        // A new ask (open, refresh, scroll) starts the backoff over; only a follow-up counts toward it. Naming ids touches neither.
        if (p && (p.ids || p.slow)) { /* exactly these ids */ } else if (all || (p && p.restart)) follows = 0; else follows++;
        // The verb itself was refused for this page: nothing is asked again until pull({refused: true}).
        if (verbRefused) { asked.forEach(markRefused); loaded = true; return; }
        await readIds(asked, gens);
        loaded = true;
      }
      // One ask: "ok", "refused" (a refusal code: some of these ids may not be asked), "pin" (arg_not_allowed: a missing or wrong pin, which
      // refuses every id alike) or "verb" (the whole verb is refused for this page, which one ask is enough to know). Any other thrown
      // error is transient: it is thrown, whole, for the caller's backoff.
      async function attempt(ids, gens) {
        try {
          apply(await read(ids), ids, gens);
          return "ok";
        } catch (e) {
          if (core.isSuperseded(e) || !core.isRefusal(e)) throw e;
          if (core.isVerbRefusal(e)) return "verb";
          return /^arg_not_allowed(?::|$)/.test(String((e && e.message) || e)) ? "pin" : "refused";
        }
      }
      // The rule: an id is marked refused only when its own ask was refused, or the server said the whole verb is refused (a verb code).
      // The one inference, the unpinned verdict below, is only ever made before this reader has had any answer. The page-wide flag
      // (nothing is asked again) is set only then too; after that a verb code marks only its own batch, and later pulls ask normally.
      function refuseVerb(ids) {
        if (!everReady) verbRefused = true;
        ids.forEach(markRefused);
      }
      // ids were refused as one ask: halve them and ask each half once through the budget, and keep halving each half that is refused,
      // down to single ids. A refusal is never inferred for ids that were not refused themselves.
      async function isolate(ids, gens) {
        if (ids.length === 1) { markRefused(ids[0]); return; }
        var mid = Math.ceil(ids.length / 2), halves = [ids.slice(0, mid), ids.slice(mid)];
        var results = await Promise.all(halves.map(function (h) { return attempt(h, gens); }));
        // Past the first round some id may already have been accepted, so no half is read as the verb being refused: it is halved on.
        await Promise.all(halves.filter(function (h, i) { return results[i] !== "ok"; }).map(function (h) { return isolate(h, gens); }));
      }
      // ids of unknown standing (some probes were refused, so the rest may be fine): one id is asked, more are halved and asked.
      async function sift(ids, gens) {
        if (ids.length === 1) { if ((await attempt(ids, gens)) !== "ok") markRefused(ids[0]); return; }
        await isolate(ids, gens);
      }
      async function readIds(ids, gens) {
        var result = await attempt(ids, gens);
        if (result === "verb") { refuseVerb(ids); return; }
        if (result === "ok") return;
        if (result === "pin" && ids.length > 1) {
          // arg_not_allowed on many ids: probe three (first, middle, last; all of them when there are three or fewer) in one budgeted round.
          // LEAD DECISION (accepted residual risk): on a reader that has never had an answer, a batch whose first, middle and last ids are
          // ALL refused this way is treated as unpinned (every id refused, one console warning) until refresh({refused: true}). Once the
          // reader has had any answer, nothing is inferred: refused probes are marked and the rest is sifted, id by id if need be.
          var picks = [ids[0], ids[Math.floor(ids.length / 2)], ids[ids.length - 1]].filter(function (id, i, all) { return all.indexOf(id) === i; });
          var probes = await Promise.all(picks.map(function (id) { return attempt([id], gens); }));
          var accepted = probes.some(function (r) { return r === "ok"; });
          if (!accepted && !everReady && probes.every(function (r) { return r === "pin" || r === "verb"; })) {
            if (!warnedPin) {
              warnedPin = true;
              console.warn(name + " refused for every probed id: check the page's storyIds pin");
            }
            refuseVerb(ids);
            return;
          }
          picks.forEach(function (id, i) { if (probes[i] !== "ok") markRefused(id); });
          var left = ids.filter(function (id) { return picks.indexOf(id) < 0; });
          if (!left.length) return;
          // Nothing answered yet on this reader: the rest is read fresh. Otherwise the rest is sifted: never read as a whole, never given
          // a verdict, and a single remaining id is asked rather than deduced.
          if (!accepted && !everReady) await readIds(left, gens); else await sift(left, gens);
          return;
        }
        await isolate(ids, gens);
      }
      // The delay before the next follow-up, or null when every story is settled or the limit is reached for ones that may not settle.
      function nextDelay() {
        var open = unsettled();
        if (!open.length) return null;
        var onlyPending = open.every(function (id) { return ((byId.get(id) || {}).kind || "pending") === "pending"; });
        if (!onlyPending && follows >= TOTALS_FOLLOW_LIMIT) return null;
        return TOTALS_FOLLOW_DELAYS[Math.min(follows, TOTALS_FOLLOW_DELAYS.length - 1)];
      }
      return {
        pull: pull, nextDelay: nextDelay, unsettled: unsettled, needs: needs,
        unseenIds: function () { return getIds().filter(unseen); }, isFast: fast, fastIds: function () { return getIds().filter(fast); }, slowIds: function () { return getIds().filter(slow); },
        get: function (id) { return byId.get(id); },
        get loaded() { return loaded; }, get indexing() { return indexing; }, get answeredAt() { return answeredAt; }, get source() { return source; },
        known: function (ids) { return ids.filter(function (id) { return (byId.get(id) || {}).kind === "ready"; }).length; },
        reasons: function (ids) { return distinctReasons(ids.flatMap(function (id) { return Object.values((byId.get(id) || {}).unavailable || {}); })); },
        // A data-change event named these ids: they are asked again when visible (or when they scroll in), and nothing else is.
        // Only ids this reader has answered for can go stale, and the set is capped: an id never seen is dropped.
        markDirty: function (ids) { ids.forEach(function (id) { if (byId.has(id) && (dirty.has(id) || dirty.size < 1000)) dirty.set(id, ++dirtyGen); }); },
        forget: function () { byId.clear(); dirty.clear(); askedDirty.clear(); attempted.clear(); verbRefused = false; everReady = false; indexing = null; loaded = false; follows = 0; },
      };
    }
    function totalsRead(query, slot, boxes) {
      // `boxes` (a list owned by the caller) collects each ask's cancel handle, so stopping the caller frees its budget slots.
      var own = boxes || [];
      return function (part) {
        if (query !== "storyTotals") return core.tracked(own, function (box) { return core.read(query, { storyIds: part }, { slot: slot, box: box }); });
        // The host sizes and merges its own asks; each one goes through the budget.
        return pick().storyTotals(part, function (ids) { return core.tracked(own, function (box) { return core.ask("storyTotals", { storyIds: ids }, { slot: slot && slot + stable(ids), box: box }); }); });
      };
    }
    // batchTotals(ids | () => ids, {query: 'storyTotals'}): a reader. pull() makes ONE storyTotals call per <= 500 ids.
    core.batchTotals = function (ids, o) {
      o = o || {};
      return createTotalsReader({ read: o.read || totalsRead(o.query || "storyTotals", o.slot, o.boxes), getIds: typeof ids === "function" ? ids : function () { return ids; }, retryable: o.retryable, name: o.query || "storyTotals" });
    };
    // K4+: totals for the rows on screen only. Visible ids are batched and debounced, each id cached with its computedAtMs;
    // pending ids are asked again with backoff while visible, refreshing ones on a long interval; an id that was never in view is never
    // asked, and a show asks nothing for an id already asked.
    var FOLLOW_MS = [2000, 4000, 8000, 16000], SLOW_MS = [60000, 120000, 240000];
    var totalsHandles = new Set(), totalsSerial = 0;
    core.visibleTotals = function (container, o) {
      o = o || {};
      var attr = o.idAttr || "data-id", debounce = o.debounceMs == null ? 150 : o.debounceMs, visible = new Set(), timer = null, follow = null, stopped = false, tries = 0, io = null, mo = null, named = new Set(), namedTimer = null, chasing = new Set(), chaseTimer = null, chaseTries = 0, slowTimer = null, slowTries = 0, boxes = [], watched = new Set();
      // A story the engine refused (a pin rejection, an unknown id) is not asked again; only busy and transient failures are.
      var reader = core.batchTotals(function () { return Array.from(visible); }, { query: o.query, slot: "visibleTotals:" + (o.slot || attr) + "#" + (++totalsSerial), retryable: function (entry) { return core.errorKind(String(entry.error)) === "transient"; }, boxes: boxes });
      function idOf(row) { return row.getAttribute(attr); }
      var pulls = 0;
      // p: {all, restart, refused}, {ids, restart} or {slow}.
      async function pull(p) {
        if (stopped || !visible.size) return;
        if (p.restart) tries = 0;
        pulls++;
        try {
          await reader.pull(p);
        } catch (e) {
          // A superseded ask is not an answer, but the follow-up it was part of still has to be armed.
          if (!core.isSuperseded(e) && o.onError) o.onError(e);
        } finally {
          pulls--;
        }
        if (stopped) return;
        if (o.onChange) o.onChange(handle);
        arm();
      }
      // A named id still pending or failed belongs to its own chase backoff, so the list's follow-up leaves it out.
      function ownFast() { return reader.fastIds().filter(function (id) { return !chasing.has(id); }); }
      // Pending and retryable-error ids are asked again while visible, after 2, 4, 8 then every 16 s, for as long as they stay on screen.
      // Rows that already show values and are only recomputing (refreshing) are asked at most every 60 s, backing off to 240 s.
      function arm() {
        unlater(follow);
        follow = null;
        armSlow();
        if (stopped || !visible.size || !ownFast().length) { tries = 0; return; }
        // Follow-ups pause while the page is hidden; showing it only starts the timers again: no read fires on return.
        if (document.hidden) return;
        follow = later(function () {
          follow = null;
          if (document.hidden || pulls) return;
          // A try is counted only when an ask is made; rows that left the screen meanwhile start the backoff over.
          var own = ownFast();
          if (!own.length) { tries = 0; return; }
          tries++;
          pull({ ids: own });
        }, FOLLOW_MS[Math.min(tries, FOLLOW_MS.length - 1)]);
      }
      // An armed slow timer is never pushed out; hiding drops it, so a show waits a whole fresh interval.
      function armSlow() {
        if (stopped || document.hidden || !visible.size || !reader.slowIds().length) { unlater(slowTimer); slowTimer = null; slowTries = 0; return; }
        if (slowTimer) return;
        slowTimer = later(function () {
          slowTimer = null;
          if (document.hidden) return;
          if (pulls || !reader.slowIds().length) { armSlow(); return; }
          slowTries = Math.min(slowTries + 1, SLOW_MS.length - 1);
          pull({ all: false, slow: true });
        }, SLOW_MS[slowTries]);
      }
      function onShown() {
        if (stopped) return;
        if (document.hidden) { unlater(slowTimer); slowTimer = null; return; }
        if (!follow) arm();
        if (!chaseTimer && chasing.size) chase();
        if (!namedTimer && named.size) namedTimer = later(flushNamed, debounce);
      }
      document.addEventListener("visibilitychange", onShown);
      // Rows coming into view are asked only if never attempted (or named while off screen); the others belong to the follow-up timers,
      // and ids a live event named while visible belong to flushNamed, so a show, whose observer entries land on every row, makes no ask.
      function plan() {
        unlater(timer);
        timer = later(function () {
          timer = null;
          var fresh = reader.unseenIds().filter(function (id) { return !named.has(id); });
          if (fresh.length) pull({ ids: fresh, restart: true });
        }, debounce);
      }
      // New rows are watched as they arrive; without an IntersectionObserver every row counts as visible.
      function sweep() {
        var present = new Set();
        container.querySelectorAll(o.rowSelector).forEach(function (row) {
          var id = idOf(row);
          if (id) present.add(id);
          if (row.__csTotals) return;
          row.__csTotals = true;
          watched.add(row);
          if (io) io.observe(row); else if (id) visible.add(id);
        });
        // Rows that left the container are no longer watched.
        watched.forEach(function (row) { if (!container.contains(row)) { if (io) io.unobserve(row); watched.delete(row); row.__csTotals = false; } });
        visible.forEach(function (id) { if (!present.has(id)) visible.delete(id); });
        if (!io) plan();
      }
      if (typeof IntersectionObserver !== "undefined") {
        io = new IntersectionObserver(function (entries) {
          entries.forEach(function (e) {
            var id = idOf(e.target);
            if (!id) return;
            if (e.isIntersecting) visible.add(id); else visible.delete(id);
          });
          plan();
          // A pending row that scrolls back in is picked up by the follow-up timer, without an ask of its own.
          if (!follow) arm();
        }, { root: o.root || null });
      }
      if (typeof MutationObserver !== "undefined") { mo = new MutationObserver(sweep); mo.observe(container, { childList: true, subtree: true }); }
      sweep();
      // Named ids still pending or failed in transit are asked again, only they, after 2, 4, 8, then every 16 s while visible. A named id
      // that answers refreshing already shows values, so it leaves this backoff for the slow interval. This backoff is theirs alone: it
      // never touches the list's own follow-ups, nor theirs.
      function chase() {
        chasing.forEach(function (id) { if (!visible.has(id) || !reader.isFast(id)) chasing.delete(id); });
        if (stopped || !chasing.size) { unlater(chaseTimer); chaseTimer = null; chaseTries = 0; return; }
        // An armed timer is never pushed out (an event only adds ids to the set), and a try is counted only when an ask is made.
        if (document.hidden || chaseTimer) return;
        chaseTimer = later(function () {
          chaseTimer = null;
          if (document.hidden) return;
          // Re-checked now: an id the list's own follow-up (or a refresh) settled meanwhile is not asked, and no try is counted.
          chasing.forEach(function (id) { if (!visible.has(id) || !reader.isFast(id)) chasing.delete(id); });
          if (!chasing.size) { chaseTries = 0; return; }
          chaseTries++;
          askNamed(Array.from(chasing));
        }, FOLLOW_MS[Math.min(chaseTries, FOLLOW_MS.length - 1)]);
      }
      function flushNamed() {
        namedTimer = null;
        var list = Array.from(named);
        named.clear();
        askNamed(list);
      }
      function askNamed(list) {
        return reader.pull({ ids: list })
          .then(function () { if (!stopped && o.onChange) o.onChange(handle); }, function (e) { if (!core.isSuperseded(e) && o.onError) o.onError(e); })
          .then(function () { chase(); armSlow(); });
      }
      var handle = {
        get: function (id) { return reader.get(id); },
        reader: reader,
        ids: function () { return Array.from(visible); },
        known: function () { return reader.known(Array.from(visible)); },
        // A live event or the page's own refresh: every visible id again, except refused ones; refresh({refused: true}) asks those too.
        refresh: function (r) { return pull({ all: true, refused: !!(r && r.refused) }); },
        // A data-change event naming these ids: the visible ones are asked again (one ask, exactly those ids, batched over the debounce;
        // no other id is asked and nobody's backoff restarts), the others are marked and asked when they scroll in. An event naming no
        // ids names nothing: the page calls refresh() deliberately.
        changed: function (ids) {
          if (!Array.isArray(ids) || !ids.length) return;
          reader.markDirty(ids);
          ids.forEach(function (id) { if (visible.has(id)) { named.add(id); chasing.add(id); } });
          // Hidden: the names wait, and showing asks them in one batch; nothing is asked on the way back except what an event named.
          if (!named.size || document.hidden) return;
          unlater(namedTimer);
          namedTimer = later(flushNamed, debounce);
        },
        stop: function () { stopped = true; unlater(timer); unlater(namedTimer); unlater(chaseTimer); unlater(slowTimer); boxes.splice(0).forEach(function (b) { if (b.cancel) b.cancel(); }); unlater(follow); totalsHandles.delete(handle); document.removeEventListener("visibilitychange", onShown); if (io) io.disconnect(); if (mo) mo.disconnect(); },
      };
      totalsHandles.add(handle);
      return handle;
    };

    // ── K7 cachedQuery ─────────────────────────────────────────────────────
    // The last answer paints at once, from storage keyed by host, dashboard and args; one read then revalidates it, whatever its age.
    // maxAgeMs decides the label (cached while younger, stale after) and when a later refresh is due, not whether the read happens. Storage is
    // the host adapter's own (`storage()`, the in-app kit only; the published toolkit has none and keeps answers in memory for the page). Any
    // storage failure falls back to memory, and a write that does not fit is skipped.
    var memory = new Map(), cached = new Map();
    function hostStore() { var a = pick(); return a && typeof a.storage === "function" ? a.storage() : null; }
    function storeGet(k) {
      try {
        var store = hostStore(), raw = store ? store.getItem(k) : null, saved = raw != null ? JSON.parse(raw) : null;
        if (saved && typeof saved.at === "number") return saved;
      } catch (e) { /* memory below */ }
      return memory.get(k) || null;
    }
    function storePut(k, entry) {
      memory.set(k, entry);
      try { var store = hostStore(); if (store) store.setItem(k, JSON.stringify(entry)); } catch (e) { /* memory only */ }
    }
    core.cachedQuery = function (name, args, o) {
      o = o || {};
      if (!(o.maxAgeMs > 0)) throw new Error("cachedQuery needs maxAgeMs");
      args = args || {};
      var skey = "cs-kit:1:" + core.host() + ":" + (o.key || window.location.pathname) + ":" + name + ":" + stable(args);
      // One handle per name, args, key and maxAgeMs: asking again with another maxAgeMs gets its own (same stored answer, own label).
      var hkey = skey + "|" + o.maxAgeMs;
      if (cached.has(hkey)) return cached.get(hkey);
      var entry = storeGet(skey), source = entry ? "storage" : null, error = null, seq = 0, listeners = [], settled;
      function peek() {
        if (!entry) return { value: undefined, ageMs: null, state: "none", error: error };
        var ageMs = now() - entry.at;
        if (!isFinite(ageMs)) return { value: entry.value, ageMs: null, state: "stale", error: error };
        ageMs = Math.max(0, ageMs);
        return { value: entry.value, ageMs: ageMs, state: ageMs > o.maxAgeMs ? "stale" : source === "net" ? "fresh" : "cached", error: error };
      }
      function emit() { listeners.slice().forEach(function (cb) { cb(peek()); }); }
      function refresh() {
        var mine = ++seq;
        settled = core.read(name, args, { slot: skey }).then(function (value) {
          if (mine !== seq) return peek();
          entry = { at: now(), value: value };
          source = "net";
          error = null;
          storePut(skey, entry);
          emit();
          return peek();
        }, function (e) {
          if (mine !== seq || core.isSuperseded(e)) return peek();
          error = e;
          emit();
          return peek();
        });
        return settled;
      }
      var handle = {
        peek: peek,
        refresh: refresh,
        onChange: function (cb) { listeners.push(cb); return function () { listeners = listeners.filter(function (l) { return l !== cb; }); }; },
        get ready() { return settled; },
      };
      cached.set(hkey, handle);
      refresh();
      return handle;
    };

    // ── keyed lists, K8 section (resource-store.js) ────────────────────────
    // Render a list into `container` by key, reusing each item's element while its version is unchanged.
    core.keyed = function (container, items, opts) {
      var prev = container.__csKeyed || new Map(), next = new Map();
      var els = items.map(function (item) {
        var k = String(opts.key(item)), v = opts.version ? String(opts.version(item)) : jsonOr(item), hit = prev.get(k), reused = hit && hit.v === v;
        var node = reused ? hit.el : opts.render(item);
        if (reused && opts.update) opts.update(node, item);
        node.dataset.key = k;
        next.set(k, { v: v, el: node });
        return node;
      });
      var keep = new Set(els);
      Array.prototype.slice.call(container.childNodes).forEach(function (n) { if (!keep.has(n)) n.remove(); });
      els.forEach(function (node, i) { if (container.childNodes[i] !== node) container.insertBefore(node, container.childNodes[i] || null); });
      container.__csKeyed = next;
    };
    // One owner per async resource. No scheduling and no shared completion serial. A slot (name + scope) owns its read: fn(slot)
    // starts it and hangs the read's bookkeeping (startedAt, boxes, cancelReads, retry) on the slot, which every view shares.
    function createResourceStore(o) {
      var onChange = o.onChange, slots = new Map(), closedScopes = new Set();
      function get(name, scope) { var slot = slots.get(name); return scope === undefined || (slot && slot.scope === scope) ? slot : undefined; }
      function run(name, scope, fn) {
        if (closedScopes.has(scope)) return Promise.resolve({ applied: false });
        var slot = slots.get(name);
        // Another scope taking the key disposes the slot it replaces: its read is cancelled and its retry timer is gone.
        if (!slot || slot.scope !== scope || slot.disposed) {
          if (slot && slot.dispose && !slot.disposed) slot.dispose();
          slot = { value: null, error: null, token: 0, scope: scope, pending: null };
          slots.set(name, slot);
        }
        slot.cancelled = false;
        var token = ++slot.token;
        function current() { return slots.get(name) === slot && slot.token === token && slot.scope === scope; }
        function finish(value, error) {
          if (!current()) return { applied: false };
          if (!error) slot.value = value;
          slot.error = error;
          slot.pending = null;
          onChange(name, slot);
          return { applied: true, value: slot.value, error: error };
        }
        var result;
        try { result = fn(slot); } catch (e) { return Promise.resolve(finish(null, e)); }
        if (result && typeof result.then === "function") {
          var pending = Promise.resolve(result).then(function (v) { return finish(v, null); }, function (e) { return finish(null, e); });
          slot.pending = pending;
          return pending;
        }
        return Promise.resolve(finish(result, null));
      }
      // Drops the read in flight: a late answer lands on an older token and is ignored. The slot keeps its last value.
      function cancel(name, scope) {
        var slot = get(name, scope);
        if (!slot) return;
        slot.token++;
        slot.pending = null;
        slot.error = null;
        slot.cancelled = true;
        onChange(name, slot);
      }
      function invalidate(scope) {
        closedScopes.add(scope);
        slots.forEach(function (slot) {
          if (slot.scope !== scope) return;
          if (slot.dispose) slot.dispose();
          slot.token++; slot.scope = null; slot.value = null; slot.error = null; slot.pending = null;
        });
      }
      // The slot under this name is this one and its scope is open: the only slot allowed to ask again.
      function alive(name, slot) { return slots.get(name) === slot && !slot.disposed && slot.scope !== null && !closedScopes.has(slot.scope); }
      return { get: get, run: run, cancel: cancel, invalidate: invalidate, alive: alive, each: function (fn) { slots.forEach(fn); } };
    }
    var painters = new Map(), sections = null, revs = 0;
    var BUSY_BACKOFF = [2000, 4000, 8000, 16000];
    // Once per slot change, whoever is looking: stamp a landed read, and ask again after a busy answer (2, 4, 8, then every 16 s)
    // while some view still shows the slot.
    // A view is active when it can be seen: a visible section always, a <details> section while it is open.
    function viewsActive(name) {
      var views = painters.get(name);
      return !!views && Array.from(views).some(function (v) { return v.active && v.active(); });
    }
    function armRetry(slot) {
      var tries = slot.busyTries || 0;
      slot.retryTimer = later(function () {
        slot.retryTimer = null;
        // Hidden, or every view closed: wait. Showing or opening starts the timer again; it never reads on return.
        if (document.hidden || !viewsActive(slot.name)) { slot.retryWaiting = true; return; }
        slot.busyTries = tries + 1;
        if (slot.retry) slot.retry();
      }, BUSY_BACKOFF[Math.min(tries, BUSY_BACKOFF.length - 1)]);
    }
    function settled(slot, views) {
      unlater(slot.retryTimer);
      slot.retryTimer = null;
      slot.retryWaiting = false;
      if (!slot.error && !slot.pending && !slot.cancelled) { slot.readAt = now(); slot.rev = ++revs; slot.busyTries = 0; }
      if (slot.error && core.isBusy(slot.error) && slot.retry && views) armRetry(slot);
    }
    var deferredKeys = [], drainTimer = null;
    // A deferred key's identity: the key and, when named, its scope (so {key, scope} objects compare by value).
    function keyId(k) { return typeof k === "string" ? "s:" + k : "o:" + k.key + ":" + stable(k.scope === undefined ? null : k.scope); }
    // Keys named by an event while the page was hidden are read one at a time, a second apart, once it is shown: no burst on return.
    function drainDeferred() {
      if (drainTimer || !deferredKeys.length) return;
      drainTimer = later(function () {
        drainTimer = null;
        if (document.hidden) return;
        readKey(deferredKeys.shift());
        drainDeferred();
      }, 1000);
    }
    function readKey(k) {
      var key = typeof k === "string" ? k : k.key, slot = sections && sections.get(key, typeof k === "string" ? undefined : k.scope);
      if (slot && slot.retry && slot.readAt != null) slot.retry();
    }
    function sectionStore() {
      if (sections) return sections;
      sections = createResourceStore({
        onChange: function (name, slot) {
          var views = painters.get(name);
          slot.name = name;
          settled(slot, views && views.size && sections.alive(name, slot));
          if (views) views.forEach(function (paint) { paint(slot); });
        },
      });
      document.addEventListener("visibilitychange", function () {
        if (document.hidden) return;
        sections.each(function (slot, name) {
          var views = painters.get(name);
          if (slot.retryWaiting && views && views.size && viewsActive(name) && sections.alive(name, slot)) { slot.retryWaiting = false; armRetry(slot); }
        });
        drainDeferred();
      });
      return sections;
    }
    function minutes(ms) {
      var m = Math.floor(ms / 60000);
      return m < 1 ? "just now" : m < 60 ? m + " min ago" : Math.floor(m / 60) + " h ago";
    }
    // Start the one read of a slot. Same scope and no force: join the read in flight (no cancel, no engine call, startedAt kept).
    // A force, or another scope taking the key, frees the budget slot of the read it replaces first.
    function startRead(key, scope, load, force, alive) {
      var store = sectionStore(), cur = store.get(key);
      if (cur && cur.scope === scope && !force && cur.pending && !cur.disposed) return cur.pending;
      if (cur && cur.cancelReads && force && cur.scope === scope) cur.cancelReads();
      return store.run(key, scope, function (slot) {
        var mine = slot.token, boxes = slot.boxes = [];
        slot.startedAt = now();
        slot.cancelReads = function () { boxes.splice(0).forEach(function (b) { if (b.cancel) b.cancel(); }); };
        slot.retry = function () { if (store.alive(key, slot)) startRead(key, scope, load, true, alive); };
        // The one way a slot ends: its read is cancelled and dropped (a late answer lands on an older token), its retry timer is gone,
        // it can never ask again and nothing can join it: the next read of this key and scope gets a fresh slot.
        slot.dispose = function () {
          slot.disposed = true;
          slot.token++;
          slot.pending = null;
          slot.cancelReads();
          unlater(slot.retryTimer);
          slot.retryTimer = null;
          slot.retry = null;
        };
        function stale() { return slot.token !== mine || store.get(key, scope) !== slot || (alive && !alive()); }
        return load({ scope: scope, read: function (name, args, x) {
          return core.tracked(boxes, function (box) { return core.read(name, args, Object.assign({ box: box, stale: stale }, x)); });
        } });
      });
    }
    // section(el, {key, load, render, when: 'open' | 'visible', scope, isEmpty, emptyText, stillMs}): one bounded read when a
    // <details> opens or the region scrolls into view. load({read, scope}) resolves the value, or {unknown: reason}. A section is
    // a view over the store slot of its key and scope: the slot owns the read, so two sections of one key and scope share one
    // engine call, one elapsed time and one Cancel. The view draws a skeleton, empty, unknown (with the reason), error with Retry,
    // "Waiting for the Mac" (the Mac said busy: asked again after 2, 4, 8, 16 s), "Read N min ago" and "Still reading (N s)" with
    // Cancel (which frees the read's budget slot at once). `scope` (a value or a function) names the owner of the answer: another
    // scope taking the key cancels the read it replaces, and an answer that lands after the scope changed, or the scope was closed
    // with closeScope(), is dropped. Use a fresh token per visit: a closed scope stays closed. A clock tick rewrites only the status
    // text, never the content, so focus and selection survive it.
    core.section = function (host, o) {
      var store = sectionStore(), ticker = null, off = null, unwatch = null, stopped = false, contentKey = "", actionKey = "", lastSlot = null;
      var stillMs = o.stillMs == null ? 15000 : o.stillMs, when = o.when || (host.tagName === "DETAILS" ? "open" : "visible");
      var body = Array.from(host.children).find(function (c) { return c.classList.contains("cs-section-body"); });
      if (!body) { body = el("div", "cs-section-body"); host.appendChild(body); }
      var content = el("div", "cs-section-content"), line = el("div", "cs-section-meta"), lineText = document.createTextNode("");
      line.hidden = true;
      line.append(lineText);
      body.replaceChildren(content, line);
      style();
      var retry = { label: "Retry", onClick: function () { run(true); } }, stopReading = { label: "Cancel", onClick: cancel };
      // Sections do not refresh themselves, so a read a minute old offers to read again.
      var refreshNow = { label: "Refresh", onClick: retry.onClick };
      function scopeNow() { var s = typeof o.scope === "function" ? o.scope() : o.scope; return s === undefined ? "page" : s; }
      function slotNow() { return store.get(o.key, scopeNow()); }
      function isEmpty(v) { return o.isEmpty ? o.isEmpty(v) : Array.isArray(v) ? !v.length : v == null; }
      // The content is rebuilt only when what it shows changes: a new answer, the skeleton, nothing.
      function paintContent(slot) {
        var key = slot && slot.rev != null ? "value:" + slot.rev : slot && slot.pending ? "skeleton" : "none";
        if (key === contentKey) return;
        contentKey = key;
        if (key === "skeleton") {
          var sk = el("div", "cs-section-skeleton");
          sk.setAttribute("aria-busy", "true");
          sk.append(el("span"), el("span"), el("span"));
          content.replaceChildren(sk);
        } else if (key === "none") content.replaceChildren();
        else if (slot.value && slot.value.unknown) content.replaceChildren(el("div", "cs-section-error", unknownLabel(slot.value.unknown)));
        else if (isEmpty(slot.value)) content.replaceChildren(el("div", "cs-empty", o.emptyText || "Nothing here yet."));
        else {
          var wrap = el("div", "cs-section-value");
          try {
            o.render(slot.value, wrap);
            content.replaceChildren(wrap);
          } catch (e) {
            console.error(e);
            content.replaceChildren(el("div", "cs-section-error", "Could not show this."));
          }
        }
      }
      // The status line: its text node is rewritten in place; its button only when the action changes. Elapsed time is the slot's.
      function paintMeta(slot) {
        var loading = !!(slot && slot.pending), err = slot && slot.error, text = "", action = null, elapsed = loading ? now() - slot.startedAt : 0;
        if (slot && slot.cancelled) { text = "Cancelled."; action = retry; }
        else if (err && core.isBusy(err)) text = WAITING;
        else if (err) { var why = core.errorText(err); text = why === GENERIC_ERROR ? GENERIC_ERROR + "." : GENERIC_ERROR + ". " + why; action = retry; }
        else if (loading && elapsed >= stillMs) { text = "Still reading (" + (elapsed < 120000 ? Math.floor(elapsed / 1000) + " s" : Math.floor(elapsed / 60000) + " min") + ")"; action = stopReading; }
        else if (loading && core.busy()) text = WAITING;
        else if (slot && slot.readAt != null) {
          var age = now() - slot.readAt;
          text = "Read " + minutes(age) + (loading ? " · updating" : "");
          if (!loading && age >= 60000) action = refreshNow;
        }
        lineText.data = text;
        line.hidden = !text;
        var key = action ? action.label : "";
        if (key === actionKey) return;
        actionKey = key;
        while (line.childNodes.length > 1) line.lastChild.remove();
        if (action) {
          var b = el("button", "cs-btn is-sm", action.label);
          b.type = "button";
          b.addEventListener("click", action.onClick);
          line.append(b);
        }
      }
      // One timer: at stillMs while reading, then every 10 s to keep "Still reading (N s)" true (30 s once the read has landed).
      function tick() {
        unlater(ticker);
        ticker = null;
        var slot = slotNow(), loading = !!(slot && slot.pending);
        if (stopped || !(loading || (slot && slot.readAt != null))) return;
        var wait = loading ? (now() - slot.startedAt < stillMs ? stillMs - (now() - slot.startedAt) : 10000) : 30000;
        ticker = later(function () { ticker = null; paintMeta(slotNow()); tick(); }, wait);
      }
      function paint(slot) { if (slot) lastSlot = slot; paintContent(slot); paintMeta(slot); tick(); }
      function repaint(slot) { if (!stopped && slot.scope === scopeNow()) paint(slot); }
      repaint.active = function () { return !stopped && (when !== "open" || host.open); };
      // Opening (or scrolling in) starts a busy retry that waited for this view again; it never reads on the way.
      function resumeRetry() {
        var slot = slotNow();
        if (slot && slot.retryWaiting && !document.hidden && sections.alive(o.key, slot)) { slot.retryWaiting = false; armRetry(slot); }
      }
      if (!painters.has(o.key)) painters.set(o.key, new Set());
      painters.get(o.key).add(repaint);
      unwatch = core.budget().onChange(function () { var slot = slotNow(); if (slot && slot.pending) paintMeta(slot); });
      function run(force) {
        resumeRetry();
        var scope = scopeNow(), slot = store.get(o.key, scope);
        if (!force && slot && slot.pending) { paint(slot); return slot.pending; }
        // A section that has read, was cancelled or failed shows that again, however long ago, however long it was hidden and however
        // often it scrolls back in: only Retry or refresh() (force), a data-change event that names its key (dataChanged) or the busy
        // backoff reads again.
        if (!force && slot && !slot.disposed && (slot.readAt != null || slot.error || slot.cancelled)) return paint(slot);
        var started = startRead(o.key, scope, o.load, force, function () { return scopeNow() === scope; });
        paint(store.get(o.key, scope));
        started.then(function (r) { if (r && !r.applied) paint(store.get(o.key, scopeNow())); });
        return started;
      }
      // Cancel frees the reads' budget slots at once; the slot drops the read in flight, so a late answer is ignored.
      function cancel() {
        var slot = slotNow();
        if (!slot || !slot.pending) return;
        if (slot.cancelReads) slot.cancelReads();
        store.cancel(o.key, scopeNow());
      }
      function arm() {
        if (when === "open") {
          var onToggle = function () { if (host.open) run(); };
          host.addEventListener("toggle", onToggle);
          off = function () { host.removeEventListener("toggle", onToggle); };
          if (host.open) run();
        } else if (typeof IntersectionObserver !== "undefined") {
          var io = new IntersectionObserver(function (entries) { if (entries.some(function (e) { return e.isIntersecting; })) run(); });
          io.observe(host);
          off = function () { io.disconnect(); };
        } else run();
      }
      arm();
      // A key an event named while the page was hidden and no section existed is consumed now: this section's first read is fresh.
      deferredKeys = deferredKeys.filter(function (k) {
        if ((typeof k === "string" ? k : k.key) !== o.key) return true;
        return typeof k !== "string" && k.scope !== undefined && k.scope !== scopeNow();
      });
      return {
        refresh: function () { return run(true); },
        cancel: cancel,
        get loaded() { var slot = slotNow(); return !!slot && slot.readAt != null; },
        // The last view of a key leaving disposes its slot: no read is left running and no timer fires later.
        stop: function () {
          stopped = true;
          unlater(ticker);
          if (off) off();
          if (unwatch) unwatch();
          var views = painters.get(o.key);
          views.delete(repaint);
          // The slot this view last painted, not whatever its scope names now (the page may have moved on before stopping).
          if (!views.size && lastSlot && lastSlot.dispose && !lastSlot.disposed) lastSlot.dispose();
        },
      };
    };
    core.closeScope = function (scope) { sectionStore().invalidate(scope); };
    // A live data-change event: dataChanged({ids, keys}). `ids` (row or story ids) go to every visibleTotals: the visible ones are asked
    // again, one ask with just those ids, and the rest are marked and asked when they scroll in. `keys` (a section key, or {key, scope})
    // read again quietly in place, only the sections named. An event naming neither does nothing: nothing requeries a whole list or story.
    core.dataChanged = function (ev) {
      ev = ev || {};
      if (Array.isArray(ev.ids)) totalsHandles.forEach(function (h) { h.changed(ev.ids); });
      (ev.keys || []).forEach(function (k) {
        if (document.hidden) { if (!deferredKeys.some(function (d) { return keyId(d) === keyId(k); }) && deferredKeys.length < 200) deferredKeys.push(k); } else readKey(k);
      });
    };

    // ── K10 keyedRegion (focus.js, patches.js) ─────────────────────────────
    // Capture meaning before replacing DOM; every restoration uses the same fallback chain.
    function createFocusIntents(p) {
      var root = p.root, doc = p.doc || root.ownerDocument;
      function attr(name, value) { return "[" + name + '="' + CSS.escape(String(value)) + '"]'; }
      function available(node) { return node && !node.disabled && !node.closest("[hidden], [inert]"); }
      function controls(row) { return Array.from(row.querySelectorAll("button,a[href],input,select,textarea,summary,[tabindex]")).filter(function (n) { return available(n) && n.tabIndex >= 0; }); }
      function find(scope, selector) { return selector ? (scope.matches && scope.matches(selector) ? scope : scope.querySelector(selector)) : null; }
      function point(node, offset) {
        var parent = node.nodeType === 1 ? node : node.parentElement, scope = (parent && parent.closest("[data-row-focus],[id]")) || root;
        var key = scope.id ? "#" + CSS.escape(scope.id) : scope.dataset && scope.dataset.rowFocus ? attr("data-row-focus", scope.dataset.rowFocus) : null, path = [];
        for (var at = node; at !== scope; at = at.parentNode) { if (!at.parentNode) return null; path.unshift(Array.from(at.parentNode.childNodes).indexOf(at)); }
        return { key: key, path: path, offset: offset };
      }
      function textSelection() {
        var sel = doc.getSelection();
        if (!sel || sel.isCollapsed || !root.contains(sel.anchorNode) || !root.contains(sel.focusNode)) return null;
        var range = sel.getRangeAt(0);
        return { start: point(range.startContainer, range.startOffset), end: point(range.endContainer, range.endOffset), text: sel.toString(), forward: sel.anchorNode === range.startContainer && sel.anchorOffset === range.startOffset };
      }
      function restoreSelection(saved) {
        if (!saved || !saved.start || !saved.end) return;
        function resolve(pt) {
          var node = pt.key ? root.querySelector(pt.key) : root;
          if (pt.key && root.matches && root.matches(pt.key)) node = root;
          for (var i = 0; i < pt.path.length; i++) node = node && node.childNodes[pt.path[i]];
          return node && pt.offset <= (node.nodeType === 3 ? node.length : node.childNodes.length) ? node : null;
        }
        var start = resolve(saved.start), end = resolve(saved.end);
        if (!start || !end) return;
        var range = doc.createRange();
        range.setStart(start, saved.start.offset);
        range.setEnd(end, saved.end.offset);
        if (range.toString() !== saved.text) return;
        var sel = doc.getSelection();
        if (saved.forward) sel.setBaseAndExtent(start, saved.start.offset, end, saved.end.offset); else sel.setBaseAndExtent(end, saved.end.offset, start, saved.start.offset);
      }
      function capture(element) {
        element = element || doc.activeElement;
        var selection = textSelection();
        if (!element || element === doc.body || !root.contains(element)) return selection ? { focused: false, selection: selection } : null;
        var row = element.closest("[data-row-focus]"), rowId = row ? row.dataset.rowFocus : null;
        var key = ["data-focus-key", "data-panel-key", "data-sort", "data-open", "data-retry", "data-group", "name"].find(function (n) { return element.hasAttribute(n); });
        var controlId = key ? attr(key, element.getAttribute(key)) : element.id ? "#" + CSS.escape(element.id) : null;
        var sectionNode = element.closest("section");
        var heading = sectionNode && sectionNode.querySelector("h2[id]"), sectionId = heading ? heading.id : null;
        return { selection: selection, rowId: rowId, controlId: controlId, sectionId: sectionId, position: row ? Math.max(0, controls(row).indexOf(element)) : 0, element: element };
      }
      function restore(intent, r) {
        if (!intent) return;
        r = r || {};
        restoreSelection(intent.selection);
        if (intent.focused === false) return;
        var rowNode = intent.rowId ? root.querySelector(attr("data-row-focus", intent.rowId)) : null, row = available(rowNode) ? rowNode : null;
        var target = available(intent.element) && root.contains(intent.element) ? intent.element : null;
        if (target == null) target = find(row || root, intent.controlId);
        if (!available(target)) target = null;
        if (!target && intent.controlId) target = find(root, intent.controlId);
        if (!available(target)) target = null;
        var fallback = false;
        if (!target && row) { fallback = true; var candidates = controls(row); target = candidates[Math.min(intent.position || 0, candidates.length - 1)] || row; }
        if (!target) fallback = true;
        if (!target) target = (intent.sectionId ? doc.getElementById(intent.sectionId) : null) || root.querySelector("h1,h2");
        if (target) {
          if (target.tabIndex < 0) target.setAttribute("tabindex", "-1");
          target.focus({ preventScroll: true });
          if (r.reveal || fallback) target.scrollIntoView && target.scrollIntoView({ block: "nearest" });
        }
      }
      var view = doc.defaultView, lastIntent = null, guardianFrame = null;
      function remember(event) { var intent = capture(event.target); if (intent && intent.focused !== false) lastIntent = intent; }
      function guard(event) {
        if (event && event.target) remember(event);
        // Native date editors can omit focusin; focusout still names the destination.
        if (event && event.relatedTarget) { if (root.contains(event.relatedTarget)) remember({ target: event.relatedTarget }); return; }
        if (guardianFrame != null) return;
        guardianFrame = view.requestAnimationFrame(function () {
          guardianFrame = null;
          // A deliberate Tab/Escape into the host must remain outside this document.
          if (lastIntent && doc.hasFocus() && (doc.activeElement === doc.body || !root.contains(doc.activeElement))) {
            if (root.contains(lastIntent.element) && available(lastIntent.element)) { lastIntent = null; return; }
            restore(Object.assign({}, lastIntent, { selection: null }));
          }
        });
      }
      root.addEventListener("focusin", remember, true);
      root.addEventListener("focusout", guard, true);
      // WebKit does not emit focusout when the focused node is removed.
      var observer = new view.MutationObserver(function () {
        if (lastIntent && (!root.contains(lastIntent.element) || !available(lastIntent.element)) && doc.hasFocus() && (doc.activeElement === doc.body || !root.contains(doc.activeElement))) guard();
      });
      observer.observe(root, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled", "hidden", "inert"] });
      function stop() {
        observer.disconnect();
        root.removeEventListener("focusin", remember, true);
        root.removeEventListener("focusout", guard, true);
        if (guardianFrame != null) view.cancelAnimationFrame(guardianFrame);
        lastIntent = null;
      }
      return { capture: capture, restore: restore, stop: stop };
    }
    // keyed owns reconciliation; this layer only postpones protected structure (a focused or selected row) and gestures.
    function createPatches(p) {
      var root = p.root, doc = p.doc || root.ownerDocument, records = new WeakMap(), pending = new Map(), texts = new Map(), refreshes = new Map();
      var gesturing = false, stopped = false, releaseTimer = null;
      function key(opts, item) { return String(opts.key(item)); }
      function version(opts, item) { return String(opts.version ? opts.version(item) : jsonOr(item)); }
      function selected(node) {
        var sel = doc.getSelection();
        if (!sel || !sel.rangeCount || sel.isCollapsed || !node.isConnected) return false;
        return sel.getRangeAt(0).intersectsNode(node);
      }
      function associated(node) {
        var rowNode = node && node.closest && node.closest("[data-row-id]"), id = rowNode && rowNode.dataset.rowId;
        return id ? root.querySelector('[data-row-focus="' + CSS.escape(id) + '"]') : null;
      }
      function touching(node) {
        var active = doc.hasFocus() ? doc.activeElement : null;
        return !!(active && (node.contains(active) || node.contains(associated(active))) || selected(node));
      }
      function text(node, value) {
        value = String(value == null ? "" : value);
        if (node.textContent === value) { texts.delete(node); return; }
        if (selected(node)) { texts.set(node, value); return; }
        if (node.childNodes.length === 1 && node.firstChild.nodeType === 3) node.firstChild.data = value; else node.textContent = value;
        texts.delete(node);
      }
      function keyed(container, items, opts, passive) {
        var previous = records.get(container), incoming = new Map(items.map(function (item) { return [key(opts, item), item]; }));
        var order = items.map(function (item) { return key(opts, item); });
        var structural = previous && (order.join("\0") !== previous.order.join("\0") || items.some(function (item) { return previous.versions.get(key(opts, item)) !== version(opts, item); }));
        // A genuine deletion of the protected row is handled by the focus guardian.
        var protectedRow = Array.from(container.children).some(function (node) { return incoming.has(node.dataset.key) && touching(node); });
        var effective = items, versions = new Map(items.map(function (item) { return [key(opts, item), version(opts, item)]; }));
        if (passive && structural && protectedRow) {
          pending.set(container, { items: items, opts: opts });
          effective = previous.items.map(function (item) { return incoming.get(key(opts, item)) || item; });
          versions = previous.versions;
        } else pending.delete(container);
        core.keyed(container, effective, {
          key: opts.key, version: function (item) { return versions.get(key(opts, item)) || version(opts, item); },
          render: function (item) { var node = opts.render(item); if (opts.update) opts.update(node, item); return node; }, update: opts.update,
        });
        records.set(container, { items: effective, order: effective.map(function (item) { return key(opts, item); }), versions: versions });
      }
      // Gestures protect the whole page: even facts in another row can move a target.
      function defer(owner, refresh) { if (!gesturing) return false; refreshes.set(owner, refresh); return true; }
      function flush() {
        if (stopped || gesturing) return;
        var work = Array.from(refreshes.values());
        refreshes.clear();
        work.forEach(function (refresh) { refresh(); });
        texts.forEach(function (value, node) { if (!node.isConnected) texts.delete(node); else if (!selected(node)) text(node, value); });
        Array.from(pending).forEach(function (pair) { if (pair[0].isConnected) keyed(pair[0], pair[1].items, pair[1].opts, true); else pending.delete(pair[0]); });
      }
      function down(event) {
        if (event.button !== 0 || event.isPrimary === false) return;
        if (releaseTimer != null) { unlater(releaseTimer); releaseTimer = null; }
        gesturing = true;
      }
      // The click handler consumes the held target before queued reads can patch it.
      function end() { if (releaseTimer != null) { unlater(releaseTimer); releaseTimer = null; } gesturing = false; queueMicrotask(flush); }
      function up() { if (gesturing && releaseTimer == null) releaseTimer = later(end, 0); }
      function exit(event) { if (gesturing && event.buttons && !event.relatedTarget) end(); }
      function leave() { queueMicrotask(flush); }
      var win = doc.defaultView, listeners = [["pointerdown", down, true], ["pointerup", up, true], ["pointerleave", exit, true], ["mouseleave", exit, true], ["click", end, false], ["auxclick", end, true], ["dragend", end, true], ["pointercancel", end, true], ["focusout", leave, true], ["selectionchange", leave, false]];
      listeners.forEach(function (l) { doc.addEventListener(l[0], l[1], l[2]); });
      win.addEventListener("blur", end);
      function stop() {
        stopped = true;
        end();
        pending.clear(); texts.clear(); refreshes.clear();
        listeners.forEach(function (l) { doc.removeEventListener(l[0], l[1], l[2]); });
        win.removeEventListener("blur", end);
      }
      return { keyed: keyed, text: text, defer: defer, stop: stop, flush: flush };
    }
    // keyedRegion(el, scope): regions repaint in place on a selection change, keeping focus, selection, scroll and gesture
    // deferral. paint(items, {key, render, version, update}, forScope): a paint for a scope that is no longer current is dropped.
    core.keyedRegion = function (root, scope) {
      var current = scope, intents = createFocusIntents({ root: root }), patches = createPatches({ root: root });
      return {
        setScope: function (next) { current = next; },
        paint: function (items, opts, forScope) {
          if (forScope !== undefined && forScope !== current) return false;
          var intent = intents.capture();
          patches.keyed(root, items, opts, true);
          intents.restore(intent);
          return true;
        },
        text: patches.text,
        defer: patches.defer,
        flush: patches.flush,
        stop: function () { intents.stop(); patches.stop(); },
      };
    };

    // ── K1 boot ────────────────────────────────────────────────────────────
    // boot({render, queries: {alias: {name, args, maxAgeMs}}, key, onError}) -> {host, call, subscribe, state, refresh, stop}
    // render(ctx) runs at once (cached answers are there synchronously) and again whenever a query changes; it must be
    // idempotent. ctx.state.data[alias] is cachedQuery's {value, ageMs, state, error}. The host comes from the bridge object
    // that is present (core.host), so one entry file runs in the app and on the share host.
    core.boot = function (o) {
      o = o || {};
      var adapter = pick(), handles = {}, offs = [], stopped = false;
      var ctx = {
        host: adapter.name, state: { host: adapter.name, ready: !adapter.ready, error: null, data: {} },
        call: function (name, args, opts) { return core.read(name, args, opts); }, subscribe: adapter.subscribe,
      };
      function paint() {
        if (stopped || typeof o.render !== "function") return;
        try { o.render(ctx); } catch (e) { if (o.onError) o.onError(e); else console.error(e); }
      }
      Object.keys(o.queries || {}).forEach(function (alias) {
        var q = o.queries[alias], h = core.cachedQuery(q.name, q.args || {}, { key: o.key, maxAgeMs: q.maxAgeMs || 60000 });
        handles[alias] = h;
        ctx.state.data[alias] = h.peek();
        offs.push(h.onChange(function (v) { ctx.state.data[alias] = v; paint(); }));
      });
      if (adapter.ready) {
        adapter.ready().then(function () { ctx.state.ready = true; paint(); }, function (e) { ctx.state.error = String((e && e.message) || e); paint(); });
      }
      paint();
      return {
        host: adapter.name, state: ctx.state, call: ctx.call, subscribe: adapter.subscribe,
        refresh: function () { return Promise.all(Object.keys(handles).map(function (a) { return handles[a].refresh(); })); },
        stop: function () { stopped = true; offs.forEach(function (off) { off(); }); },
      };
    };

    core.api = {
      coreVersion: core.version, dataChanged: core.dataChanged, errorText: core.errorText, errorKind: core.errorKind, errorCodes: core.errorCodes, boot: core.boot, paced: core.paced, budget: core.budget, value: core.value, valueState: core.valueState, stateBanner: core.stateBanner,
      partial: core.partial, batchTotals: core.batchTotals, visibleTotals: core.visibleTotals, cachedQuery: core.cachedQuery,
      section: core.section, closeScope: core.closeScope, keyedRegion: core.keyedRegion, keyed: core.keyed,
    };
    return core;
  })());
  /*@kit-core-end*/
  csCore.register("app", {
    present: function () { return typeof cs.invoke === "function"; },
    // K7's last-answer cache lives in the app frame's own storage; the published toolkit registers no storage.
    storage: function () { return window.localStorage; },
    ask: function (name, args, o) { return wireQuery(name, args, o && o.signal); },
    storyTotals: storyTotals,
    subscribe: subscribe,
  });

  cs.kit = Object.assign({
    version: "1",
    stageRail: stageRail,
    statusMark: statusMark,
    fileKindIcon: fileKindIcon,
    navGlyph: navGlyph,
    fileKindOf: fileKindOf,
    stripViewerExtension: stripViewerExtension,
    stripPipelineExtension: stripPipelineExtension,
    inLens: inLens,
    countLenses: countLenses,
    stageGroupOf: stageGroupOf,
    compareNavNames: compareNavNames,
    h: h,
    s: s,
    icon: icon,
    iconMarkup: iconMarkup,
    iconNames: iconNames,
    agentStates: AGENT_STATES,
    phases: PHASES,
    storyStatuses: STORY_STATUSES,
    agentState: agentState,
    phase: phase,
    storyStatus: storyStatus,
    statusDot: statusDot,
    statusBadge: statusBadge,
    phaseBadge: phaseBadge,
    storyBadge: storyBadge,
    avatar: avatar,
    timeAgo: timeAgo,
    empty: empty,
    notice: notice,
    has: has,
    call: call,
    on: on,
    live: live,
    layoutDag: layoutDag,
    edgePath: edgePath,
    dagSvg: dagSvg,
    dagView: dagView,
    skeleton: skeleton,
    keyed: keyed,
    queryAll: queryAll,
    query: query,
    storyTotals: storyTotals,
    subscribeAll: subscribeAll,
    partialList: partialList,
  }, csCore.api);
  // The same patterns under the name a hosted page uses. Never clobbers a CS that is already there.
  if (!window.CS) window.CS = cs.kit;
})();

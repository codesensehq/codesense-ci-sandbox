import { EditorView, basicSetup } from "codemirror";
export function mountTextEditor() {
  (window as any).editor = new EditorView({
    doc: "editor text",
    extensions: [basicSetup],
    parent: document.body,
  });
}
if (window.parent !== window) mountTextEditor();

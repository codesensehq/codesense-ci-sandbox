(function () {
  // Embedded panes already receive the host kit; only previews need the fallback.
  if (window === window.parent) {
    const fallback = document.createElement("link");
    fallback.rel = "stylesheet";
    fallback.href = "../content/_shared/kit-fallback.css";
    document.head.append(fallback);
  }

  let errorNotice;

  function reportError(error) {
    if (!errorNotice) {
      errorNotice = document.createElement("p");
      errorNotice.setAttribute("role", "alert");
      errorNotice.style.padding = "0 16px";
      document.querySelector(".pane__header").after(errorNotice);
    }
    errorNotice.textContent = "Settings unavailable: " + (error.message || error);
  }

  function onContext(applyContext) {
    let selectedFile;
    return window.codesense?.onSlotContext?.((slot) => {
      const currentFile = slot.currentFile || "";
      if (currentFile === selectedFile) return;
      selectedFile = currentFile;
      const filename = currentFile.split("/").pop();
      const surface = /\.(md|markdown)$/i.test(filename)
        ? "viewer:markdown"
        : /\.excalidraw$/i.test(filename)
          ? "viewer:excalidraw"
          : currentFile
            ? "viewer:html"
            : "";
      applyContext({ surface, filename });
    });
  }

  function ready() {
    requestAnimationFrame(() => requestAnimationFrame(() => window.codesense?.ready?.()));
  }

  window.CodeSenseDocsPane = { reportError, onContext, ready };
})();

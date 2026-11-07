// SPDX-License-Identifier: MIT
//! PDF export via hidden offscreen WKWebView (macOS only).
//!
//! Two Tauri commands:
//! - `export_pdf_from_url`: loads a URL and exports to PDF
//! - `export_pdf_from_html`: loads an HTML string and exports to PDF
//!
//! Plus a utility command:
//! - `write_binary_file`: decodes base64 data and writes raw bytes to disk

use base64::Engine as _;
use log::{debug, error};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{
    NSData, NSError, NSObject, NSPoint, NSRect, NSSize, NSString, NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationDelegate, WKPDFConfiguration, WKWebView, WKWebViewConfiguration,
};

use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// In-flight export registry (SW-FX-64: no more leaked WKWebView per export)
// ---------------------------------------------------------------------------
//
// A hidden WKWebView drives each export, and its navigation delegate is held by
// the webview only WEAKLY (ObjC convention). So from setup until the delegate's
// callbacks fire, *something* must strong-hold both, or they deallocate and the
// export silently hangs. The previous implementation solved that by
// `std::mem::forget`-ing both — leaking an entire native WKWebView on every
// export (a real, unbounded native-memory climb over a long session).
//
// Instead we park the webview + delegate in this main-thread-only registry for
// the duration of the export and release them when the PDF completion handler
// (or a navigation failure) fires. Confined to the main thread — all WKWebView
// work and every delegate/completion callback run there — so no `Send`/lock is
// needed and the `!Send` `Retained` objects are legal to hold.

/// A parked in-flight export: kept alive until its callbacks complete, then
/// dropped (which deallocates the native webview + the delegate).
struct PendingExport {
    _web_view: Retained<WKWebView>,
    _delegate: Retained<ProtocolObject<dyn WKNavigationDelegate>>,
}

thread_local! {
    static PENDING_EXPORTS: RefCell<HashMap<usize, PendingExport>> =
        RefCell::new(HashMap::new());
}

static NEXT_EXPORT_ID: AtomicUsize = AtomicUsize::new(0);

fn next_export_id() -> usize {
    NEXT_EXPORT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Park an export's webview + delegate under `id`. Main thread only.
fn park_export(
    id: usize,
    web_view: Retained<WKWebView>,
    delegate: Retained<ProtocolObject<dyn WKNavigationDelegate>>,
) {
    PENDING_EXPORTS.with(|m| {
        m.borrow_mut().insert(
            id,
            PendingExport {
                _web_view: web_view,
                _delegate: delegate,
            },
        );
    });
}

/// Release a parked export on the NEXT main-queue turn. Deferring is what makes
/// this safe from inside a delegate method or the PDF completion block: the
/// caller (which is itself the delegate, or a block that ran because the
/// delegate is alive) has fully returned before we drop the delegate's last
/// strong ref — no use-after-free. A never-firing navigation leaves at most one
/// parked export (bounded), versus the old leak-on-every-export.
fn schedule_export_release(id: usize) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        PENDING_EXPORTS.with(|m| {
            m.borrow_mut().remove(&id);
        });
    });
}

// ---------------------------------------------------------------------------
// Navigation delegate
// ---------------------------------------------------------------------------

/// Oneshot sender for the final PDF result; wrapped in `Option<...>` so the
/// delegate can `take()` the sender on send (oneshot is single-use), and
/// wrapped in `Mutex<...>` for shared-mutability across the delegate's
/// objc method dispatches.
type PdfResultTx = Mutex<Option<oneshot::Sender<Result<Vec<u8>, String>>>>;

/// The delegate holds a oneshot sender for the final PDF result.
/// When `didFinishNavigation` fires, it calls `createPDF` on the webview
/// (received as a parameter) and sends the result through the channel.
struct NavDelegateIvars {
    result_tx: PdfResultTx,
    /// Registry key for this export, so the delegate can release its own
    /// parked webview + delegate once its callbacks complete (SW-FX-64).
    export_id: usize,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CsPdfNavDelegate"]
    #[ivars = NavDelegateIvars]

    struct CsPdfNavDelegate;

    unsafe impl WKNavigationDelegate for CsPdfNavDelegate {
        #[unsafe(method(webView:didFinishNavigation:))]
        #[allow(non_snake_case)]
        fn webView_did_finish_navigation(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
        ) {
            debug!(target: "export_pdf", "didFinishNavigation fired");
            self.create_pdf(web_view);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        #[allow(non_snake_case)]
        fn webView_did_fail_navigation(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            let msg = format!("Navigation failed: {}", error.localizedDescription());
            error!(target: "export_pdf", "{}", msg);
            self.send_error(msg);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        #[allow(non_snake_case)]
        fn webView_did_fail_provisional_navigation(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            let msg = format!("Navigation failed: {}", error.localizedDescription());
            error!(target: "export_pdf", "{}", msg);
            self.send_error(msg);
        }
    }
);

unsafe impl NSObjectProtocol for CsPdfNavDelegate {}

impl CsPdfNavDelegate {
    fn new(
        result_tx: oneshot::Sender<Result<Vec<u8>, String>>,
        export_id: usize,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NavDelegateIvars {
            result_tx: Mutex::new(Some(result_tx)),
            export_id,
        });
        unsafe { objc2::msg_send![super(this), init] }
    }

    /// Take the sender and send an error. Used by failure delegate methods.
    fn send_error(&self, msg: String) {
        if let Ok(mut guard) = self.ivars().result_tx.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(Err(msg));
            }
        }
        // Navigation failed — the export is over. Release the parked
        // webview + delegate (deferred, so `self` isn't dropped mid-method).
        schedule_export_release(self.ivars().export_id);
    }

    /// Called on successful navigation — generates PDF and sends bytes.
    fn create_pdf(&self, web_view: &WKWebView) {
        let tx = match self.ivars().result_tx.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => {
                error!(target: "export_pdf", "Failed to lock result_tx mutex");
                return;
            }
        };
        let Some(tx) = tx else { return };

        let mtm = MainThreadMarker::new().expect("didFinishNavigation always fires on main thread");
        let pdf_config = unsafe { WKPDFConfiguration::new(mtm) };

        let tx_holder = Arc::new(Mutex::new(Some(tx)));
        // Copy the id out (usize, Copy) so the block doesn't capture `self`.
        let export_id = self.ivars().export_id;

        let block = RcBlock::new(move |data: *mut NSData, err: *mut NSError| {
            let result = if let Some(e) = unsafe { err.as_ref() } {
                Err(format!("createPDF failed: {}", e.localizedDescription()))
            } else if let Some(d) = unsafe { data.as_ref() } {
                Ok(d.to_vec())
            } else {
                Err("createPDF returned nil data and nil error".to_string())
            };

            if let Ok(mut guard) = tx_holder.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(result);
                }
            }
            // PDF generated (or errored) — the export is complete. Release the
            // parked webview + delegate on the next main-queue turn. The block
            // doesn't capture `self`, and the deferral means the delegate is
            // dropped only after this block has returned — no use-after-free.
            schedule_export_release(export_id);
        });

        unsafe {
            web_view.createPDFWithConfiguration_completionHandler(Some(&pdf_config), &block);
        }
    }
}

// ---------------------------------------------------------------------------
// Core PDF generation
// ---------------------------------------------------------------------------

enum ContentSource {
    Url(String),
    Html(String),
}

/// Dispatches WKWebView creation + content loading to the main thread,
/// then waits for the navigation delegate to generate the PDF and send
/// the bytes back through a oneshot channel.
///
/// The WKWebView and delegate are parked in the main-thread export registry
/// for the duration of the export and released when the PDF completion handler
/// (or a navigation failure) fires — so no native webview is leaked per export
/// (SW-FX-64).
async fn generate_pdf(source: ContentSource) -> Result<Vec<u8>, String> {
    let (tx, rx) = oneshot::channel::<Result<Vec<u8>, String>>();

    dispatch2::DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("exec_async on main queue is main thread");

        // Validate URL before creating the delegate (which consumes tx).
        // Invalid URLs return an immediate error instead of hanging 30s.
        if let ContentSource::Url(ref url_str) = source {
            if NSURL::URLWithString(&NSString::from_str(url_str)).is_none() {
                let msg = format!("Invalid URL: {}", url_str);
                error!(target: "export_pdf", "{}", msg);
                let _ = tx.send(Err(msg));
                return;
            }
        }

        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        // US Letter at 72dpi: 612x792pt. The frame width determines how
        // WKWebView lays out content — a zero-width frame produces a
        // single-word-per-line PDF.
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(612.0, 792.0));
        let web_view = unsafe {
            WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config)
        };

        let export_id = next_export_id();
        let delegate = CsPdfNavDelegate::new(tx, export_id, mtm);
        let proto = ProtocolObject::from_retained(delegate);
        unsafe {
            web_view.setNavigationDelegate(Some(&proto));
        }

        match source {
            ContentSource::Url(url_str) => {
                // URL already validated above — unwrap is safe.
                let ns_url = NSURL::URLWithString(&NSString::from_str(&url_str)).unwrap();
                let request = NSURLRequest::requestWithURL(&ns_url);
                unsafe {
                    web_view.loadRequest(&request);
                }
                debug!(target: "export_pdf", "Loading URL in hidden webview");
            }
            ContentSource::Html(html) => {
                unsafe {
                    web_view.loadHTMLString_baseURL(&NSString::from_str(&html), None);
                }
                debug!(target: "export_pdf", "Loading HTML string in hidden webview");
            }
        }

        // Park the webview + delegate so they stay alive until the delegate's
        // callbacks fire (WKWebView holds only a WEAK ref to the delegate). The
        // completion handler / failure path releases them via the registry —
        // replacing the old `std::mem::forget` that leaked a native WKWebView on
        // every export (SW-FX-64).
        park_export(export_id, web_view, proto);
    });

    rx.await
        .map_err(|_| "PDF export channel closed unexpectedly".to_string())?
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn export_pdf_from_url(url: String, output_path: String) -> Result<(), String> {
    debug!(target: "export_pdf", "export_pdf_from_url: url={}, output={}", url, output_path);

    let pdf_bytes = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        generate_pdf(ContentSource::Url(url)),
    )
    .await
    .map_err(|_| "PDF export timed out after 30 seconds".to_string())?
    .map_err(|e| format!("PDF generation failed: {}", e))?;

    let path = PathBuf::from(&output_path);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }
    tokio::fs::write(&path, &pdf_bytes)
        .await
        .map_err(|e| format!("Failed to write PDF to {}: {}", output_path, e))?;

    debug!(target: "export_pdf", "PDF written: {} bytes to {}", pdf_bytes.len(), output_path);
    Ok(())
}

#[tauri::command]
pub async fn export_pdf_from_html(html: String, output_path: String) -> Result<(), String> {
    debug!(target: "export_pdf", "export_pdf_from_html: html_len={}, output={}", html.len(), output_path);

    let pdf_bytes = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        generate_pdf(ContentSource::Html(html)),
    )
    .await
    .map_err(|_| "PDF export timed out after 30 seconds".to_string())?
    .map_err(|e| format!("PDF generation failed: {}", e))?;

    let path = PathBuf::from(&output_path);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }
    tokio::fs::write(&path, &pdf_bytes)
        .await
        .map_err(|e| format!("Failed to write PDF to {}: {}", output_path, e))?;

    debug!(target: "export_pdf", "PDF written: {} bytes to {}", pdf_bytes.len(), output_path);
    Ok(())
}

#[tauri::command]
pub async fn write_binary_file(base64_data: String, output_path: String) -> Result<(), String> {
    crate::commands::await_blocking("write_binary_file", move || {
        debug!(target: "export_pdf", "write_binary_file: output={}", output_path);

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&base64_data)
            .map_err(|e| format!("Base64 decode failed: {}", e))?;

        let path = PathBuf::from(&output_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
        }

        fs::write(&path, &bytes)
            .map_err(|e| format!("Failed to write file {}: {}", output_path, e))?;

        debug!(target: "export_pdf", "Binary file written: {} bytes to {}", bytes.len(), output_path);
        Ok(())
    })
    .await
}

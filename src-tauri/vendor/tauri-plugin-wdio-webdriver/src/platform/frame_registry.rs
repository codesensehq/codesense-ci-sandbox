//! Which native frame each entered frame context is, for platforms that can run a script inside a
//! given frame (macOS: `callAsyncJavaScript…inFrame:`) but cannot name a frame from its element.
//!
//! Switching into a frame posts a one-time nonce into the frame's window; a plugin-owned script in
//! that frame hands the nonce back through a native message handler, which knows the native frame
//! that sent it. `resolve` then files that frame under the context's key (window label + frame
//! path), and every later script for that context runs in it. A context with no frame filed has
//! no frame to run in: callers fail rather than run the script anywhere else.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::platform::FrameId;

/// The prefix of the handshake message a frame's plugin script answers.
pub const HANDSHAKE_PREFIX: &str = "wdio-frame-handshake:";

/// The most entered frames kept; the oldest go first. A session reading many frames in turn enters
/// each anew, so old entries are only ever re-entered through a new switch.
pub const MAX_FRAMES: usize = 256;

/// The key of a frame context: the window's label, then each frame step from the top.
pub fn frame_key(window_label: &str, frame_context: &[FrameId]) -> String {
    let mut key = String::from(window_label);
    for frame in frame_context {
        match frame {
            FrameId::Index(index) => {
                let _ = write!(key, "/index:{index}");
            }
            FrameId::Element(js_var) => {
                let _ = write!(key, "/element:{js_var}");
            }
        }
    }
    key
}

struct Inner<H> {
    /// Handshakes in flight: nonce → (the context key it enters, who waits for it).
    pending: HashMap<String, (String, oneshot::Sender<()>)>,
    /// Entered frames, oldest first.
    frames: VecDeque<(String, H)>,
}

/// Entered frames by context key, and the handshakes that file them. `H` is the platform's frame
/// handle.
pub struct FrameRegistry<H> {
    inner: Mutex<Inner<H>>,
}

impl<H> Default for FrameRegistry<H> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                pending: HashMap::new(),
                frames: VecDeque::new(),
            }),
        }
    }
}

impl<H: Clone> FrameRegistry<H> {
    /// Wait for the handshake `nonce`, which enters the context `key`.
    pub fn expect(&self, nonce: String, key: String) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut inner) = self.inner.lock() {
            inner.pending.insert(nonce, (key, tx));
        }
        rx
    }

    /// Drop a handshake no frame answered.
    pub fn cancel(&self, nonce: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pending.remove(nonce);
        }
    }

    /// A frame answered `nonce`: file it under the context the handshake enters and wake the
    /// waiter. An unknown or already answered nonce files nothing (false).
    pub fn resolve(&self, nonce: &str, frame: H) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        let Some((key, tx)) = inner.pending.remove(nonce) else {
            return false;
        };
        inner.frames.retain(|(filed, _)| *filed != key);
        while inner.frames.len() >= MAX_FRAMES {
            inner.frames.pop_front();
        }
        inner.frames.push_back((key, frame));
        let _ = tx.send(());
        true
    }

    /// The frame filed under `key`, if any.
    pub fn frame(&self, key: &str) -> Option<H> {
        let inner = self.inner.lock().ok()?;
        inner
            .frames
            .iter()
            .find(|(filed, _)| filed == key)
            .map(|(_, frame)| frame.clone())
    }
}

/// The script, run in the frame context a switch starts from, that checks the frame `id` names
/// and posts the handshake `nonce` into it. It returns false when there is no such frame (an index
/// out of range, or a frame with no window) and throws when the element is stale or not a frame,
/// as the switch's own check does.
pub fn handshake_script(id: &FrameId, nonce: &str) -> String {
    let find = match id {
        FrameId::Index(index) => format!(
            r"var frames = document.querySelectorAll('iframe, frame');
                if ({index} >= frames.length) return false;
                var el = frames[{index}];"
        ),
        FrameId::Element(js_var) => format!(
            r"var el = window.{js_var};
                if (!el || !el.isConnected) throw new Error('stale element reference');
                if (el.tagName !== 'IFRAME' && el.tagName !== 'FRAME') throw new Error('element is not a frame');"
        ),
    };
    format!(
        r"(function() {{
                {find}
                var win = el.contentWindow;
                if (!win) return false;
                win.postMessage('{HANDSHAKE_PREFIX}{nonce}', '*');
                return true;
            }})()"
    )
}

/// The plugin's own script for every frame, run in the plugin's private script world: it answers
/// a handshake through `handler` (a message handler only that world sees) and keeps the handshake
/// from the page's own listeners.
pub fn frame_script(handler: &str) -> String {
    format!(
        r"(function () {{
  var prefix = '{HANDSHAKE_PREFIX}';
  window.addEventListener('message', function (event) {{
    var data = event.data;
    if (typeof data !== 'string' || data.indexOf(prefix) !== 0) return;
    event.stopImmediatePropagation();
    try {{ window.webkit.messageHandlers.{handler}.postMessage(data.slice(prefix.length)); }} catch (e) {{}}
  }}, true);
}})();"
    )
}

/// A script that `evaluateJavaScript` would run as an expression, as the body of a
/// DirectEval-style callback function: it reports `{ ok: true, value }` or `{ ok: false, error }`
/// to its last argument.
pub fn expression_as_callback(script: &str) -> String {
    let expression = script.trim_end().trim_end_matches(';');
    format!(
        r"var __wdioReport = arguments[arguments.length - 1];
var __wdioValue;
try {{
  __wdioValue = (
{expression}
  );
}} catch (e) {{
  __wdioReport({{ ok: false, error: (e && e.message) ? String(e.message) : String(e) }});
  return;
}}
__wdioReport({{ ok: true, value: __wdioValue === undefined ? null : __wdioValue }});"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_the_window_and_every_frame_step() {
        assert_eq!(frame_key("main", &[]), "main");
        assert_eq!(
            frame_key(
                "main",
                &[FrameId::Element("__wd_el_a".into()), FrameId::Index(2)]
            ),
            "main/element:__wd_el_a/index:2"
        );
        assert_ne!(
            frame_key("main", &[FrameId::Index(0)]),
            frame_key("other", &[FrameId::Index(0)])
        );
    }

    #[test]
    fn a_resolved_handshake_files_the_frame_and_wakes_the_waiter() {
        let registry = FrameRegistry::<u32>::default();
        let mut rx = registry.expect("n1".into(), "main/index:0".into());
        assert!(registry.resolve("n1", 7));
        assert!(rx.try_recv().is_ok());
        assert_eq!(registry.frame("main/index:0"), Some(7));
    }

    #[test]
    fn an_unknown_or_repeated_nonce_files_nothing() {
        let registry = FrameRegistry::<u32>::default();
        assert!(!registry.resolve("nobody", 1));
        let _rx = registry.expect("n1".into(), "main/index:0".into());
        assert!(registry.resolve("n1", 1));
        assert!(!registry.resolve("n1", 2));
        assert_eq!(registry.frame("main/index:0"), Some(1));
    }

    #[test]
    fn a_cancelled_handshake_files_nothing() {
        let registry = FrameRegistry::<u32>::default();
        let _rx = registry.expect("n1".into(), "main/index:0".into());
        registry.cancel("n1");
        assert!(!registry.resolve("n1", 1));
        assert_eq!(registry.frame("main/index:0"), None);
    }

    #[test]
    fn a_context_never_entered_has_no_frame() {
        let registry = FrameRegistry::<u32>::default();
        assert_eq!(registry.frame("main/index:0"), None);
    }

    #[test]
    fn entering_a_context_again_files_the_frame_answering_now() {
        let registry = FrameRegistry::<u32>::default();
        let _a = registry.expect("n1".into(), "main/index:0".into());
        registry.resolve("n1", 1);
        let _b = registry.expect("n2".into(), "main/index:0".into());
        registry.resolve("n2", 2);
        assert_eq!(registry.frame("main/index:0"), Some(2));
    }

    #[test]
    fn the_oldest_frames_go_past_the_cap() {
        let registry = FrameRegistry::<usize>::default();
        for n in 0..=MAX_FRAMES {
            let _rx = registry.expect(format!("n{n}"), format!("main/index:{n}"));
            registry.resolve(&format!("n{n}"), n);
        }
        assert_eq!(registry.frame("main/index:0"), None);
        assert_eq!(registry.frame("main/index:1"), Some(1));
        assert_eq!(
            registry.frame(&format!("main/index:{MAX_FRAMES}")),
            Some(MAX_FRAMES)
        );
    }

    #[test]
    fn the_handshake_checks_the_frame_then_posts_the_nonce_into_it() {
        let by_element = handshake_script(&FrameId::Element("__wd_el_x".into()), "abc");
        assert!(by_element.contains("window.__wd_el_x"));
        assert!(by_element.contains("throw new Error('stale element reference')"));
        assert!(by_element.contains("throw new Error('element is not a frame')"));
        assert!(by_element.contains("win.postMessage('wdio-frame-handshake:abc', '*')"));

        let by_index = handshake_script(&FrameId::Index(3), "abc");
        assert!(by_index.contains("if (3 >= frames.length) return false;"));
        assert!(by_index.contains("win.postMessage('wdio-frame-handshake:abc', '*')"));
    }

    #[test]
    fn the_frame_script_answers_only_the_handshake_through_its_handler() {
        let script = frame_script("wdioFrame");
        assert!(script.contains("var prefix = 'wdio-frame-handshake:'"));
        assert!(script.contains("data.indexOf(prefix) !== 0) return;"));
        assert!(script.contains("event.stopImmediatePropagation();"));
        assert!(script.contains(
            "window.webkit.messageHandlers.wdioFrame.postMessage(data.slice(prefix.length))"
        ));
    }

    #[test]
    fn an_expression_reports_its_value_or_its_error() {
        let body = expression_as_callback("window['x'];  \n");
        assert!(body.contains("__wdioValue = (\nwindow['x']\n  );"));
        assert!(body.contains("__wdioReport({ ok: false, error:"));
        assert!(body.contains("__wdioReport({ ok: true, value:"));
    }
}

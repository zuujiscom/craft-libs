//! Page hooks around saving: the "Leave site?" question, the tab being hidden, and downloads.

use js_sys::{Array, Uint8Array};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{BeforeUnloadEvent, Blob, BlobPropertyBag, HtmlAnchorElement, VisibilityState};

use crate::idb::{Listeners, js};

/// While this lives, leaving or reloading the page asks first whenever `unsaved()` is true.
/// Browsers show their own wording; the question is skipped for pages the user never touched.
pub struct UnloadGuard {
    _listeners: Listeners,
}

impl UnloadGuard {
    pub fn new(unsaved: impl Fn() -> bool + 'static) -> Result<Self, String> {
        let window = web_sys::window().ok_or("no window")?;
        let mut listeners = Listeners::new();
        listeners
            .add(&window, "beforeunload", move |event| {
                if unsaved()
                    && let Some(event) = event.dyn_ref::<BeforeUnloadEvent>()
                {
                    event.prevent_default();
                    // Older Safari and Chrome ask only when returnValue is set.
                    event.set_return_value("unsaved changes");
                }
            })
            .map_err(js)?;
        Ok(UnloadGuard { _listeners: listeners })
    }
}

/// While this lives, `hidden()` runs whenever the tab is hidden: the last moment browsers promise
/// to the page before they may freeze or discard it (Safari on iPhone and iPad discards hidden
/// tabs freely). A good place to write recovery copies at once instead of at the next interval.
pub struct HiddenHook {
    _listeners: Listeners,
}

impl HiddenHook {
    pub fn new(mut hidden: impl FnMut() + 'static) -> Result<Self, String> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        let doc = document.clone();
        let mut listeners = Listeners::new();
        listeners
            .add(&document, "visibilitychange", move |_| {
                if doc.visibility_state() == VisibilityState::Hidden {
                    hidden();
                }
            })
            .map_err(js)?;
        Ok(HiddenHook { _listeners: listeners })
    }
}

/// Save `bytes` as a browser download named `name` (only its last path component is used).
pub fn download(name: &str, mime: &str, bytes: &[u8]) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let name = name.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or("download");
    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let blob = Blob::new_with_u8_array_sequence_and_options(&Array::of1(&Uint8Array::from(bytes)), &options).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let a: HtmlAnchorElement = document.create_element("a").map_err(js)?.dyn_into().map_err(|_| "could not create a link")?;
    a.set_href(&url);
    a.set_download(name);
    a.style().set_property("display", "none").map_err(js)?;
    // Safari only downloads from a link that is in the document.
    document.body().ok_or("no body")?.append_child(&a).map_err(js)?;
    a.click();
    a.remove();
    // Revoking at once can cancel the download in Safari; a minute later is safe everywhere.
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 60_000).map(|_| ()).map_err(|e: JsValue| js(e))
}

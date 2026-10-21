//! The browser's Web Locks (`navigator.locks`), where it has them (Safari 15.4+, Chrome 69+,
//! Firefox 96+). A lock lasts until its holder releases it or the page goes away, crash included.

use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

use crate::idb::{before, field};

fn locks() -> Option<JsValue> {
    let locks = Reflect::get(&web_sys::window()?.navigator(), &JsValue::from_str("locks")).ok()?;
    (!locks.is_undefined() && !locks.is_null()).then_some(locks)
}

fn method(locks: &JsValue, name: &str) -> Option<Function> {
    Reflect::get(locks, &JsValue::from_str(name)).ok()?.dyn_into().ok()
}

/// Hold `name` for as long as the page lives. False when the browser has no Web Locks.
pub(crate) fn hold_forever(name: &str) -> bool {
    let Some(locks) = locks() else { return false };
    let Some(request) = method(&locks, "request") else { return false };
    let forever = Closure::<dyn FnMut(JsValue) -> Promise>::new(|_| Promise::new(&mut |_, _| {}));
    if request.call2(&locks, &JsValue::from_str(name), forever.as_ref().unchecked_ref()).is_ok() {
        forever.forget();
        true
    } else {
        false
    }
}

/// Names of the locks held right now (by any tab of this site) that start with `prefix`, without
/// it. `None` when the browser can't tell.
pub(crate) async fn held(prefix: &str) -> Option<Vec<String>> {
    let locks = locks()?;
    let promise: Promise = method(&locks, "query")?.call0(&locks).ok()?.dyn_into().ok()?;
    let snapshot = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    let held: Array = field(&snapshot, "held").dyn_into().ok()?;
    Some(held.iter().filter_map(|l| field(&l, "name").as_string()?.strip_prefix(prefix).map(str::to_string)).collect())
}

/// An exclusive lock, held until dropped.
pub(crate) struct Exclusive {
    release: Option<Function>,
    _granted: Closure<dyn FnMut(JsValue) -> Promise>,
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

/// Wait (until `deadline`) for the exclusive lock `name`. `Ok(None)`: the browser has no Web Locks.
pub(crate) async fn exclusive(name: &str, deadline: f64) -> Result<Option<Exclusive>, String> {
    let Some(locks) = locks() else { return Ok(None) };
    let Some(request) = method(&locks, "request") else { return Ok(None) };
    // The lock is held while the promise the callback returns is pending; dropping `Exclusive`
    // settles it. A grant that comes after we stopped waiting is released at once, because by
    // then `release` has already been called.
    let mut release = None;
    let held = Promise::new(&mut |resolve, _| release = Some(resolve));
    let mut on_grant = None;
    let acquired = Promise::new(&mut |resolve, _| on_grant = Some(resolve));
    let granted = Closure::<dyn FnMut(JsValue) -> Promise>::new(move |_| {
        if let Some(f) = &on_grant {
            let _ = f.call0(&JsValue::NULL);
        }
        held.clone()
    });
    request.call2(&locks, &JsValue::from_str(name), granted.as_ref().unchecked_ref()).map_err(crate::idb::js)?;
    match before(&acquired, deadline).await {
        Ok(_) => Ok(Some(Exclusive { release, _granted: granted })),
        Err(e) => {
            // The request stays queued: keep its callback alive for the late grant.
            if let Some(release) = release {
                let _ = release.call0(&JsValue::NULL);
            }
            granted.forget();
            Err(e)
        }
    }
}

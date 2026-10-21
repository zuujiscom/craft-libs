//! The few IndexedDB pieces the store needs, as promises with a deadline.

use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, EventTarget, IdbDatabase, IdbRequest, IdbTransaction};

/// No single wait (a request, the whole startup read) takes longer than this.
pub(crate) const TIMEOUT_MS: f64 = 20_000.0;

pub(crate) fn js(e: JsValue) -> String {
    if let Some(e) = e.dyn_ref::<js_sys::Error>() {
        return String::from(e.message());
    }
    if let Some(e) = e.dyn_ref::<web_sys::DomException>() {
        return format!("{}: {}", e.name(), e.message());
    }
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

pub(crate) fn field(o: &JsValue, k: &str) -> JsValue {
    Reflect::get(o, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

type Listener = (EventTarget, &'static str, Closure<dyn FnMut(Event)>);

/// Event listeners that are removed when this is dropped.
pub(crate) struct Listeners(Vec<Listener>);

impl Listeners {
    pub(crate) fn new() -> Self {
        Self(Vec::new())
    }
    pub(crate) fn add(&mut self, target: &EventTarget, name: &'static str, f: impl FnMut(Event) + 'static) -> Result<(), JsValue> {
        let cb = Closure::<dyn FnMut(Event)>::new(f);
        target.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref())?;
        self.0.push((target.clone(), name, cb));
        Ok(())
    }
}

impl Drop for Listeners {
    fn drop(&mut self) {
        for (target, name, cb) in &self.0 {
            let _ = target.remove_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
        }
    }
}

/// A request's outcome as a promise (kept alive with its listeners).
pub(crate) struct Pending {
    pub(crate) promise: Promise,
    _listeners: Listeners,
}

pub(crate) fn request(request: &IdbRequest) -> Pending {
    let mut listeners = Listeners::new();
    let promise = Promise::new(&mut |resolve, reject| {
        let req = request.clone();
        let failed = reject.clone();
        if let Err(e) = listeners.add(request, "success", move |_| match req.result() {
            Ok(v) => {
                let _ = resolve.call1(&JsValue::NULL, &v);
            }
            Err(e) => {
                let _ = failed.call1(&JsValue::NULL, &e);
            }
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
        let req = request.clone();
        let failed = reject.clone();
        if let Err(e) = listeners.add(request, "error", move |_| {
            let e = req.error().ok().flatten().map(JsValue::from).unwrap_or_else(|| JsValue::from_str("IndexedDB request failed"));
            let _ = failed.call1(&JsValue::NULL, &e);
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
    });
    Pending { promise, _listeners: listeners }
}

/// Wait for `promise`, but not past `deadline` (`Date.now()` milliseconds).
pub(crate) async fn before(promise: &Promise, deadline: f64) -> Result<JsValue, String> {
    let window = web_sys::window().ok_or("no window")?;
    let mut timer = None;
    let mut callback = None;
    let timeout = Promise::new(&mut |_, reject| {
        let cb = Closure::<dyn FnMut()>::new(move || {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("browser storage timed out"));
        });
        timer = Some(window.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            (deadline - js_sys::Date::now()).clamp(0.0, TIMEOUT_MS) as i32,
        ));
        callback = Some(cb);
    });
    let id = timer.ok_or("could not start the storage timeout")?.map_err(js)?;
    let result = JsFuture::from(Promise::race(&Array::of2(promise, &timeout))).await.map_err(js);
    window.clear_timeout_with_handle(id);
    drop(callback);
    result
}

/// Open (creating on first use) database `name` with the one object store `store`.
pub(crate) async fn open(name: &str, store: &'static str, deadline: f64) -> Result<IdbDatabase, String> {
    let factory = web_sys::window().ok_or("no window")?.indexed_db().map_err(js)?.ok_or("IndexedDB is unavailable")?;
    let req = factory.open_with_u32(name, 1).map_err(js)?;
    let mut upgrade = Listeners::new();
    let upgrade_req = req.clone();
    upgrade
        .add(&req, "upgradeneeded", move |_| {
            let result = upgrade_req.result().and_then(|v| v.dyn_into::<IdbDatabase>()).and_then(|db| db.create_object_store(store));
            if result.is_err()
                && let Some(tx) = upgrade_req.transaction()
            {
                let _ = tx.abort();
            }
        })
        .map_err(js)?;
    let pending = request(&req);
    match before(&pending.promise, deadline).await {
        Ok(value) => value.dyn_into().map_err(|_| "IndexedDB returned no database".into()),
        Err(e) => {
            // An open request can't be cancelled: close a late connection when it arrives.
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(value) = JsFuture::from(pending.promise.clone()).await
                    && let Ok(db) = value.dyn_into::<IdbDatabase>()
                {
                    db.close();
                }
                drop(pending);
                drop(upgrade);
            });
            Err(e)
        }
    }
}

/// Wait until `tx` commits; an abort or error (a full disk: `QuotaExceededError`) is the error.
pub(crate) async fn committed(tx: &IdbTransaction, deadline: f64) -> Result<(), String> {
    let mut listeners = Listeners::new();
    let promise = Promise::new(&mut |resolve, reject| {
        if let Err(e) = listeners.add(tx, "complete", move |_| {
            let _ = resolve.call0(&JsValue::NULL);
        }) {
            let _ = reject.call1(&JsValue::NULL, &e);
        }
        for event in ["abort", "error"] {
            let t = tx.clone();
            let failed: Function = reject.clone();
            if let Err(e) = listeners.add(tx, event, move |_| {
                let e = t.error().map(JsValue::from).unwrap_or_else(|| JsValue::from_str("the browser storage transaction was aborted"));
                let _ = failed.call1(&JsValue::NULL, &e);
            }) {
                let _ = reject.call1(&JsValue::NULL, &e);
            }
        }
    });
    let result = before(&promise, deadline).await.map(|_| ());
    if result.is_err() {
        let _ = tx.abort();
    }
    result
}

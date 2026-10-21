//! A small persistent key → bytes map in IndexedDB, for apps whose recovery code is synchronous
//! (a trait with `read`/`write`/`list`) and keeps its entries by name rather than as documents.
//!
//! The app reads everything once at startup ([`open_kv`]) and works on its own in-memory copy;
//! [`Kv::put`] and [`Kv::delete`] queue the change, which runs in the background in order, one
//! transaction each. A failed write (a full disk is `QuotaExceededError`) is kept for
//! [`Kv::take_error`]. Tabs tell each other which keys they put or deleted over a
//! `BroadcastChannel`, so [`ForeignChange`] lets an app keep its copy in step without re-reading.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use js_sys::{Array, Uint8Array};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{BroadcastChannel, IdbDatabase, IdbTransactionMode, MessageEvent};

use crate::idb::{TIMEOUT_MS, before, committed, js, request};

const STORE: &str = "kv";

/// What another tab did to the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForeignChange {
    /// The tab stored a new value under the key (not sent over: read it from IndexedDB if needed).
    Put(String),
    Delete(String),
}

enum Op {
    Put(String, Vec<u8>),
    Delete(String),
}

struct Inner {
    db: IdbDatabase,
    queue: RefCell<VecDeque<Op>>,
    running: RefCell<bool>,
    error: RefCell<Option<String>>,
    channel: Option<BroadcastChannel>,
    _listener: RefCell<Option<Closure<dyn FnMut(MessageEvent)>>>,
}

/// The map's connection. Cheap to clone.
#[derive(Clone)]
pub struct Kv(Rc<Inner>);

/// Open database `name` and read every entry. `on_foreign` is called for changes other tabs
/// announce.
pub async fn open_kv(name: &str, on_foreign: impl Fn(ForeignChange) + 'static) -> Result<(Kv, Vec<(String, Vec<u8>)>), String> {
    let deadline = js_sys::Date::now() + TIMEOUT_MS;
    let db = crate::idb::open(name, STORE, deadline).await?;
    let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readonly).map_err(js)?;
    let store = tx.object_store(STORE).map_err(js)?;
    let keys = request(&store.get_all_keys().map_err(js)?);
    let values = request(&store.get_all().map_err(js)?);
    let keys = Array::from(&before(&keys.promise, deadline).await?);
    let values = Array::from(&before(&values.promise, deadline).await?);
    let mut entries = Vec::new();
    for (k, v) in keys.iter().zip(values.iter()) {
        let (Some(k), Ok(v)) = (k.as_string(), v.dyn_into::<Uint8Array>()) else { continue };
        entries.push((k, v.to_vec()));
    }
    let channel = BroadcastChannel::new(&format!("{name}.kv")).ok();
    let inner = Rc::new(Inner {
        db,
        queue: RefCell::new(VecDeque::new()),
        running: RefCell::new(false),
        error: RefCell::new(None),
        channel,
        _listener: RefCell::new(None),
    });
    if let Some(channel) = &inner.channel {
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let data = e.data();
            let key = js_sys::Reflect::get(&data, &"key".into()).ok().and_then(|k| k.as_string());
            let op = js_sys::Reflect::get(&data, &"op".into()).ok().and_then(|k| k.as_string());
            match (op.as_deref(), key) {
                (Some("put"), Some(k)) => on_foreign(ForeignChange::Put(k)),
                (Some("delete"), Some(k)) => on_foreign(ForeignChange::Delete(k)),
                _ => {}
            }
        });
        channel.set_onmessage(Some(listener.as_ref().unchecked_ref()));
        *inner._listener.borrow_mut() = Some(listener);
    }
    Ok((Kv(inner), entries))
}

impl Kv {
    /// Queue a write of `bytes` under `key`.
    pub fn put(&self, key: &str, bytes: &[u8]) {
        self.push(Op::Put(key.to_string(), bytes.to_vec()));
    }

    /// Queue the deletion of `key`.
    pub fn delete(&self, key: &str) {
        self.push(Op::Delete(key.to_string()));
    }

    /// The first failed write since the last call, if any.
    pub fn take_error(&self) -> Option<String> {
        self.0.error.borrow_mut().take()
    }

    fn push(&self, op: Op) {
        self.0.queue.borrow_mut().push_back(op);
        if std::mem::replace(&mut *self.0.running.borrow_mut(), true) {
            return;
        }
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let next = this.0.queue.borrow_mut().pop_front();
                let Some(op) = next else {
                    *this.0.running.borrow_mut() = false;
                    break;
                };
                if let Err(e) = this.run(&op).await {
                    log::warn!("browser storage write failed: {e}");
                    this.0.error.borrow_mut().get_or_insert(e);
                }
            }
        });
    }

    async fn run(&self, op: &Op) -> Result<(), String> {
        let deadline = js_sys::Date::now() + TIMEOUT_MS;
        let tx = self.0.db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(js)?;
        let store = tx.object_store(STORE).map_err(js)?;
        let (kind, key) = match op {
            Op::Put(key, bytes) => {
                // A fresh buffer: the stored value is the whole ArrayBuffer behind a view.
                let value = Uint8Array::from(bytes.as_slice());
                store.put_with_key(&value, &JsValue::from_str(key)).map_err(js)?;
                ("put", key)
            }
            Op::Delete(key) => {
                store.delete(&JsValue::from_str(key)).map_err(js)?;
                ("delete", key)
            }
        };
        committed(&tx, deadline).await?;
        if let Some(channel) = &self.0.channel {
            let message = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&message, &"op".into(), &kind.into());
            let _ = js_sys::Reflect::set(&message, &"key".into(), &JsValue::from_str(key));
            let _ = channel.post_message(&message);
        }
        Ok(())
    }
}

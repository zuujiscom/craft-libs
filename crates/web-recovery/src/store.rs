//! The recovery store: one IndexedDB database per app (`<app>.recovery`), one object store of
//! entries `{name, path, meta, revision, session, savedAt, bytes: Blob}` keyed by the app's key.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Blob, IdbCursorWithValue, IdbDatabase, IdbTransactionMode};

use crate::Warning;
use crate::idb::{TIMEOUT_MS, before, committed, field, js, request};
use crate::locks;
use crate::plan::{Listed, plan};

const ENTRIES: &str = "entries";
/// Startup reads at most this much; anything past it stays stored for a later load.
const MAX_LOAD_BYTES: f64 = 1024.0 * 1024.0 * 1024.0;

/// What to store for one document.
pub struct Snapshot<'a> {
    /// Shown in the restore prompt.
    pub name: &'a str,
    /// Where the document was opened from or last saved to, if anywhere.
    pub path: Option<&'a str>,
    /// Anything else the app wants back (often JSON).
    pub meta: Option<&'a str>,
    /// The copy itself, usually the app's native file format.
    pub bytes: &'a [u8],
}

/// The outcome of one [`Store::put`], reported once IndexedDB has committed it (or failed).
#[derive(Debug)]
pub struct Written {
    pub key: String,
    pub revision: u64,
    pub result: Result<(), String>,
}

/// A copy a previous session left behind; it now belongs to this session.
pub struct Recovered {
    /// Pass it to [`Store::put`] and [`Store::remove`] to keep using this entry.
    pub key: String,
    pub name: String,
    pub path: Option<String>,
    pub meta: Option<String>,
    pub revision: u64,
    /// When it was written (`Date.now()` milliseconds).
    pub saved_at: f64,
    pub bytes: Vec<u8>,
}

/// What [`open`] found.
pub struct Opened {
    pub store: Store,
    /// Copies left by sessions that are gone, newest first.
    pub recovered: Vec<Recovered>,
    /// Worth telling the user (see [`Warning`]).
    pub warnings: Vec<Warning>,
}

/// This session's recovery store. Cheap to clone; every clone is the same store.
#[derive(Clone)]
pub struct Store(Rc<Inner>);

struct Inner {
    db: Option<IdbDatabase>,
    session: String,
    unavailable: Option<String>,
    written: RefCell<Vec<Written>>,
    waker: RefCell<Option<Rc<dyn Fn()>>>,
}

/// Open app `app`'s store (a short, stable name such as `"photocraft"`), hold this session's lock
/// and take over the copies of sessions that are gone. Never fails: without IndexedDB the app runs
/// as before, [`Opened::warnings`] says why, and every [`Store::put`] reports the problem.
///
/// Call it before the app is built, so the restore prompt has everything at launch.
pub async fn open(app: &str) -> Opened {
    let session = format!("{:x}{:08x}", js_sys::Date::now() as u64, (js_sys::Math::random() * f64::from(u32::MAX)) as u32);
    let prefix = format!("{app}.recovery.session.");
    locks::hold_forever(&format!("{prefix}{session}"));
    let deadline = js_sys::Date::now() + TIMEOUT_MS;
    let db = match crate::idb::open(&format!("{app}.recovery"), ENTRIES, deadline).await {
        Ok(db) => db,
        Err(e) => {
            log::warn!("crash recovery unavailable: {e}");
            let store = Store::new(None, session, Some(e.clone()));
            return Opened { store, recovered: Vec::new(), warnings: vec![Warning::Unavailable(e)] };
        }
    };
    let mut warnings = Vec::new();
    let recovered = match take_over(&db, app, &prefix, &session, deadline, &mut warnings).await {
        Ok(found) => found,
        Err(e) => {
            log::warn!("crash recovery could not read its entries: {e}");
            warnings.push(Warning::ReadFailed(e));
            Vec::new()
        }
    };
    Opened { store: Store::new(Some(db), session, None), recovered, warnings }
}

/// Claim every entry of a session that is gone for `session`, then read them.
async fn take_over(
    db: &IdbDatabase,
    app: &str,
    prefix: &str,
    session: &str,
    deadline: f64,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<Recovered>, String> {
    // Two tabs opened together each wait here for the other's claim, so an entry is taken once.
    let _claim = locks::exclusive(&format!("{app}.recovery.claim"), deadline).await?;
    let running = locks::held(prefix).await;

    let tx = db.transaction_with_str_and_mode(ENTRIES, IdbTransactionMode::Readonly).map_err(js)?;
    let req = tx.object_store(ENTRIES).map_err(js)?.open_cursor().map_err(js)?;
    let mut scanned: Vec<(JsValue, JsValue, Option<Blob>)> = Vec::new();
    let mut listed = Vec::new();
    let mut next = request(&req);
    loop {
        let value = before(&next.promise, deadline).await?;
        if value.is_null() {
            break;
        }
        let cursor = value.dyn_into::<IdbCursorWithValue>().map_err(|_| "IndexedDB returned no cursor")?;
        let key = cursor.key().map_err(js)?;
        let v = cursor.value().map_err(js)?;
        let blob = field(&v, "bytes").dyn_into::<Blob>().ok();
        listed.push(Listed {
            name: field(&v, "name").as_string().unwrap_or_else(|| "Untitled".into()),
            session: field(&v, "session").as_string().unwrap_or_default(),
            saved_at: field(&v, "savedAt").as_f64().unwrap_or(0.0),
            size: blob.as_ref().map(Blob::size),
        });
        scanned.push((key, v, blob));
        // Ask for the next row before continuing; Blob reads wait until the scan is done, since
        // awaiting them here would let the transaction close.
        next = request(&req);
        cursor.continue_().map_err(js)?;
    }

    let plan = plan(&listed, session, running.as_deref(), MAX_LOAD_BYTES);
    warnings.extend(plan.warnings);
    if plan.load.is_empty() && plan.damaged.is_empty() {
        return Ok(Vec::new());
    }
    let tx = db.transaction_with_str_and_mode(ENTRIES, IdbTransactionMode::Readwrite).map_err(js)?;
    let store = tx.object_store(ENTRIES).map_err(js)?;
    for &i in &plan.load {
        let (key, value, _) = &scanned[i];
        Reflect::set(value, &JsValue::from_str("session"), &JsValue::from_str(session)).map_err(js)?;
        store.put_with_key(value, key).map_err(js)?;
    }
    for &i in &plan.damaged {
        store.delete(&scanned[i].0).map_err(js)?;
    }
    committed(&tx, deadline).await?;

    let mut found = Vec::new();
    for i in plan.load {
        let (key, value, blob) = &scanned[i];
        let Some(blob) = blob else { continue };
        let buffer = before(&blob.array_buffer(), deadline).await?;
        found.push(Recovered {
            key: key.as_string().unwrap_or_default(),
            name: listed[i].name.clone(),
            path: field(value, "path").as_string(),
            meta: field(value, "meta").as_string(),
            revision: field(value, "revision").as_f64().unwrap_or(0.0) as u64,
            saved_at: listed[i].saved_at,
            bytes: Uint8Array::new(&buffer).to_vec(),
        });
    }
    Ok(found)
}

impl Store {
    fn new(db: Option<IdbDatabase>, session: String, unavailable: Option<String>) -> Self {
        Store(Rc::new(Inner { db, session, unavailable, written: RefCell::new(Vec::new()), waker: RefCell::new(None) }))
    }

    /// This page load's id.
    pub fn session(&self) -> &str {
        &self.0.session
    }

    /// A key for a new entry of this session: `<session>-<id>`, where `id` is unique in the app
    /// (a document id).
    pub fn key(&self, id: impl std::fmt::Display) -> String {
        format!("{}-{id}", self.0.session)
    }

    /// Whether copies can be kept at all (`false`: [`Opened::warnings`] says why).
    pub fn is_available(&self) -> bool {
        self.0.db.is_some()
    }

    /// Where copies are kept, as the user knows it.
    pub fn location(&self) -> &'static str {
        "browser storage"
    }

    /// Called after each write finishes (pass the UI's "repaint", so an idle page picks the
    /// result up).
    pub fn set_waker(&self, wake: impl Fn() + 'static) {
        *self.0.waker.borrow_mut() = Some(Rc::new(wake));
    }

    /// Create or replace entry `key` with `snapshot` as `revision`, in the background. An error
    /// here means nothing was queued; otherwise the outcome comes from [`Store::take_written`].
    /// Writes to one key land in the order they were made.
    pub fn put(&self, key: &str, revision: u64, snapshot: Snapshot<'_>) -> Result<(), String> {
        let db = match (&self.0.db, &self.0.unavailable) {
            (Some(db), _) => db.clone(),
            (None, Some(e)) => return Err(format!("browser storage is unavailable: {e}")),
            (None, None) => return Err("browser storage is unavailable".into()),
        };
        let record = record(&snapshot, revision, &self.0.session)?;
        let tx = db.transaction_with_str_and_mode(ENTRIES, IdbTransactionMode::Readwrite).map_err(js)?;
        let queued = tx.object_store(ENTRIES).and_then(|s| s.put_with_key(&record, &JsValue::from_str(key)));
        if let Err(e) = queued {
            let _ = tx.abort();
            return Err(js(e));
        }
        let inner = self.0.clone();
        let key = key.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let result = committed(&tx, js_sys::Date::now() + TIMEOUT_MS).await;
            if let Err(e) = &result {
                log::warn!("recovery copy {key} was not written: {e}");
            }
            inner.written.borrow_mut().push(Written { key, revision, result });
            let wake = inner.waker.borrow().clone();
            if let Some(wake) = wake {
                wake();
            }
        });
        Ok(())
    }

    /// The writes that finished since the last call, in the order they finished.
    pub fn take_written(&self) -> Vec<Written> {
        std::mem::take(&mut self.0.written.borrow_mut())
    }

    /// Delete entry `key` (its document was saved or closed), in the background. A missing entry
    /// is no error; a failure is only logged, since the entry is then offered again next time,
    /// which loses nothing.
    pub fn remove(&self, key: &str) {
        let Some(db) = self.0.db.clone() else { return };
        let key = key.to_string();
        let tx = match db.transaction_with_str_and_mode(ENTRIES, IdbTransactionMode::Readwrite) {
            Ok(tx) => tx,
            Err(e) => return log::warn!("could not remove recovery copy {key}: {}", js(e)),
        };
        if let Err(e) = tx.object_store(ENTRIES).and_then(|s| s.delete(&JsValue::from_str(&key))) {
            let _ = tx.abort();
            return log::warn!("could not remove recovery copy {key}: {}", js(e));
        }
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = committed(&tx, js_sys::Date::now() + TIMEOUT_MS).await {
                log::warn!("could not remove recovery copy {key}: {e}");
            }
        });
    }
}

fn record(s: &Snapshot<'_>, revision: u64, session: &str) -> Result<Object, String> {
    let o = Object::new();
    let blob = Blob::new_with_u8_array_sequence(&Array::of1(&Uint8Array::from(s.bytes))).map_err(js)?;
    let text = |v: Option<&str>| v.map_or(JsValue::NULL, JsValue::from_str);
    for (k, v) in [
        ("name", JsValue::from_str(s.name)),
        ("path", text(s.path)),
        ("meta", text(s.meta)),
        ("revision", JsValue::from_f64(revision as f64)),
        ("session", JsValue::from_str(session)),
        ("savedAt", JsValue::from_f64(js_sys::Date::now())),
        ("bytes", blob.into()),
    ] {
        Reflect::set(&o, &JsValue::from_str(k), &v).map_err(js)?;
    }
    Ok(o)
}

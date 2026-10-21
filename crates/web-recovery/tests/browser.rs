//! Real IndexedDB and Web Locks: `cargo test -p craft-web-recovery --target wasm32-unknown-unknown`
//! with `wasm-bindgen-test-runner` as the runner and a browser driver (`SAFARIDRIVER`,
//! `CHROMEDRIVER` or `GECKODRIVER`).
#![cfg(target_arch = "wasm32")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::rc::Rc;

use craft_web_recovery::{Snapshot, Store, Warning, Written, open};
use js_sys::{Function, Promise};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// A database of its own for each test.
fn app(test: &str) -> String {
    format!("test-{test}-{}", js_sys::Math::random().to_bits())
}

async fn run_js(body: &str, db: &str) -> JsValue {
    let f = Function::new_with_args("db", body);
    let p: Promise = f.call1(&JsValue::NULL, &JsValue::from_str(db)).unwrap().into();
    JsFuture::from(p).await.unwrap()
}

/// Entries as a crashed session left them: one good copy, one damaged.
async fn leave_crashed(app: &str) {
    run_js(
        "return new Promise((ok, fail) => {
            const r = indexedDB.open(db, 1);
            r.onupgradeneeded = () => r.result.createObjectStore('entries');
            r.onerror = () => fail(r.error);
            r.onsuccess = () => {
                const d = r.result, tx = d.transaction('entries', 'readwrite'), s = tx.objectStore('entries');
                s.put({name: 'Crashed', path: '/docs/a.craft', meta: '{\"zoom\":2}', revision: 7, session: 'gone',
                       savedAt: Date.now(), bytes: new Blob([new Uint8Array([1, 2, 3])])}, 'gone-1');
                s.put({name: 'Broken', session: 'gone', savedAt: 1, bytes: 'not a blob'}, 'gone-2');
                tx.oncomplete = () => { d.close(); ok(); };
                tx.onerror = () => fail(tx.error);
            };
        })",
        &format!("{app}.recovery"),
    )
    .await;
}

async fn keys(app: &str) -> Vec<String> {
    let v = run_js(
        "return new Promise((ok, fail) => {
            const r = indexedDB.open(db, 1);
            r.onupgradeneeded = () => r.result.createObjectStore('entries');
            r.onsuccess = () => {
                const d = r.result, q = d.transaction('entries').objectStore('entries').getAllKeys();
                q.onsuccess = () => { d.close(); ok(q.result); };
                q.onerror = () => fail(q.error);
            };
        })",
        &format!("{app}.recovery"),
    )
    .await;
    js_sys::Array::from(&v).iter().filter_map(|k| k.as_string()).collect()
}

async fn sleep(ms: i32) {
    let p = Promise::new(&mut |ok, _| {
        web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&ok, ms).unwrap();
    });
    JsFuture::from(p).await.unwrap();
}

async fn written(store: &Store, n: usize) -> Vec<Written> {
    let mut all = Vec::new();
    for _ in 0..200 {
        all.extend(store.take_written());
        if all.len() >= n {
            return all;
        }
        sleep(25).await;
    }
    panic!("only {} of {n} writes finished", all.len());
}

fn snap(bytes: &[u8]) -> Snapshot<'_> {
    Snapshot { name: "Doc", path: None, meta: None, bytes }
}

#[wasm_bindgen_test]
async fn a_crashed_sessions_copies_are_recovered_once() {
    let app = app("crashed");
    leave_crashed(&app).await;
    let first = open(&app).await;
    assert_eq!(first.warnings, vec![Warning::Damaged { name: "Broken".into() }]);
    assert_eq!(first.recovered.len(), 1);
    let r = &first.recovered[0];
    assert_eq!((r.key.as_str(), r.name.as_str(), r.path.as_deref(), r.meta.as_deref()), ("gone-1", "Crashed", Some("/docs/a.craft"), Some("{\"zoom\":2}")));
    assert_eq!((r.revision, r.bytes.as_slice()), (7, &[1u8, 2, 3][..]));
    // The damaged copy is gone; the recovered one stays until the app removes it.
    assert_eq!(keys(&app).await, vec!["gone-1".to_string()]);
    // A second tab doesn't take what the first (still running) recovered.
    let second = open(&app).await;
    assert!(second.recovered.is_empty());
    assert!(second.warnings.is_empty());
}

#[wasm_bindgen_test]
async fn writes_are_acknowledged_after_commit_and_wake_the_app() {
    let app = app("write");
    let opened = open(&app).await;
    let store = opened.store;
    assert!(store.is_available());
    let woken = Rc::new(Cell::new(0));
    let w = woken.clone();
    store.set_waker(move || w.set(w.get() + 1));
    let key = store.key(1);
    store.put(&key, 1, snap(&[1])).unwrap();
    store.put(&key, 2, snap(&[2, 2])).unwrap();
    let done = written(&store, 2).await;
    assert_eq!(done.iter().map(|w| (w.key.as_str(), w.revision, w.result.is_ok())).collect::<Vec<_>>(), vec![(key.as_str(), 1, true), (key.as_str(), 2, true)]);
    assert_eq!(woken.get(), 2);
    assert_eq!(keys(&app).await, vec![key.clone()]);
    store.remove(&key);
    for _ in 0..200 {
        if keys(&app).await.is_empty() {
            return;
        }
        sleep(25).await;
    }
    panic!("the entry was not removed");
}

#[wasm_bindgen_test]
async fn a_running_tabs_copies_are_left_alone() {
    let app = app("running");
    let a = open(&app).await;
    a.store.put(&a.store.key("x"), 1, snap(b"mine")).unwrap();
    written(&a.store, 1).await;
    let b = open(&app).await;
    assert!(b.recovered.is_empty());
    assert_ne!(a.store.session(), b.store.session());
}

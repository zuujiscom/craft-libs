# craft-web-recovery

Crash-recovery copies of unsaved documents for the Craft apps' web builds, kept in IndexedDB, and
the page hooks a web build needs around saving. It is the browser-storage store that #124 leaves
for later, with the same rules as the desktop stores:

- **One area per running app.** Every page load is a session holding a Web Lock named after it,
  so another tab never offers, adopts or deletes a running tab's copies. Copies of sessions that
  are gone are taken over at startup under a short exclusive lock, so two tabs opened together
  never both recover the same document.
- **Commit, then acknowledge.** `Store::put` returns at once; `Store::take_written` reports each
  write only after IndexedDB committed it (a full disk is reported there), so an app counts a
  revision as saved only when it is.
- **An entry stays until the app removes it** (saved or closed), never just because it was
  recovered. Damaged entries are deleted and reported.

```rust
// Before the app is built (so the restore prompt has everything at launch):
let opened = craft_web_recovery::open("photocraft").await;
for r in opened.recovered { /* reopen r.bytes as "<r.name> (Recovered)", keep r.key */ }
for w in opened.warnings { /* show w (Display) */ }
let store = opened.store;
store.set_waker(move || ctx.request_repaint());

// Every autosave interval, for each modified document:
store.put(&store.key(doc_id), revision, Snapshot { name, path, meta: None, bytes: &bytes })?;
// Each frame: record the revisions that really were written.
for w in store.take_written() { /* w.key, w.revision, w.result */ }
// Saved or closed:
store.remove(&key);
```

Also: `now_ms()` (a wall clock that works on `wasm32`, where `std::time::Instant` panics),
`UnloadGuard` (the browser's "Leave site?" while there are unsaved changes), `HiddenHook` (runs when
the tab is hidden, the last moment before a browser may discard it) and `download` (Save as a
download, in a way Safari accepts).

The app keeps its document format, the autosave timer and preferences, the restore prompt and the
"(Recovered)" title. Everything but `now_ms` and `Warning` exists only on `wasm32`.

## Tests

`cargo test -p craft-web-recovery` runs the selection rules on any target. The IndexedDB and Web
Locks tests run in a real browser:

```sh
cargo install wasm-bindgen-cli --version <the lockfile's wasm-bindgen> --locked
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner SAFARIDRIVER=/usr/bin/safaridriver \
  cargo test -p craft-web-recovery --target wasm32-unknown-unknown
```

(`CHROMEDRIVER` or `GECKODRIVER` instead of `SAFARIDRIVER` for Chrome or Firefox; Safari needs
Develop › Allow Remote Automation.)

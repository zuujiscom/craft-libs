//! Which stored copies a new session recovers. Pure, so it is tested on every target.

use std::fmt;

/// Something about recovery worth telling the user (the app shows it, translated if it likes).
#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// The browser has no usable IndexedDB (turned off, private mode in some browsers, a timeout):
    /// nothing is recovered and nothing will be kept.
    Unavailable(String),
    /// The stored copies could not be read; they stay stored for a later load.
    ReadFailed(String),
    /// `name` wasn't recovered because the copies together are larger than a page reads at once;
    /// it stays stored for a later load.
    TooLarge { name: String },
    /// `name`'s stored copy is damaged and was deleted.
    Damaged { name: String },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::Unavailable(e) => write!(f, "Crash recovery is unavailable in this browser: {e}"),
            Warning::ReadFailed(e) => write!(f, "Recovered documents could not be read: {e}"),
            Warning::TooLarge { name } => {
                write!(f, "{name} was not recovered: the recovered documents are larger than this page reads at once")
            }
            Warning::Damaged { name } => write!(f, "{name} was not recovered: its stored copy was damaged"),
        }
    }
}

/// One stored entry, as the startup scan sees it.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) struct Listed {
    pub name: String,
    pub session: String,
    pub saved_at: f64,
    /// Bytes of its copy; `None` when the copy is damaged.
    pub size: Option<f64>,
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Plan {
    /// Entries to recover (indices into the list), newest first.
    pub load: Vec<usize>,
    /// Entries whose copy is damaged, to delete.
    pub damaged: Vec<usize>,
    pub warnings: Vec<Warning>,
}

/// Pick the entries session `own` recovers: those of sessions that aren't running, newest first,
/// up to `max_bytes` in all. `running` lists the sessions open in other tabs; `None` means the
/// browser can't tell (no Web Locks), and then only `own`'s entries are left alone.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn plan(listed: &[Listed], own: &str, running: Option<&[String]>, max_bytes: f64) -> Plan {
    let mut out = Plan::default();
    let mut candidates: Vec<usize> = Vec::new();
    for (i, entry) in listed.iter().enumerate() {
        if entry.session == own || running.is_some_and(|r| r.contains(&entry.session)) {
            continue;
        }
        if entry.size.is_none() {
            out.damaged.push(i);
            out.warnings.push(Warning::Damaged { name: entry.name.clone() });
        } else {
            candidates.push(i);
        }
    }
    candidates.sort_by(|a, b| listed[*b].saved_at.total_cmp(&listed[*a].saved_at));
    let mut total = 0.0;
    for i in candidates {
        let size = listed[i].size.unwrap_or(0.0);
        if total + size <= max_bytes {
            total += size;
            out.load.push(i);
        } else {
            out.warnings.push(Warning::TooLarge { name: listed[i].name.clone() });
        }
    }
    out
}

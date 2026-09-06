//! Recently traced key values, remembered per Log Analytics workspace.
//!
//! Correlation ids are long, opaque and impossible to retype, so losing the
//! last few on restart makes the app markedly worse to use. Persistence is
//! best-effort throughout: a history that fails to save is a nuisance, never
//! an error worth interrupting a trace for.

use crate::services::az::Workspace;
use crate::services::trace::ErrorRule;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Enough to get back to what you were just looking at, few enough to stay a
/// single row of chips.
pub const MAX_ENTRIES: usize = 5;
/// Recently opened workspaces. Discovery needs `az login` and a scan of
/// every subscription, so remembering the last few is the difference
/// between reopening a workspace instantly and waiting for the list.
pub const MAX_WORKSPACES: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub value: String,
    /// The field it was traced on, so a chip can say what the value means.
    pub key: String,
}

#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
struct Store {
    /// Keyed by workspace GUID — unique, unlike the display name.
    #[serde(default)]
    workspaces: BTreeMap<String, Vec<Entry>>,
    /// Most recently opened first.
    #[serde(default)]
    recent_workspaces: Vec<Workspace>,
    /// What counts as a failed step, keyed by workspace GUID. This is
    /// domain knowledge the user taught the app, so it must outlive the
    /// session that taught it.
    #[serde(default)]
    error_rules: BTreeMap<String, Vec<ErrorRule>>,
}

fn dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ais-analytics")
}

/// Reads what is on disk. Safe without a lock because nothing is ever
/// written in place — see `write_at`.
fn read() -> Store {
    read_at(&dir())
}

fn read_at(dir: &std::path::Path) -> Store {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

const FILE: &str = "history.json";

/// Read, edit, write — as one operation, against one process at a time.
///
/// Two things made the plain version lossy. Every window and every second
/// instance ran its own read-modify-write, so two of them interleaving lost
/// one edit outright; and the write truncated the live file before writing,
/// so anything that died mid-write left a half-file that `read` turns into
/// `Store::default()` — silently discarding the error rules, which are the
/// one thing in here the user actually taught the app.
fn update<T>(edit: impl FnOnce(&mut Store) -> T) -> T {
    let _guard = FileGuard::acquire();
    let mut store = read();
    let out = edit(&mut store);
    write(&store);
    out
}

/// An exclusive lock held for one read-modify-write.
///
/// Best-effort by design: a lock we cannot take costs us the protection
/// against a concurrent instance, never the ability to save.
struct FileGuard(#[allow(dead_code)] Option<std::fs::File>);

impl FileGuard {
    fn acquire() -> FileGuard {
        let _ = std::fs::create_dir_all(dir());
        let Ok(file) = std::fs::File::create(dir().join("history.lock")) else {
            return FileGuard(None);
        };
        match file.lock() {
            Ok(()) => FileGuard(Some(file)),
            Err(_) => FileGuard(None),
        }
    }
}

/// Writes the whole store as one atomic replacement.
///
/// A temporary file plus a rename, rather than writing over the target:
/// `fs::write` truncates first, so a crash or a full disk mid-write leaves a
/// file that parses as nothing at all. The rename is atomic on every
/// platform this ships to, so a reader sees either the old file or the new
/// one and never a torn one.
fn write(store: &Store) {
    write_at(&dir(), store);
}

fn write_at(dir: &std::path::Path, store: &Store) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(FILE);
    let Ok(json) = serde_json::to_string_pretty(store) else {
        return;
    };
    // Per-process name: two instances staging at once must not share a file.
    let staged = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
    if std::fs::write(&staged, json).is_err() {
        let _ = std::fs::remove_file(&staged);
        return;
    }
    if std::fs::rename(&staged, &path).is_err() {
        let _ = std::fs::remove_file(&staged);
    }
}

pub fn load(workspace_id: &str) -> Vec<Entry> {
    read().workspaces.remove(workspace_id).unwrap_or_default()
}

/// Records a value as the most recent, returning the updated list.
pub fn record(workspace_id: &str, entry: Entry) -> Vec<Entry> {
    update(|store| {
        let list = store
            .workspaces
            .entry(workspace_id.to_string())
            .or_default();
        insert(list, entry);
        list.clone()
    })
}

pub fn clear(workspace_id: &str) -> Vec<Entry> {
    update(|store| {
        store.workspaces.remove(workspace_id);
    });
    Vec::new()
}

/// Most recent first, no duplicates, capped. Re-tracing a value moves it back
/// to the front rather than adding a second chip for it.
fn insert(list: &mut Vec<Entry>, entry: Entry) {
    list.retain(|e| e.value != entry.value);
    list.insert(0, entry);
    list.truncate(MAX_ENTRIES);
}

// ── Recently opened workspaces ────────────────────────────────────────────

pub fn load_workspaces() -> Vec<Workspace> {
    read().recent_workspaces
}

pub fn record_workspace(workspace: &Workspace) -> Vec<Workspace> {
    update(|store| {
        insert_workspace(&mut store.recent_workspaces, workspace.clone());
        store.recent_workspaces.clone()
    })
}

pub fn forget_workspace(workspace_id: &str) -> Vec<Workspace> {
    update(|store| {
        store
            .recent_workspaces
            .retain(|w| w.customer_id != workspace_id);
        store.recent_workspaces.clone()
    })
}

// ── Error rules ───────────────────────────────────────────────────────────

pub fn load_rules(workspace_id: &str) -> Vec<ErrorRule> {
    read().error_rules.remove(workspace_id).unwrap_or_default()
}

/// Replaces the whole set — the caller owns the list and edits it in place.
pub fn save_rules(workspace_id: &str, rules: &[ErrorRule]) {
    update(|store| {
        if rules.is_empty() {
            store.error_rules.remove(workspace_id);
        } else {
            store
                .error_rules
                .insert(workspace_id.to_string(), rules.to_vec());
        }
    });
}

/// Deduped by workspace GUID rather than name: two subscriptions can hold
/// workspaces with the same display name, and they are not the same
/// workspace.
fn insert_workspace(list: &mut Vec<Workspace>, workspace: Workspace) {
    list.retain(|w| w.customer_id != workspace.customer_id);
    list.insert(0, workspace);
    list.truncate(MAX_WORKSPACES);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: &str) -> Entry {
        Entry {
            value: value.into(),
            key: "OperationId".into(),
        }
    }

    #[test]
    fn most_recent_comes_first() {
        let mut list = Vec::new();
        insert(&mut list, entry("a"));
        insert(&mut list, entry("b"));
        assert_eq!(values(&list), vec!["b", "a"]);
    }

    #[test]
    fn retracing_a_value_moves_it_up_instead_of_duplicating() {
        let mut list = Vec::new();
        for v in ["a", "b", "c"] {
            insert(&mut list, entry(v));
        }
        insert(&mut list, entry("a"));
        assert_eq!(values(&list), vec!["a", "c", "b"]);
    }

    /// Re-tracing under a different key updates the annotation rather than
    /// leaving two chips that look identical.
    #[test]
    fn a_repeated_value_keeps_only_its_latest_key() {
        let mut list = vec![entry("a")];
        insert(
            &mut list,
            Entry {
                value: "a".into(),
                key: "TraceId".into(),
            },
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key, "TraceId");
    }

    #[test]
    fn the_oldest_entry_falls_off_the_end() {
        let mut list = Vec::new();
        for v in ["a", "b", "c", "d", "e", "f"] {
            insert(&mut list, entry(v));
        }
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(values(&list), vec!["f", "e", "d", "c", "b"]);
    }

    fn values(list: &[Entry]) -> Vec<&str> {
        list.iter().map(|e| e.value.as_str()).collect()
    }

    fn workspace(name: &str, id: &str) -> Workspace {
        Workspace {
            name: name.into(),
            resource_group: "rg".into(),
            customer_id: id.into(),
            subscription_id: "sub".into(),
        }
    }

    #[test]
    fn reopening_a_workspace_moves_it_to_the_front() {
        let mut list = Vec::new();
        for (n, id) in [("a", "ia"), ("b", "ib"), ("c", "ic")] {
            insert_workspace(&mut list, workspace(n, id));
        }
        insert_workspace(&mut list, workspace("a", "ia"));
        let names: Vec<&str> = list.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c", "b"]);
    }

    /// Two subscriptions can hold workspaces with the same display name;
    /// only the GUID identifies one.
    #[test]
    fn workspaces_are_deduped_by_id_not_name() {
        let mut list = Vec::new();
        insert_workspace(&mut list, workspace("shared", "id-one"));
        insert_workspace(&mut list, workspace("shared", "id-two"));
        assert_eq!(list.len(), 2);
    }

    /// The failure this replaced: `fs::write` truncates first, so anything
    /// dying mid-write left a file that parses as nothing — silently taking
    /// the error rules with it. A staged file plus a rename means a reader
    /// sees the old store or the new one, never a torn one.
    #[test]
    fn a_store_is_replaced_whole_and_leaves_nothing_staged() {
        let dir = std::env::temp_dir().join(format!("ais-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = Store::default();
        store
            .error_rules
            .insert("ws".into(), vec![rule("resultcode", "500")]);
        write_at(&dir, &store);

        // A second write must replace the first without ever leaving the
        // target absent or half-written.
        store.recent_workspaces.push(workspace("law", "ws"));
        write_at(&dir, &store);

        let back = read_at(&dir);
        assert_eq!(
            back.error_rules["ws"].len(),
            1,
            "rules survived the rewrite"
        );
        assert_eq!(back.recent_workspaces.len(), 1);

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging files left behind: {leftovers:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that predates the atomic write — or one truncated by anything
    /// else — must not be mistaken for a store.
    #[test]
    fn a_torn_file_reads_as_empty_rather_than_as_junk() {
        let dir = std::env::temp_dir().join(format!("ais-hist-torn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), r#"{"workspaces":{"ws":[{"value":"#).unwrap();

        assert_eq!(read_at(&dir), Store::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn rule(field: &str, value: &str) -> ErrorRule {
        ErrorRule {
            field: field.into(),
            display: field.into(),
            value: value.into(),
        }
    }

    #[test]
    fn the_workspace_list_is_capped() {
        let mut list = Vec::new();
        for i in 0..8 {
            insert_workspace(&mut list, workspace(&format!("a{i}"), &format!("i{i}")));
        }
        assert_eq!(list.len(), MAX_WORKSPACES);
        assert_eq!(list[0].name, "a7");
    }
}

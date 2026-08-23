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

#[derive(Default, Serialize, Deserialize)]
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

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ais-analytics")
        .join("history.json")
}

fn read() -> Store {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write(store: &Store) {
    let path = path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(store) {
        let _ = std::fs::write(path, json);
    }
}

pub fn load(workspace_id: &str) -> Vec<Entry> {
    read().workspaces.remove(workspace_id).unwrap_or_default()
}

/// Records a value as the most recent, returning the updated list.
pub fn record(workspace_id: &str, entry: Entry) -> Vec<Entry> {
    let mut store = read();
    let list = store.workspaces.entry(workspace_id.to_string()).or_default();
    insert(list, entry);
    let updated = list.clone();
    write(&store);
    updated
}

pub fn clear(workspace_id: &str) -> Vec<Entry> {
    let mut store = read();
    store.workspaces.remove(workspace_id);
    write(&store);
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
    let mut store = read();
    insert_workspace(&mut store.recent_workspaces, workspace.clone());
    let updated = store.recent_workspaces.clone();
    write(&store);
    updated
}

pub fn forget_workspace(workspace_id: &str) -> Vec<Workspace> {
    let mut store = read();
    store.recent_workspaces.retain(|w| w.customer_id != workspace_id);
    let updated = store.recent_workspaces.clone();
    write(&store);
    updated
}

// ── Error rules ───────────────────────────────────────────────────────────

pub fn load_rules(workspace_id: &str) -> Vec<ErrorRule> {
    read().error_rules.remove(workspace_id).unwrap_or_default()
}

/// Replaces the whole set — the caller owns the list and edits it in place.
pub fn save_rules(workspace_id: &str, rules: &[ErrorRule]) {
    let mut store = read();
    if rules.is_empty() {
        store.error_rules.remove(workspace_id);
    } else {
        store
            .error_rules
            .insert(workspace_id.to_string(), rules.to_vec());
    }
    write(&store);
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

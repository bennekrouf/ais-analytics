//! On-disk cache of the workspace scan.
//!
//! Scanning means reading the metadata endpoint, then sampling every table
//! that has data — seconds of waiting before the app can show anything.
//! Since the shape of a workspace changes rarely and the whole result is
//! derived data, it is cheap to keep the last scan and open on it
//! immediately while a fresh one runs behind.
//!
//! The cache is a convenience, never a source of truth: a corrupt or
//! unreadable file simply means a cold start, and a refresh always follows.

use crate::services::loganalytics::TimeRange;
use crate::services::schema::TableSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedScan {
    /// Unix seconds, so the UI can say how stale this is.
    pub scanned_at: i64,
    /// The window the scan sampled. A scan is only meaningful for the range
    /// it was taken over — the tables with data in the last hour are not the
    /// tables with data in the last month — so a cache taken over a
    /// different range is discarded rather than shown.
    #[serde(default)]
    pub range: TimeRange,
    pub schemas: Vec<TableSchema>,
}

/// Workspace ids are GUIDs, but never trust an id straight into a path:
/// flatten to something safe for a filename while staying recognisable when
/// someone looks in the directory.
fn slug(workspace_id: &str) -> String {
    workspace_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn path(workspace_id: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ais-analytics")
        .join("scans")
        .join(format!("{}.json", slug(workspace_id)))
}

pub fn load(workspace_id: &str, range: TimeRange) -> Option<CachedScan> {
    let text = std::fs::read_to_string(path(workspace_id)).ok()?;
    let scan: CachedScan = serde_json::from_str(&text).ok()?;
    // Opening on a scan taken over a different window would show tables as
    // empty (or absent) for reasons that have nothing to do with the data.
    (scan.range == range).then_some(scan)
}

pub fn save(workspace_id: &str, range: TimeRange, schemas: &[TableSchema]) {
    // An empty scan is indistinguishable from a failed one at read time, so
    // don't persist it — a cold start is better than a cache that says the
    // workspace is empty.
    if schemas.is_empty() {
        return;
    }
    let path = path(workspace_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let scan = CachedScan {
        scanned_at: chrono::Utc::now().timestamp(),
        range,
        schemas: schemas.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&scan) {
        let _ = std::fs::write(path, json);
    }
}

/// "just now", "5m ago", "3h ago" — enough to judge whether to trust it.
pub fn age(scanned_at: i64) -> String {
    let secs = (chrono::Utc::now().timestamp() - scanned_at).max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_ids_become_safe_recognisable_filenames() {
        assert_eq!(
            slug("11111111-2222-3333-4444-555555555555"),
            "11111111-2222-3333-4444-555555555555"
        );
        // A traversal attempt in an id must not escape the cache directory.
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        // Two different workspaces must never collide on one file.
        assert_ne!(slug("aaaa-1111"), slug("bbbb-2222"));
    }

    /// A cache taken over a different window describes a different set of
    /// tables, so it must not be shown as if it described this one.
    #[test]
    fn a_scan_from_another_time_range_is_not_reused() {
        let scan = CachedScan {
            scanned_at: 0,
            range: TimeRange::LastHour,
            schemas: vec![],
        };
        let text = serde_json::to_string(&scan).unwrap();
        let back: CachedScan = serde_json::from_str(&text).unwrap();
        assert_eq!(back.range, TimeRange::LastHour);
        assert_ne!(back.range, TimeRange::LastWeek);
    }

    #[test]
    fn ages_read_in_sensible_units() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(age(now), "just now");
        assert_eq!(age(now - 300), "5m ago");
        assert_eq!(age(now - 7200), "2h ago");
        assert_eq!(age(now - 172_800), "2d ago");
        // A clock that jumped backwards must not print a negative age.
        assert_eq!(age(now + 60), "just now");
    }
}

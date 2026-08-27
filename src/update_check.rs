//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each GitHub release and compares
//! the version field to this build's `CARGO_PKG_VERSION`. Designed to be
//! cheap and side-effect-free so it can run in the background at startup.

use serde::Deserialize;

/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/ais-analytics/latest/latest.json";
/// Where the user is sent to get the new version. The binaries are
/// distributed from mayorana.ch, not from GitHub.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    #[allow(dead_code)]
    pub latest_tag: String,
    pub release_url: String,
}

/// Returns `Some(UpdateInfo)` if a newer release is available, else `None`.
/// Any network / parse failure → `None`. Never panics.
/// Disabled if DISABLE_UPDATE_CHECK environment variable is set.
pub async fn check() -> Option<UpdateInfo> {
    if std::env::var("DISABLE_UPDATE_CHECK").is_ok() {
        return None;
    }

    let current = env!("CARGO_PKG_VERSION");
    let body = reqwest::Client::new()
        .get(LATEST_URL)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let latest: LatestJson = serde_json::from_str(&body).ok()?;
    if is_newer(&latest.version, current) {
        Some(UpdateInfo {
            latest_version: latest.version,
            latest_tag: latest.tag,
            release_url: RELEASES_URL.to_string(),
        })
    } else {
        None
    }
}

fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts
            .next()?
            .split(|c: char| c == '-' || c == '+')
            .next()?
            .parse()
            .ok()?;
        Some((major, minor, patch))
    };
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av > bv,
        _ => false,
    }
}

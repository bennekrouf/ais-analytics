//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each GitHub release and compares
//! the version field to this build's `CARGO_PKG_VERSION`. Designed to be
//! cheap and side-effect-free so it can run in the background at startup.

use serde::Deserialize;
use std::collections::HashMap;

/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/ais-analytics/latest/latest.json";
/// Fallback when `latest.json` has no entry for this OS (e.g. an Intel Mac —
/// only Apple Silicon is built). Sends the user to pick a build by hand
/// instead of at a link that would 404.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

/// Sent on the update check so the download logs can tell a new install
/// (a browser hitting the site) from an existing user updating. Also
/// carries the version, which is what makes per-version adoption
/// visible — the number that says how many people are still on a build
/// with a bug that is already fixed.
const USER_AGENT: &str = concat!("ais-analytics/", env!("CARGO_PKG_VERSION"), " (updater)");

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
    platforms: Platforms,
}

#[derive(Debug, Deserialize)]
struct Platforms {
    macos: HashMap<String, Artifact>,
    windows: HashMap<String, Artifact>,
    linux: HashMap<String, Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    url: String,
    #[allow(dead_code)]
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    #[allow(dead_code)]
    pub latest_tag: String,
    /// Direct link to this OS's build, so the banner's button downloads the
    /// binary itself rather than opening a landing page to pick one from.
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
        .header(reqwest::header::USER_AGENT, USER_AGENT)
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
            release_url: platform_url(&latest.platforms),
        })
    } else {
        None
    }
}

/// Picks the artifact URL published for this OS and architecture. Nothing
/// unambiguous for this machine falls back to the landing page.
fn platform_url(platforms: &Platforms) -> String {
    let by_os = match std::env::consts::OS {
        "macos" => &platforms.macos,
        "windows" => &platforms.windows,
        "linux" => &platforms.linux,
        _ => return RELEASES_URL.to_string(),
    };
    pick(by_os)
        .filter(|u| !u.is_empty())
        // Marks the hit as coming from an existing install. The banner opens
        // this in the user's browser, so the updater's own User-Agent is not
        // what fetches the file — without the marker the request is
        // indistinguishable from a first-time download off the website.
        // nginx serves the file regardless of the query string.
        .map(|u| format!("{u}?src=updater"))
        .unwrap_or_else(|| RELEASES_URL.to_string())
}

/// The artifact for this machine, out of what one OS publishes.
///
/// `HashMap::values().next()` was enough while every OS shipped exactly one
/// build, but it is iteration order, not a choice: the day a second
/// architecture is published it starts handing people a coin-flip between
/// them. So: name the architecture if any entry names it, take the only
/// entry if there is only one, and otherwise send the user to pick — a
/// landing page beats a download that will not run.
fn pick(by_os: &HashMap<String, Artifact>) -> Option<String> {
    let aliases: &[&str] = match std::env::consts::ARCH {
        "x86_64" => &["x86_64", "x86-64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        other => &[other],
    };
    let matches_arch = |key: &str, url: &str| {
        let key = key.to_lowercase();
        let url = url.to_lowercase();
        aliases.iter().any(|a| key.contains(a) || url.contains(a))
    };

    // Sorted so that, whatever we end up choosing, we choose it the same way
    // every run.
    let mut entries: Vec<(&String, &Artifact)> = by_os.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut for_this_arch = entries
        .iter()
        .filter(|(key, artifact)| matches_arch(key, &artifact.url));
    if let Some((_, artifact)) = for_this_arch.next() {
        return Some(artifact.url.clone());
    }
    match entries.as_slice() {
        [(_, artifact)] => Some(artifact.url.clone()),
        _ => None,
    }
}

fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av > bv,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(url: &str) -> Artifact {
        Artifact {
            url: url.into(),
            sha256: String::new(),
        }
    }

    fn map(entries: &[(&str, &str)]) -> HashMap<String, Artifact> {
        entries
            .iter()
            .map(|(k, u)| ((*k).to_string(), artifact(u)))
            .collect()
    }

    /// Today's `latest.json`: one build per OS, so there is nothing to choose
    /// between and the single entry wins whatever it is called.
    #[test]
    fn a_lone_artifact_is_taken_as_is() {
        let only = map(&[("tarball", "https://x/ais-analytics-linux-x86_64.tar.gz")]);
        assert_eq!(
            pick(&only).as_deref(),
            Some("https://x/ais-analytics-linux-x86_64.tar.gz")
        );
    }

    /// The bug this guards: `values().next()` on a `HashMap` is iteration
    /// order. With two architectures published it would hand roughly half of
    /// users a build that cannot run on their machine.
    #[test]
    fn a_second_architecture_does_not_become_a_coin_flip() {
        let both = map(&[
            ("arm64", "https://x/ais-analytics-macos-arm64.dmg"),
            ("x86_64", "https://x/ais-analytics-macos-x86_64.dmg"),
        ]);
        let chosen = pick(&both).expect("one of them matches this machine");
        let expected = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x86_64"
        };
        assert!(chosen.contains(expected), "got {chosen}");

        // And the same answer every time, not whatever the map yields first.
        for _ in 0..8 {
            assert_eq!(pick(&both), Some(chosen.clone()));
        }
    }

    /// Several builds and none of them ours: sending the user to a page they
    /// can choose from beats sending them to a binary that will not start.
    #[test]
    fn an_unrecognisable_set_sends_the_user_to_choose() {
        let neither = || {
            map(&[
                ("riscv64", "https://x/ais-analytics-riscv64.tar.gz"),
                ("ppc64le", "https://x/ais-analytics-ppc64le.tar.gz"),
            ])
        };
        assert_eq!(pick(&neither()), None);
        assert_eq!(
            platform_url(&Platforms {
                macos: neither(),
                windows: neither(),
                linux: neither(),
            }),
            RELEASES_URL
        );
    }

    /// An OS with nothing published for it at all.
    #[test]
    fn a_missing_build_is_not_a_broken_link() {
        assert_eq!(
            platform_url(&Platforms {
                macos: map(&[]),
                windows: map(&[]),
                linux: map(&[]),
            }),
            RELEASES_URL
        );
    }

    #[test]
    fn only_a_higher_version_counts_as_newer() {
        assert!(is_newer("0.1.26", "0.1.25"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.25", "0.1.25"));
        assert!(!is_newer("0.1.24", "0.1.25"));
        // Ten sorts after nine, which a string comparison would get wrong.
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(!is_newer("not a version", "0.1.25"));
    }
}

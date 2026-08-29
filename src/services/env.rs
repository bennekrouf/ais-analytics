//! Making the app's `PATH` look like your terminal's.
//!
//! An app launched from Finder, the Dock or a `.dmg` does not inherit the
//! shell environment. It gets roughly `/usr/bin:/bin:/usr/sbin:/sbin`, which
//! is why `az` reports as "not found on PATH" in the packaged build and works
//! perfectly under `cargo run` — the terminal passed its own `PATH` down.
//!
//! Homebrew installs `az` into `/opt/homebrew/bin`, which a bundle never sees.
//! So at startup we ask the login shell what it thinks `PATH` should be and
//! adopt it, falling back to the usual locations when that fails.
//!
//! A *non-interactive* login shell is used deliberately: it reads the profile
//! where `brew shellenv` lives, without the risk that an interactive shell
//! blocks on something and hangs startup before a window ever appears.

/// Directories worth having even if the shell tells us nothing.
const FALLBACKS: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// Adopts the login shell's `PATH`, merged with whatever we already have.
/// Call once, before anything runs a subprocess.
pub fn adopt_login_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    let login = login_shell_path();
    let cargo_bin = dirs::home_dir().map(|h| h.join(".cargo/bin").to_string_lossy().to_string());

    let mut extras: Vec<String> = FALLBACKS.iter().map(|s| s.to_string()).collect();
    if let Some(bin) = cargo_bin {
        extras.push(bin);
    }

    let merged = merge_paths(&current, login.as_deref(), &extras);
    // SAFETY: called first thing in main, before any other threads start.
    unsafe {
        std::env::set_var("PATH", merged);
    }
}

/// Login shell first (it is the informed answer), then what we already had,
/// then the fallbacks. Order is preserved and duplicates are dropped, so the
/// first place a tool is found stays the one that wins.
pub fn merge_paths(current: &str, login: Option<&str>, extras: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = vec![];

    let sources = [
        login.unwrap_or_default().to_string(),
        current.to_string(),
        extras.join(":"),
    ];

    for source in sources {
        for entry in source.split(':') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if seen.insert(entry.to_string()) {
                out.push(entry.to_string());
            }
        }
    }
    out.join(":")
}

#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    use std::process::{Command, Stdio};

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = Command::new(&shell)
        // -l reads the profile (where `brew shellenv` normally is); no -i, so
        // an interactive prompt can never block startup.
        .args(["-lc", "printf '%s' \"$PATH\""])
        .stdin(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(unix))]
fn login_shell_path() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extras() -> Vec<String> {
        vec!["/opt/homebrew/bin".to_string(), "/usr/bin".to_string()]
    }

    #[test]
    fn a_bundles_bare_path_gains_the_places_tools_actually_live() {
        // The exact failure: az is in /opt/homebrew/bin, the bundle sees neither.
        let merged = merge_paths("/usr/bin:/bin", None, &extras());
        assert!(merged.split(':').any(|p| p == "/opt/homebrew/bin"));
    }

    #[test]
    fn the_login_shell_is_believed_before_anything_else() {
        let merged = merge_paths("/usr/bin", Some("/opt/homebrew/bin:/usr/bin"), &extras());
        assert!(merged.starts_with("/opt/homebrew/bin"));
    }

    #[test]
    fn a_directory_never_appears_twice() {
        let merged = merge_paths(
            "/usr/bin:/bin",
            Some("/usr/bin:/opt/homebrew/bin"),
            &extras(),
        );
        let count = merged.split(':').filter(|p| *p == "/usr/bin").count();
        assert_eq!(count, 1, "got {merged}");
    }

    #[test]
    fn order_decides_which_copy_of_a_tool_wins() {
        let merged = merge_paths("/usr/bin", Some("/opt/homebrew/bin:/usr/bin"), &extras());
        let entries: Vec<&str> = merged.split(':').collect();
        let brew = entries
            .iter()
            .position(|p| *p == "/opt/homebrew/bin")
            .unwrap();
        let usr = entries.iter().position(|p| *p == "/usr/bin").unwrap();
        assert!(brew < usr);
    }

    #[test]
    fn empty_and_ragged_input_does_not_produce_empty_entries() {
        let merged = merge_paths("::/usr/bin: :", Some(""), &extras());
        assert!(!merged.split(':').any(|p| p.trim().is_empty()));
    }

    #[test]
    fn nothing_at_all_still_yields_a_usable_path() {
        let merged = merge_paths("", None, &extras());
        assert!(merged.split(':').any(|p| p == "/usr/bin"));
    }

    /// Not an assertion about this machine — just proof the probe returns
    /// something shaped like a PATH when a shell is available.
    #[test]
    fn the_login_shell_probe_returns_a_path_or_nothing() {
        if let Some(path) = login_shell_path() {
            assert!(path.contains('/'), "got {path}");
        }
    }
}
